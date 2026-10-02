"""Isolation without a fork, as one object (TID-123, step 2): what a test may have disturbed,
measured and put back.

A test run in-process — the ladder's no-fork tiers — is isolated by snapshotting the shared state
it could reach before the body and comparing (and, at the module boundary, restoring) afterwards.
Five snapshots were taken under three different conditions in `_child_exec`, folded into a purity
verdict by precedence rules written as nested `if`s, and held again as a five-key dict by
`_enter_module` / `_leave_module`. [`Isolation.before`] takes the snapshots a level needs,
[`Isolation.verdict`] is the one place the precedence lives, [`Isolation.restore`] the one place
they are put back. The snapshot and restore machinery below it moved here unchanged.
"""
from __future__ import annotations

import copy
import inspect
import logging
import os
import sys
import threading
import warnings
from dataclasses import dataclass, field
from typing import Any

from .results import UNKNOWN_PURITY as _UNKNOWN_PURITY
from .safe import MISSING as _MISSING, safe_getattr as _safe_getattr

_OPAQUE = object()


def _snapshot_shared(module) -> dict:
    """A deep snapshot of a module's mutable top-level state — the names a test could mutate to
    contaminate a batch-mate. Functions/classes/modules/dunders are excluded; values that can't be
    deep-copied are marked opaque and skipped (the differential gate is the soundness backstop)."""
    out = {}
    for k, v in list(vars(module).items()):
        if k.startswith("__") or callable(v) or isinstance(v, type) or inspect.ismodule(v):
            continue
        try:
            copied = copy.deepcopy(v)
        except Exception:  # noqa: BLE001
            out[k] = _OPAQUE  # un-copyable ⇒ the module forks; this must stay ahead of the rule below
            continue
        # A value that copies but compares by identity (no `__eq__`): its copy could never equal
        # the original, so every test in a module holding one was judged impure and the value was
        # rebound to a fresh copy after each. `from __future__ import annotations` binds one
        # (`annotations`, a `__future__._Feature`) in almost every module — 4,491 of pirn-core's
        # 4,499 impure verdicts were that one name (TID-77). Keep the object itself: the name is
        # unchanged while it still refers to it, and mutation *inside* it is what the fingerprint
        # already leaves to the differential gate.
        out[k] = v if type(v).__eq__ is object.__eq__ else copied
    return out


def _purity_verdict(module, before: dict, env_before: dict):
    """Compare the module's shared state + `os.environ` to the pre-body snapshot. Returns an impurity
    reason (a test that mutated shared state — NOT safe to batch) or `None` (pure — batchable)."""
    after = _snapshot_shared(module)
    for k in set(before) | set(after):
        b, a = before.get(k, _MISSING), after.get(k, _MISSING)
        if b is _OPAQUE or a is _OPAQUE:
            continue  # couldn't snapshot ⇒ can't judge; leave to the differential gate
        if b is _MISSING or a is _MISSING or b != a:
            return f"mutated module global `{k}`"
    if dict(os.environ) != env_before:
        return "mutated os.environ"
    return None


def _restore_in_place(live, old) -> bool:
    """Restore `live`'s CONTENTS from `old`, preserving its identity. True if it was handled (TID-22).

    Rebinding the module attribute instead — `d[k] = deepcopy(old)` — restores the *name* but not the
    *object*, so anything holding a direct reference to the original (a registered stub, a callback, a
    fixture that captured the sink, a class attribute) keeps writing into the old object while the
    module attribute points at a fresh copy. The two silently diverge, and the resulting failure
    surfaces arbitrarily far from the cause.

    A plain module-level function is unaffected either way — it resolves globals by name at call time.
    The bug needs something that captured the object itself, which is exactly what test doubles do."""
    if live is old or type(live) is not type(old):
        return False
    if isinstance(live, dict):
        live.clear()
        live.update(copy.deepcopy(old))
        return True
    if isinstance(live, list):
        live[:] = copy.deepcopy(old)
        return True
    if isinstance(live, set):
        live.clear()
        live.update(copy.deepcopy(old))
        return True
    if isinstance(live, bytearray):
        live[:] = old
        return True
    # A user object is the other common sink: a stub or recorder held by reference, whose attributes
    # the test mutates. Restore its attributes RECURSIVELY rather than replacing its `__dict__`
    # wholesale — the object's own attributes are frequently the very containers other globals alias,
    # and swapping them for copies breaks exactly the identity this function exists to preserve.
    inst = getattr(live, "__dict__", None)
    if isinstance(inst, dict):
        old_vars = vars(old)
        for name in set(inst) | set(old_vars):
            if name not in old_vars:
                del inst[name]
            elif name not in inst or not _restore_in_place(inst[name], old_vars[name]):
                inst[name] = copy.deepcopy(old_vars[name])
        return True
    slots = [s for cls in type(live).__mro__ for s in getattr(cls, "__slots__", ())]
    if slots:
        for name in slots:
            if not hasattr(old, name):
                if hasattr(live, name):
                    delattr(live, name)
            elif not hasattr(live, name) or not _restore_in_place(
                getattr(live, name), getattr(old, name)
            ):
                setattr(live, name, copy.deepcopy(getattr(old, name)))
        return True
    # Everything else — deque, array.array, numpy arrays, custom C containers — via the two shapes
    # that preserve identity (TID-23). Slice assignment is tried first because it is the closer to
    # atomic: `clear()` followed by a failing `extend()` would leave the container empty, which is
    # worse than either restoring it or rebinding it.
    #
    # Widening `_restorable` to force a fork for these instead is the other sound answer, and was
    # rejected: Windows has no fork, so `--no-fork` would turn a module-level numpy array — entirely
    # ordinary — into a hard error. `_restorable` stays the backstop for genuinely opaque values.
    try:
        live[:] = copy.deepcopy(old)
        return True
    except Exception:  # noqa: BLE001 — not a sliceable sequence; try the other shape
        pass
    try:
        restored = copy.deepcopy(old)
        live.clear()
        live.extend(restored)
        return True
    except Exception:  # noqa: BLE001 — not a clear/extend container either; rebinding is the fallback
        pass
    return False


def _restore_shared(module, before: dict, env_before: dict) -> None:
    """Undo a (bounded) test's mutations from the pre-body snapshot — fork-free isolation. Restores the
    module's snapshotted globals (re-setting changed ones, removing added ones) and `os.environ`. Sound
    only for the snapshotted footprint: a mutation through an opaque/unsnapshottable value can't be
    undone here, so such tests must still fork (see `_restorable`)."""
    current = _snapshot_shared(module)
    d = vars(module)
    for k in set(before) | set(current):
        old = before.get(k, _MISSING)
        if old is _OPAQUE or current.get(k, _MISSING) is _OPAQUE:
            continue  # can't safely restore an opaque value
        if old is _MISSING:
            d.pop(k, None)  # the test added this global → remove it
        elif d.get(k, _MISSING) != old:
            # Contents first, identity preserved (TID-22); rebinding is the fallback for immutables
            # (int/str/tuple), where identity cannot be observed through mutation anyway.
            if not _restore_in_place(d.get(k, _MISSING), old):
                d[k] = copy.deepcopy(old)
    if dict(os.environ) != env_before:
        os.environ.clear()
        os.environ.update(env_before)



def _registry_targets(cache: dict, module_key: str, roots: tuple) -> list:
    """The module-level containers worth watching for this test's module, found once. `cache` is
    the process's memo: module key -> (sys.modules size, [(name, module)] watched, their namespace
    sizes, targets).

    Finding them means walking every module of the watched packages and every name in it, which
    measured at 7ms — far more than the tests it wraps, and paid twice per test. The containers
    themselves are few (two, on one 4,500-test suite), so the scan is cached.

    The cache is valid while nothing that could add a container has happened (TID-68). Three things
    can: `sys.modules` grew (a new module imported); a watched module was **replaced** (removed and
    re-imported — TID-56's deletions do exactly that — so its containers are new objects and the
    cached ones are stale references); or a watched module's namespace **grew** (a test or fixture
    assigned `lib.REGISTRY = {}` onto a module that already existed). The first is one integer; the
    other two are one identity check and one `len` per watched module, which is the packages the
    test file imports rather than all of `sys.modules` — cheap enough to pay per test, which the
    full scan is not. The earlier cache keyed on the first alone and its docstring called that "the
    only way a new one can appear"; it was not."""
    size = len(sys.modules)
    cached = cache.get(module_key)
    if cached is not None and cached[0] == size:
        _, watched, sizes, targets = cached
        if (all(sys.modules.get(name) is module for name, module in watched)
                and tuple(len(vars(module)) for _, module in watched) == sizes):
            return targets
    watched: list = []
    targets: list = []
    if roots:
        for name, module in list(sys.modules.items()):
            if name.partition(".")[0] not in roots or module is None:
                continue
            namespace = getattr(module, "__dict__", None)
            if not namespace:
                continue
            watched.append((name, module))
            for attr, value in list(namespace.items()):
                # Exact types only: a subclass may define `__len__` arbitrarily, and a proxy object
                # can raise on access (TID-43's lazy proxies are exactly that shape).
                if attr.startswith("__") or type(value) not in (dict, list, set):
                    continue
                targets.append((f"{name}.{attr}", value))
    sizes = tuple(len(vars(module)) for _, module in watched)
    cache[module_key] = (size, watched, sizes, targets)
    return targets


def _registry_snapshot(cache: dict, module_key: str, roots: tuple) -> dict:
    """Shallow copies of the module-level containers in the packages this test's module imports.

    Copied rather than merely sized, because detection alone does not help the *neighbours*: the
    offender can be re-run in the clean room, but the worker it polluted keeps serving tests, and
    they would go on seeing a registry entry that a finished test added. A shallow copy is enough to
    put the container back — the entries themselves are the library's, not ours to duplicate.

    Restored **in place** (`clear` + refill), never rebound: other modules hold references to that
    exact dict, and swapping in a new one would leave them looking at the polluted original — the
    same reason `_restore_in_place` exists (TID-22)."""
    out: dict = {}
    for label, container in _registry_targets(cache, module_key, roots):
        out[label] = (container, copy.copy(container))
    return out


def _is_test_owned(value, test_module: str) -> bool:
    """Whether `value` was defined by the *test code* rather than by library code.

    This is the line between pollution and a warm cache, and shape cannot draw it: both are additions
    to a module-level dict. Origin can.

    * A class the test defines and registers — click's `add_completion_class(MyshComplete)`, where
      `MyshComplete` is declared inside the test function — belongs to that test. Its neighbours must
      not see it.
    * A handler a *library* registers for itself on first import — PIL putting a WEBP writer into
      `PIL.Image.SAVE` when `WebPImagePlugin` loads — is that library warming up. Removing it breaks
      the next test that wanted to save a WEBP, which is precisely what an earlier cut of this did.

    "Library" here means anything that is not a test file, **including the project's own modules**: a
    project that fills a plugin registry when one of its modules is imported is doing the same lazy
    registration PIL does, and the fact that the code lives in this repo changes nothing about it."""
    origin = _safe_getattr(value, "__module__", None) or _safe_getattr(type(value), "__module__", "")
    if not isinstance(origin, str) or not origin:
        return False
    if origin == test_module:
        return True  # defined in this very test module, function-local classes included
    module = sys.modules.get(origin)
    file = _safe_getattr(module, "__file__", None) if module is not None else None
    if not file:
        return False
    name = os.path.basename(file)
    # A conftest counts: a fixture registering something for its tests is still test-side setup.
    return name == "conftest.py" or name.startswith("test_") or name.endswith("_test.py")


def _registry_delta(before: dict, test_module: str) -> str | None:
    """What the suite added to another module's containers since `before`, without touching it —
    the per-test verdict's view; `_restore_registries` puts it back at the module boundary."""
    changed = []
    for key, (container, saved) in before.items():
        try:
            if container == saved:
                continue
            if isinstance(container, dict):
                added = any(k not in saved and _is_test_owned(container[k], test_module) for k in container)
            else:
                added = any(v not in saved and _is_test_owned(v, test_module) for v in container)
        except Exception:  # noqa: BLE001 — an uncooperative container is not a verdict
            continue
        if added:
            changed.append(key)
    if not changed:
        return None
    shown = ", ".join(sorted(changed)[:3])
    more = "" if len(changed) <= 3 else f" (+{len(changed) - 3} more)"
    return f"{shown}{more}"


def _restore_registries(before: dict, test_module: str) -> str | None:
    """Remove what the *suite* put into another module's containers; returns what it changed.

    Only the suite's own additions are pulled back out. A library's lazy self-registration stays,
    because the next test may well depend on it having happened.

    Edited in place, so every reference to that container sees the correction."""
    changed = []
    for key, (container, saved) in before.items():
        if container == saved:
            continue
        removed = False
        try:
            if isinstance(container, dict):
                added = [k for k in container if k not in saved and _is_test_owned(container[k], test_module)]
                for k in added:
                    del container[k]
                    removed = True
            elif isinstance(container, list):
                keep = [v for v in container if v in saved or not _is_test_owned(v, test_module)]
                if len(keep) != len(container):
                    container[:] = keep
                    removed = True
            else:
                for v in [v for v in container if v not in saved and _is_test_owned(v, test_module)]:
                    container.discard(v)
                    removed = True
        except Exception:  # noqa: BLE001 — an uncooperative container stays as it is
            pass
        if removed:
            changed.append(key)
    if not changed:
        return None
    shown = ", ".join(sorted(changed)[:3])
    more = "" if len(changed) <= 3 else f" (+{len(changed) - 3} more)"
    return f"mutated another module's state: {shown}{more}"


def _state_fingerprint() -> dict:
    """A cheap snapshot of the interpreter state a test could disturb (TID-33).

    Identities, keys and counts only — never a deep copy — so this is affordable to take around
    every in-process test. It is not trying to describe the world, only to notice that the world
    moved.

    The point is catching the categories we have NOT modelled. Restore covers the test module's
    globals, `os.environ` and `sys.modules`, and each of those was added after an incident; there is
    no reason to believe the list is finished. A fingerprint that shifts means the test reached
    somewhere we do not know how to undo, which is exactly when it should have forked.

    `sys.modules` is deliberately absent, even though it is the obvious thing to watch. It is
    already handled better upstream: `_restore_modules` runs before this comparison and puts back
    everything a test removed or replaced, so the only delta left to see here is modules the test
    *added* — and those are a warmed import cache that `_restore_modules` keeps on purpose. Flagging
    them would fork the test without undoing anything, since the import stays in the wellspring
    either way. Measured on a 4,514-test corpus that was 17 of the 18 trips, every one an ordinary
    lazy import."""
    root = logging.getLogger()
    return {
        "sys.path": list(sys.path),
        "environ": frozenset(os.environ),
        "warnings.filters": len(warnings.filters),
        "logging.handlers": tuple(id(h) for h in root.handlers),
        "logging.level": root.level,
        "threads": threading.active_count(),
        # The working directory is process-wide and every relative path in the next test resolves
        # against it, so a test that chdirs without cleaning up silently moves its neighbours'
        # footing. flask's suite does exactly that and nine of its tests then disagreed with pytest —
        # but only on the in-process tier, which is the signature of a leak the fingerprint is blind
        # to (TID-45). `getcwd` can raise if the directory was deleted underneath us, which is itself
        # a disturbance worth catching rather than a reason to crash the worker.
        "cwd": _safe_cwd(),
    }


def _safe_cwd() -> str | None:
    """`os.getcwd()`, or None if the directory has been removed underneath the process."""
    try:
        return os.getcwd()
    except OSError:
        return None


def _restore_state(before: dict) -> None:
    """Put back the interpreter state the fingerprint can restore, in place.

    The fingerprint exists to notice categories nobody modelled, but several of the things it
    watches are trivially restorable once you know they moved — so knowing is most of the work.
    `sys.path`, the warnings filters and the root logger's handlers are all just lists; they are
    restored by content so anything holding a reference keeps seeing the right object, for the same
    reason `_restore_in_place` exists (TID-22).

    What cannot be undone here is a thread the test left running. That is reported rather than
    fixed, and the node is still demoted to forking."""
    sys.path[:] = before["sys.path"]
    if len(warnings.filters) != before["warnings.filters"]:
        del warnings.filters[: len(warnings.filters) - before["warnings.filters"]]
    root = logging.getLogger()
    if tuple(id(h) for h in root.handlers) != before["logging.handlers"]:
        keep = {i: h for i, h in ((id(h), h) for h in root.handlers)}
        root.handlers[:] = [keep[i] for i in before["logging.handlers"] if i in keep]
    root.setLevel(before["logging.level"])
    # Cheap to put back and cheap to check, so the common case — a test that chdirs and forgets —
    # costs its neighbours nothing. A directory that no longer exists cannot be returned to; the
    # delta below still reports the move, and the node is demoted.
    if before.get("cwd") is not None and _safe_cwd() != before["cwd"]:
        try:
            os.chdir(before["cwd"])
        except OSError:
            pass


def _fingerprint_delta(before: dict, after: dict) -> str | None:
    """A human-readable description of what moved between two fingerprints, or None if nothing did.

    Named per key rather than reported as a bare "state changed", because the whole value of this
    signal is telling an author *what* their test touched."""
    # A thread *finishing* is not this test's doing — it is some earlier test's background worker
    # exiting, and reporting it as a leak both flags an innocent test and prints "left -1 threads".
    # Only growth is a disturbance. Every other key is compared for inequality in both directions:
    # a removed environment variable or warnings filter is as much a change as an added one.
    changed = [
        k for k in before
        if (after.get(k, 0) > before[k] if k == "threads" else before[k] != after.get(k))
    ]
    if not changed:
        return None
    parts = []
    for key in changed:
        if key == "threads":
            parts.append(f"left {after[key] - before[key]} thread(s) running")
        elif key == "cwd":
            parts.append(f"changed the working directory to {after[key]}")
        elif key == "environ":
            added = sorted(after[key] - before[key])
            removed = sorted(before[key] - after[key])
            detail = ", ".join(added + [f"-{r}" for r in removed][:3])
            parts.append(f"changed os.environ ({detail})")
        else:
            parts.append(f"changed {key}")
    return "; ".join(parts)


def _restore_modules(before: dict) -> list:
    """Put back any module a test REPLACED in `sys.modules`; returns the names it swapped (TID-27).

    `_snapshot_shared` covers one module's globals, so it cannot see a test that evicts a *library*
    module and re-imports it — which leaves two copies of every class that module defines. A test
    holding the original then sets state the library, now bound to the replacement, cannot see. The
    failure lands in an unrelated test with nothing pointing back at the cause.

    The snapshot is a **shallow** `dict(sys.modules)`: identities only, ~1600 references, so it costs
    microseconds rather than the deep copy `_snapshot_shared` pays. That is what makes covering the
    whole interpreter affordable here when snapshotting every module's *contents* would not be.

    Modules the test merely **added** are left alone. Those are a warmed import cache, not damage,
    and evicting them would only make the next test pay to import them again.

    A module the test **removed** is left removed, which is a different thing from one it replaced
    (TID-56). Suites purge a name on purpose — flask's conftest pops a module at teardown so the next
    test imports it fresh from that test's own temporary directory — and putting it back handed the
    next test a module built against the previous test's tmp dir, with nothing to re-import because
    the name was already bound. Restoring only names still bound to *something else* keeps TID-27's
    case (evict-and-reimport leaves a different object there, so the original still goes back) and
    honours the removal. The cost of honouring it is one import the author asked for."""
    replaced = []
    for name, module in before.items():
        current = sys.modules.get(name)
        if current is not None and current is not module:
            sys.modules[name] = module
            replaced.append(name)
    return replaced


def _restorable(module) -> bool:
    """Whether a module's shared state is fully snapshot/restorable (no opaque mutable globals). A test
    in a non-restorable module can't use the no-fork restore path — it must fork for isolation."""
    return _OPAQUE not in _snapshot_shared(module).values()




@dataclass
class Isolation:
    """The state around one in-process test, or one module's stay on a worker.

    `measure` snapshots the module's globals and `os.environ` — enough for a purity verdict (the
    guard, and the bare tier's recordability). `full` adds `sys.modules`, the interpreter-state
    fingerprint and the watched libraries' registries — what the restore tier puts back at the
    module boundary (TID-81) and what the fingerprint watches for the categories nobody modelled
    (TID-33)."""

    module_key: str
    roots: tuple
    module: Any
    globals_before: dict | None
    environ_before: dict | None
    modules_before: dict | None
    state_before: dict | None
    registries_before: dict | None
    registry_cache: dict = field(default_factory=dict)  # the process's memo of registry targets (TID-68)

    @classmethod
    def before(cls, module_key: str, module: Any, roots: tuple, *, measure: bool, full: bool,
               registry_cache: dict | None = None) -> "Isolation":
        """Take the snapshots the level needs. `module` may be `None` when nothing could be imported
        — then there is nothing to measure, and nothing to put back either. `registry_cache` is the
        process's memo of the containers worth watching, shared across every `Isolation` it takes."""
        measured = measure and module is not None
        cache = registry_cache if registry_cache is not None else {}
        return cls(
            module_key, roots, module,
            _snapshot_shared(module) if measured else None,
            dict(os.environ) if measured else None,
            dict(sys.modules) if full else None,
            _state_fingerprint() if full else None,
            _registry_snapshot(cache, module_key, roots) if full else None,
            cache,
        )

    @property
    def test_module(self) -> str:
        """The test module's import name — what a value defined by the test code reports as its
        `__module__`; empty when nothing was imported."""
        return getattr(self.module, "__name__", "") if self.module is not None else ""

    def verdict(self) -> tuple[Any, str | None]:
        """What the test did: `(purity, leaked)`. Purity is a reason (impure), `None` (measured
        pure) or `UNKNOWN_PURITY` (not measured); `leaked` names what no restore can undo — a
        thread left running — and means the result is not to be trusted and the node forks from
        now on (TID-33, TID-50). Everything here MEASURES; nothing restores: a file's tests run in
        one process in file order, and what one leaves behind is there for the next, as under
        pytest (TID-80). The restore happens when the worker leaves the module (`restore`)."""
        purity = (_purity_verdict(self.module, self.globals_before, self.environ_before)
                  if self.globals_before is not None else _UNKNOWN_PURITY)
        if self.modules_before is not None:
            # Tracked independently of the per-module snapshot: `sys.modules` is interpreter-global,
            # and a test can swap a library module without touching a single global of its own
            # (TID-27). Impure whatever the globals said: `_purity_verdict` cannot see this, and a
            # test recorded pure would later take the BARE no-fork tier, which skips the snapshot
            # entirely and would leave the swap in place for good (TID-1).
            replaced = [name for name, module in self.modules_before.items()
                        if sys.modules.get(name) is not None and sys.modules.get(name) is not module]
            if replaced:
                shown = ", ".join(sorted(replaced)[:3])
                more = f" (+{len(replaced) - 3} more)" if len(replaced) > 3 else ""
                purity = f"replaced modules in sys.modules: {shown}{more}"
        leaked = None
        if self.state_before is not None:
            after = _state_fingerprint()
            drift = _fingerprint_delta(self.state_before, after)
            registry_drift = _registry_delta(self.registries_before, self.test_module)
            if registry_drift is not None and purity is None:
                purity = f"mutated another module's state: {registry_drift}"
            if drift is not None:
                if purity is None:
                    purity = f"changed interpreter state: {drift}"
                # A thread left running is the one thing no restore can undo, at the boundary or
                # anywhere: it keeps executing in this process, and in every child forked from it.
                # Everything else the fingerprint watches is put back when the module is left, and
                # inside the module it is what pytest would show too.
                residue = _fingerprint_delta({k: v for k, v in self.state_before.items() if k == "threads"},
                                             {k: v for k, v in after.items() if k == "threads"})
                if residue is not None:
                    leaked = f"{drift} (unrestorable: {residue})"
                    purity = f"disturbed interpreter state: {leaked}"
        return purity, leaked

    def restore(self) -> None:
        """Put back what the snapshots hold: the next module on this worker starts from the state
        this one found, whatever its tests did in between (TID-81)."""
        if self.globals_before is not None:
            try:
                _restore_shared(self.module, self.globals_before, self.environ_before)
            except Exception:  # noqa: BLE001 — a global that will not restore must not take the worker
                pass
        if self.modules_before is not None:
            _restore_modules(self.modules_before)
        if self.registries_before is not None:
            # A container the library created *during* the module (TID-68) was not in the entry
            # snapshot; found now, it is restored against "empty", which pulls the suite's own
            # entries out and leaves the library's.
            registries = dict(self.registries_before)
            found = _registry_snapshot(self.registry_cache, self.module_key, self.roots)
            for label, (container, _) in found.items():
                if label not in registries:
                    try:
                        registries[label] = (container, type(container)())
                    except Exception:  # noqa: BLE001 — an exotic container stays as it is
                        pass
            _restore_registries(registries, self.test_module)
        if self.state_before is not None:
            _restore_state(self.state_before)
