"""Wellspring shim — the only Python the engine ships (no pytest *runner* underneath).

Imports the project ONCE (this process is the Wellspring), then drives a native, fork-based
fixture-execution engine (Phase 3, ADR-E003): wider-than-function fixtures (session/package/
module/class) are set up **once in this parent** as tests stream by, and a pristine copy-on-write
child is forked **per test** to set up function-scope fixtures and run the body. Wider-scope setup
cost is paid 1x and inherited by every child via COW; per-test isolation is free.

Protocol with the Rust orchestrator over stdin(0)/stdout(1): length-prefixed (u32 LE) JSON frames
(Phase 2 CONTRACT §3, unchanged).
  startup:   shim -> {"ready": true, "pid": int}
  request:   orchestrator -> {"node_id": str, "style": "function"|"class_method"|
                              "unittest_method", "deadline_ms": int}
  response:  shim -> {"node_id": str, "outcome": "passed|failed|skipped|error", "detail": str}

The fixture **definitions** are authored with `@pytest.fixture` (the corpus is also pytest's
differential oracle), so the engine reads pytest's fixture *marker* metadata — scope / params /
autouse — via `FixtureFunctionDefinition`. It does NOT use pytest's collection or runner: closure
resolution, nearest-override, scope layering, fork-from-warm, parametrization fan-out and yield
teardown are all implemented here. A future native `@tiderace.fixture` decorator would replace only
the marker read (ADR-E001).
"""
from __future__ import annotations

import ast
import asyncio
import copy
import difflib
import enum
import fnmatch
import functools
import hashlib
import importlib
import importlib.util
import inspect
import itertools
import json
import linecache
import logging
import os
import re
import select
import signal
import socket
import struct
import subprocess
import sys
import textwrap
import threading
import time
import traceback
import typing
import unittest
import warnings

from .config import NOTSET as _NOTSET, ProjectConfig, load_project_config, option as _argv_option
from .nodes import (Target, class_method as _class_method, import_module as _import_module,
                    module_key as _module_key, module_name as _module_name, resolve_target,
                    set_run_root)
from .results import (UNKNOWN_PURITY as _UNKNOWN_PURITY, Outcome, empty_expansion, errored,
                      expansion, purity_from, response, skipped, variant, with_purity)

_STDIN = 0
_STDOUT = 1

_SCOPE_RANK = {"function": 0, "class": 1, "module": 2, "package": 3, "session": 4}


# --------------------------------------------------------------------------- framing
def _read_exactly(fd: int, n: int) -> bytes | None:
    buf = b""
    while len(buf) < n:
        chunk = os.read(fd, n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


# The modules this run will execute, suite-relative (`tests/x/test_y.py`), or None for all of them
# (TID-75). Set from `--modules <file>` before anything is imported. `_preimport` and `_discover`
# import only these and the conftests above them: a run that executes one test used to pay the
# import of every test module in the suite — 4s on pirn-agents, which was the whole of a one-test
# run after an edit. A full run passes every module and is unchanged.
_SELECTED_MODULES: set | None = None
_SKIPPED_AT_DISCOVERY = 0  # test modules discovery did not import, for `TIDERACE_TIMING=1`


class _PhaseTimer:
    """Start-up phase timings to stderr under `TIDERACE_TIMING=1`; silent otherwise."""

    def __init__(self) -> None:
        self.on = _env_flag("TIDERACE_TIMING")
        self.last = time.perf_counter()

    def mark(self, label: str) -> None:
        if not self.on:
            return
        now = time.perf_counter()
        _warn(f"start-up: {label}: {now - self.last:.2f}s")
        self.last = now


def _select_modules(path: str) -> None:
    global _SELECTED_MODULES
    with open(path, encoding="utf-8") as fh:
        _SELECTED_MODULES = {line.strip() for line in fh if line.strip()}
    if _env_flag("TIDERACE_TIMING"):
        _warn(f"start-up: {len(_SELECTED_MODULES)} modules selected")


def _module_selected(rel: str) -> bool:
    """Whether this run executes tests from `rel`. Only test *modules* are ever skipped: every
    conftest in the tree is still imported, exactly as pytest imports every conftest at collection
    whatever it later deselects — a conftest can carry a side effect the rest of the suite relies
    on, and pruning the directories without selected modules cost 50 tests their isolation on
    pirn-agents before this was understood."""
    return _SELECTED_MODULES is None or rel in _SELECTED_MODULES


def _env(name: str, default: str | None = None) -> str | None:
    """A `TIDERACE_*` setting from the environment — the one place the shim reads it (TID-115)."""
    return os.environ.get(name, default)


def _env_flag(name: str) -> bool:
    """A `TIDERACE_*` switch: set to `1`."""
    return os.environ.get(name) == "1"


def _warn(message: str) -> None:
    """A line for the user on stderr, flushed — stdout is the protocol (TID-103)."""
    print(f"tiderace: {message}", file=sys.stderr, flush=True)


def _read_frame(fd: int) -> dict | None:
    header = _read_exactly(fd, 4)
    if header is None:
        return None
    (length,) = struct.unpack("<I", header)
    payload = _read_exactly(fd, length)
    if payload is None:
        return None
    return json.loads(payload.decode("utf-8"))


def _write_frame(fd: int, obj: dict) -> None:
    payload = json.dumps(obj).encode("utf-8")
    os.write(fd, struct.pack("<I", len(payload)) + payload)


def _read_exactly_by(fd: int, n: int, deadline_at: float) -> tuple:
    """`n` bytes from `fd` by `deadline_at` (monotonic): `(bytes, False)`, `(None, True)` on timeout,
    `(None, False)` on EOF."""
    buf = b""
    while len(buf) < n:
        remaining = deadline_at - time.monotonic()
        if remaining <= 0:
            return None, True
        ready, _, _ = select.select([fd], [], [], remaining)
        if not ready:
            return None, True
        chunk = os.read(fd, n - len(buf))
        if not chunk:
            return None, False
        buf += chunk
    return buf, False


def _read_frame_by(fd: int, deadline_at: float) -> tuple:
    """One frame's payload by `deadline_at`; same triple as `_read_exactly_by`."""
    header, timed_out = _read_exactly_by(fd, 4, deadline_at)
    if header is None:
        return None, timed_out
    (length,) = struct.unpack("<I", header)
    return _read_exactly_by(fd, length, deadline_at)


def _exit_text(status: int) -> str:
    """How a reaped process ended, for a diagnostic."""
    if os.WIFSIGNALED(status):
        return f"killed by signal {os.WTERMSIG(status)}"
    code = os.WEXITSTATUS(status) if os.WIFEXITED(status) else None
    if code == _EXIT_UNREPORTABLE:
        return "it ran the test but could not serialise its result frame"
    return f"exited {code}"


class _ModuleChild:
    """The forked process running one opaque module's tests (TID-80)."""

    __slots__ = ("module_key", "pid", "req_w", "resp_r")

    def __init__(self, module_key: str, pid: int, req_w: int, resp_r: int):
        self.module_key, self.pid, self.req_w, self.resp_r = module_key, pid, req_w, resp_r


# --------------------------------------------------------------------------- node ids
_ROOT = ""  # the run root (argv[1]); set by serve()/probe()/subinterp() before any import


def _skip_exceptions() -> tuple[type[BaseException], ...]:
    """Every exception type that means "skip this test", not "this test broke".

    `unittest.SkipTest` is the obvious one. `pytest.skip()` and
    `pytest.importorskip()` raise `_pytest.outcomes.Skipped`, which derives from
    `BaseException` rather than `SkipTest` — so without it here a skip falls
    through to the catch-all and is reported as an error. A suite that skips a
    test because an optional backend is absent then shows up as broken.
    """
    try:
        from _pytest.outcomes import Skipped
    except Exception:  # noqa: BLE001 — pytest absent ⇒ unittest skips only
        return (unittest.SkipTest,)
    return (unittest.SkipTest, Skipped)


_SKIP_EXCEPTIONS = _skip_exceptions()


def _package_basedir(directory: str) -> str:
    """The first ancestor of `directory` that is not itself a package.

    pytest's basedir rule, and the only directory that belongs on `sys.path`: a *package* directory
    placed there makes its own subpackages importable as bare top-level names."""
    cur = os.path.abspath(directory)
    while os.path.exists(os.path.join(cur, "__init__.py")):
        parent = os.path.dirname(cur)
        if parent == cur:  # filesystem root; nothing further to walk to
            break
        cur = parent
    return cur


def _insert_run_root(root: str) -> None:
    """Put the run root's **basedir** on `sys.path`, and warn if it still shadows the stdlib (TID-37).

    The run root goes on `sys.path[0]`, so any directory or module inside it named like a standard
    library module wins over the real one. A suite laid out as `tests/` containing `tests/types/` or
    `tests/statistics/` captures `types` / `statistics` for the whole run. Every module that touches
    one — directly or transitively — breaks, and it surfaces as ordinary test errors with nothing
    pointing at the cause. On one real suite that was 73 of them.

    It bites unevenly, which is what makes it so hard to see: a stdlib module already in
    `sys.modules` when the path is poisoned (`types`, `json`, `os`) keeps working, because nothing
    re-resolves it. Only names first imported *during* the run are captured, so the same defect
    looks like a batch-size or ordering effect rather than a naming one.

    Inserting the basedir instead of the root only works because `_discover` now names test modules
    through `_module_name`, which walks to the same place. While the two disagreed, the run root had
    to stay importable and this could not be fixed.

    The warning stays for the case the walk cannot help with: a basedir that is *not* a package but
    still holds a directory named like a stdlib module. pytest has the same exposure there, and
    naming it beats letting it surface as unrelated errors later."""
    basedir = _package_basedir(root)
    # First, not merely present (TID-48). A monorepo venv's editable-install `.pth` files already put
    # every package directory on `sys.path` — each holding its own `tests` package — so the basedir
    # is usually there, just behind a sibling. Leaving it where it was meant `tests.unit` resolved
    # against pirn-agents' `tests` while running pirn-core: 4,855 of 4,855 tests errored.
    # `python -m pytest` hides this, because `-m` puts the cwd ahead of every `.pth` entry.
    while basedir in sys.path:
        sys.path.remove(basedir)
    sys.path.insert(0, basedir)
    try:
        entries = os.listdir(basedir)
    except OSError:
        return
    names = {e[:-3] if e.endswith(".py") else e for e in entries}
    shadowed = sorted(names & sys.stdlib_module_names)
    if shadowed:
        _warn(
            f"{basedir} is on sys.path and contains "
            f"{', '.join(shadowed)}, which shadow standard-library modules of the same name; "
            f"imports of those will resolve here, not to the stdlib"
        )


def _test_dir(module_key: str) -> str:
    return os.path.dirname(module_key)


def _is_ancestor_dir(loc: str, test_dir: str) -> bool:
    """True if directory `loc` is `test_dir` or an ancestor of it (''=root, ancestor of all)."""
    if loc == "":
        return True
    if loc.startswith(".."):
        return True  # above the run root (TID-19) ⇒ ancestor of every test inside it
    return test_dir == loc or test_dir.startswith(loc + "/")


def _location_depth(loc: str) -> int:
    """How specific a conftest directory is — deeper wins in `Registry.resolve`.

    The run root is 0 and directories under it count their segments. A conftest ABOVE the run root
    (TID-19) is expressed as a `..`-relative path and scores NEGATIVE, one step per level up, so the
    total order stays `../.. < .. < run root < tests < tests/sessions`. That is what keeps a nearer
    conftest overriding a farther one in both directions."""
    if not loc:
        return 0
    depth = len(loc.split("/"))
    return -depth if loc.startswith("..") else depth


# --------------------------------------------------------------------------- fixture model
class FixtureDef:
    """A discovered fixture definition + the location it was declared at.

    `bindings` maps each of the function's parameter *names* to the *provider name* that satisfies it.
    For pytest-authored fixtures the two are identical (name-DI); for tiderace-native providers they may
    differ (the param is wired by **type**, ADR-E012), so callers must build kwargs from `bindings`,
    not from raw parameter names. `deps` (provider names — the registry keys the closure walks) is
    derived from the bindings."""

    __slots__ = (
        "name", "scope", "params", "autouse", "func", "location", "deps", "is_yield",
        "bindings", "provides_type", "param_ids", "owner",
    )

    def __init__(self, name, scope, params, autouse, func, location, bindings=None, provides_type=None,
                 param_ids=None, owner=None):
        self.name = name
        self.scope = scope if isinstance(scope, str) else "function"
        self.params = list(params) if params else None
        # `@pytest.fixture(params=[...], ids=[...])` — a list, or a callable applied per value.
        # Carried so a parametrized fixture's cases id the way pytest spells them (TID-25).
        self.param_ids = param_ids
        self.autouse = bool(autouse)
        self.func = func
        self.location = location  # module key ('tests/m.py') for module fixtures, or dir for conftest
        self.provides_type = provides_type  # native: the type this provider is injected by (else None)
        # The class a fixture method was defined on, or None. A fixture defined inside a test class
        # is scoped to that class in pytest — flask's `TestRoutes.app` overrides the conftest `app`
        # for that class and nowhere else — and it is called with the instance as `self` (TID-47).
        self.owner = owner
        if bindings is None:
            sig = list(inspect.signature(func).parameters)
            skip = {"request"} | ({"self", "cls"} if owner is not None else set())
            bindings = {p: p for p in sig if p not in skip}  # pytest/name-DI: identity
        self.bindings = bindings  # param_name -> provider_name
        self.deps = list(bindings.values())
        self.is_yield = inspect.isgeneratorfunction(func)

    @property
    def rank(self) -> int:
        return _SCOPE_RANK.get(self.scope, 0)

    @property
    def wants_request(self) -> bool:
        return "request" in inspect.signature(self.func).parameters


def _owner_args(fdef) -> tuple:
    """The positional `self` a class fixture is called with, or `()` for an ordinary one.

    A fixture defined inside a test class is a plain function until it is looked up on an instance, so
    it needs one. pytest binds it to the class's instance; a fresh one per setup matches what fixture
    bodies actually use it for — reaching the class's own helpers — without tying fixture setup to the
    instance the test body will later run on (TID-47)."""
    return (fdef.owner(),) if fdef.owner is not None else ()


def _run_finalizers(finalizers: list) -> None:
    """Run `request.addfinalizer` callbacks newest-first, as pytest does (TID-44).

    Guarded individually, matching `_teardown`: one finalizer raising must not stop the rest from
    releasing what they hold."""
    while finalizers:
        fn = finalizers.pop()
        try:
            fn()
        except Exception:  # noqa: BLE001 — a failing finalizer must not abort the remaining ones
            pass


_CURRENT_NODE = None  # the node the worker is running right now; fixtures and the test share it


def _node_for(node_id: str, func=None, instance=None):
    """The node object for `node_id`, reused for the whole test.

    One object, so a marker a *fixture* attaches and one the *test* attaches land in the same place —
    and so the executor can read both when folding runtime markers into the outcome."""
    global _CURRENT_NODE
    if _CURRENT_NODE is None or _CURRENT_NODE.nodeid != node_id:
        _CURRENT_NODE = _Node(node_id, func, instance)
    elif func is not None and _CURRENT_NODE.function is None:
        _CURRENT_NODE.function = func
    return _CURRENT_NODE


class _Node:
    """`request.node` — what pytest calls the item under test.

    Fixtures reach for it to name the thing they are building for (`request.node.name` in a temp-file
    or database-name prefix) and, less often, to attach a marker while the run is in flight. Both
    were `AttributeError` before: the fixture request had no `node` at all, and the test request had
    the node *id string* rather than an object (TID-51)."""

    __slots__ = ("nodeid", "name", "originalname", "cls", "function", "own_markers")

    def __init__(self, node_id: str, func=None, instance=None):
        self.nodeid = node_id
        # pytest's `name` is the last component, parametrize id included: `test_x[case]`.
        self.name = node_id.rpartition("::")[2] or node_id
        self.originalname = self.name.partition("[")[0]
        self.cls = type(instance) if instance is not None else None
        self.function = func
        self.own_markers: list = []

    def add_marker(self, marker) -> None:
        """Attach a marker mid-run, as pytest allows from a fixture or a test body.

        Recorded here and folded into the outcome by the executor below, because a marker added at
        runtime is usually an `xfail` the author expects to be honoured — ignoring it would turn a
        tolerated failure into a reported one."""
        self.own_markers.append(marker)

    def iter_markers(self, name: str | None = None):
        for m in self.own_markers:
            if name is None or getattr(m, "name", None) == name:
                yield m

    def get_closest_marker(self, name: str, default=None):
        return next(self.iter_markers(name), default)

    def __repr__(self) -> str:
        return f"<Node {self.nodeid}>"


def _runtime_outcome(node, outcome: str, detail: str) -> tuple:
    """Fold markers added during the run into the outcome (`xfail`, `skip`).

    Applied here rather than beside the static marks because a marker added at runtime exists only in
    the process that ran the test."""
    if node is None or not node.own_markers:
        return outcome, detail
    return _fold_pytest_marks(node.own_markers, outcome, detail)


def _fold_pytest_marks(markers, outcome: str, detail: str) -> tuple:
    """Fold pytest-style `xfail` / `skip` markers into an outcome.

    Shared by the static path and the runtime one, because `@pytest.mark.xfail` written above a test
    and `request.node.add_marker(pytest.mark.xfail(...))` added during it mean exactly the same thing
    and must land on the same outcome. Reporting a plain failure for either turns a failure the
    author expected into one the run complains about (TID-63)."""
    for m in markers:
        name = getattr(m, "name", "")
        kwargs = getattr(m, "kwargs", None) or {}
        reason = kwargs.get("reason") or ""
        if name == "skip":
            return "skipped", reason or "skipped at runtime"
        if name == "xfail":
            # `@pytest.mark.xfail(sys.platform == "win32", reason=...)` puts the condition first
            # positionally; the bare form has none and always applies. A string condition is left
            # unevaluated and treated as applying, matching `_marker_skip_reason`'s caution in the
            # other direction: an xfail that does not fire only ever reports a real failure.
            args = getattr(m, "args", ()) or ()
            condition = args[0] if args and not isinstance(args[0], str) else kwargs.get("condition", True)
            if condition is False:
                continue
            if outcome in ("failed", "error"):
                return "xfail", reason or detail
            if outcome == "passed":
                return ("failed", f"[xpass strict] {reason}".strip()) if kwargs.get("strict") \
                    else ("xpass", reason)
    return outcome, detail


class _Request:
    """The minimal `request` object a fixture sees: `.param` and `.node`, plus `addfinalizer`."""

    __slots__ = ("param", "node", "_finalizers")

    def __init__(self, param, node=None):
        self.param = param
        self.node = node
        self._finalizers: list = []

    def addfinalizer(self, fn) -> None:
        """Run `fn` when this fixture tears down (TID-44).

        Tied to the fixture's own teardown handle, so a session-scoped fixture's finalizer runs at
        session end rather than after the first test. flask's fixtures use this for app and
        request-context cleanup; without it they raised `AttributeError` during setup."""
        self._finalizers.append(fn)


class _FinalizingHandle:
    """A fixture teardown handle plus the finalizers its body registered (TID-44).

    Only built when a fixture actually called `addfinalizer`, so every fixture that did not keeps the
    exact handle it always had. pytest registers a yield fixture's own teardown *after* the body
    returns, which makes it the newest finalizer: the yield teardown runs first, then the body's
    `addfinalizer` callbacks, newest first."""

    __slots__ = ("inner", "finalizers")

    def __init__(self, inner, finalizers: list):
        self.inner = inner
        self.finalizers = finalizers


# Command-line options declared by conftests via `pytest_addoption`, as `dest -> default` (TID-14).
# Only defaults live here: tiderace has no way to *pass* a custom flag yet (that is TID-17), so a
# declared option always reads as its default — which is exactly what an opt-in guard like
# `if not request.config.getoption("--real"): pytest.skip(...)` needs to resolve correctly.
_CLI_OPTIONS: dict[str, object] = {}
# `parser.addini(name, help, type, default)` declarations, as `name -> (type, default)` (TID-87). A
# value the project's config sets wins over the declared default; `getini` of a name nobody
# declared is `None`, as before.
_INI_DECLARED: dict[str, tuple] = {}
_PROJECT: ProjectConfig | None = None  # the project's config, loaded once by `_discover` (TID-121)


def _ini_value(name: str):
    """`config.getini(name)`: the project's configured value if set, else the declared default,
    else `None`. Typed the way pytest types it — `bool` parses, list types split."""
    declared = _INI_DECLARED.get(name)
    ini_type = declared[0] if declared else None
    values = _PROJECT.values(name) if _PROJECT is not None else []
    if values:
        if ini_type == "bool":
            text = str(values[0]).strip().lower()
            return text in ("1", "true", "yes", "on")
        if ini_type in ("linelist", "args", "paths", "pathlist"):
            return [str(v) for v in values]
        return values[0] if len(values) == 1 else values
    if declared is None:
        return None
    default = declared[1]
    if default is not _NOTSET:
        return default
    return {"bool": False, "linelist": [], "args": [], "paths": [], "pathlist": []}.get(ini_type, "")


class _OptionRecorder:
    """Stands in for pytest's argument parser while a conftest's `pytest_addoption` hook runs.

    The hook expects to be handed a parser and to call `addoption` on it (or on a group). Rather
    than model argparse, record just what `getoption` needs: the destination name and the default
    the option would have carried."""

    def __init__(self, options: dict):
        self._options = options

    def addoption(self, *names, **kw) -> None:
        dest = kw.get("dest")
        if dest is None:
            flag = next((n for n in names if n.startswith("--")), names[0] if names else None)
            if flag is None:
                return
            dest = flag.lstrip("-").replace("-", "_")
        if "default" in kw:
            default = kw["default"]
        else:  # mirror argparse's implicit defaults for the actions conftests actually use
            action = kw.get("action")
            default = {"store_true": False, "store_false": True, "count": 0, "append": []}.get(action)
        self._options[dest] = default

    _addoption = addoption  # the private spelling plugins use on a group (xdist)

    def getgroup(self, *_a, **_kw):
        return self  # groups expose the same `addoption`, so the recorder can be its own group

    def addini(self, name, help=None, type=None, default=_NOTSET, **_kw) -> None:  # noqa: A002
        _INI_DECLARED[name] = (type, default)  # read back through `config.getini` (TID-87)


def _collect_addoption(module) -> None:
    """Run a conftest's `pytest_addoption` hook against the recorder, if it has one."""
    hook = getattr(module, "pytest_addoption", None)
    if hook is None:
        return
    try:
        hook(_OptionRecorder(_CLI_OPTIONS))
    except Exception as exc:  # noqa: BLE001 — a hook we can't model must not abort discovery
        _warn(f"pytest_addoption in {getattr(module, '__file__', '?')} "
              f"could not be recorded: {exc!r}")


class _Config:
    """The slice of pytest's `config` that tests reach for through `request.config`."""

    __slots__ = ()

    def getoption(self, name: str, default=_NOTSET, skip: bool = False):
        key = name.lstrip("-").replace("-", "_")
        if key in _CLI_OPTIONS:
            value = _CLI_OPTIONS[key]
        elif default is not _NOTSET:
            value = default
        else:
            # pytest raises for an option nobody declared; matching that beats inventing a value,
            # which would silently flip an opt-in guard the wrong way.
            raise ValueError(f"no option named {name!r}")
        if skip and value is None:
            raise _SKIP_EXCEPTIONS[0](f"no value for option {name!r}")
        return value

    def getini(self, name: str):
        return _ini_value(name)


# Node ids a collection hook (or a direct `@pytest.mark.skip`) decided to skip, as `node_id -> reason`
# (TID-20). Computed once during discovery, consulted per node in `Engine.run`.
_MARKER_SKIPS: dict[str, str] = {}


def _own_markers(*owners) -> list:
    """The `@pytest.mark.*` marks on a chain of owners, widest first (module → class → function).

    pytest stores them as a `pytestmark` list on whatever they decorate, so gathering them is just
    reading that attribute at each level. `__tiderace_marks__` is the native analogue and is read
    separately by `_marks`; this is the pytest-compat side."""
    out = []
    for owner in owners:
        if owner is None:
            continue
        marks = _safe_getattr(owner, "pytestmark", None)  # owners can carry a raising metaclass
        if not marks:
            continue
        # pytest accepts both spellings — `pytestmark = pytest.mark.slow` and
        # `pytestmark = [pytest.mark.slow, ...]` — and a bare `MarkDecorator` is not iterable, so
        # extending on it raises `TypeError` and takes the whole discovery pass down with it.
        out.extend(marks if isinstance(marks, (list, tuple)) else [marks])
    return out


class _HookItem:
    """The `item` a `pytest_collection_modifyitems` hook is handed (TID-20).

    Only the surface real conftests use: `nodeid` / `name` to identify it, `keywords` and
    `own_markers` / `iter_markers` / `get_closest_marker` to inspect it, and `add_marker` to change
    it. The overwhelmingly common shape — the one this ticket was filed for — is

        for item in items:
            if "needs_kuzu" in item.keywords:
                item.add_marker(pytest.mark.skip(reason="pass --real to run Kuzu tests"))

    which needs exactly `keywords` and `add_marker`."""

    __slots__ = ("nodeid", "name", "own_markers", "keywords")

    def __init__(self, nodeid: str, name: str, markers: list):
        self.nodeid = nodeid
        self.name = name
        self.own_markers = list(markers)
        # pytest's `keywords` is a mapping that answers `in` for mark names, the node name, and the
        # module. Membership is what conftests actually use it for.
        self.keywords = {getattr(m, "name", str(m)): m for m in self.own_markers}
        self.keywords[name] = True
        self.keywords[nodeid] = True

    def add_marker(self, marker, append: bool = True) -> None:
        if append:
            self.own_markers.append(marker)
        else:
            self.own_markers.insert(0, marker)
        self.keywords[getattr(marker, "name", str(marker))] = marker

    def iter_markers(self, name: str | None = None):
        for m in reversed(self.own_markers):
            if name is None or getattr(m, "name", None) == name:
                yield m

    def get_closest_marker(self, name: str, default=None):
        return next(self.iter_markers(name), default)

    def __repr__(self) -> str:  # a hook that logs its items should print something useful
        return f"<Item {self.nodeid}>"


def _marker_skip_reason(markers: list):
    """The skip reason implied by a pytest marker set, or None.

    `skipif`'s condition may be a bool or a string expression; only the bool form is evaluated. A
    string condition is treated as *not* skipping, because guessing at an unevaluated expression
    could silently skip a test that should have run — the failure that cannot be seen."""
    for m in reversed(markers):
        name = getattr(m, "name", None)
        if name not in ("skip", "skipif"):
            continue
        kwargs = getattr(m, "kwargs", {}) or {}
        args = getattr(m, "args", ()) or ()
        if name == "skipif":
            condition = args[0] if args else kwargs.get("condition")
            if not isinstance(condition, bool) or not condition:
                continue
            return kwargs.get("reason") or "skipif"
        return kwargs.get("reason") or (args[0] if args and isinstance(args[0], str) else "skip")
    return None


def _enumerate_items(test_modules: list) -> list:
    """The collected items, for the collection hooks to inspect (TID-20).

    Mirrors `RegexCollector`'s rules by **introspection** rather than by re-scanning source: module
    functions named `test*`, and methods named `test*` on `unittest.TestCase` subclasses (any name)
    or `Test*` classes.

    The Rust collector remains authoritative for what actually *runs*; this list only feeds the
    hooks. So a divergence can cost a skip that should have been applied, never a test that should
    not have run."""
    items = []
    for module, rel in test_modules:
        module_marks = _own_markers(module)
        for name, obj in vars(module).items():
            if name.startswith("test") and inspect.isfunction(obj):
                items.append(_HookItem(f"{rel}::{name}", name, module_marks + _own_markers(obj)))
            elif inspect.isclass(obj) and (
                name.startswith("Test") or issubclass(obj, unittest.TestCase)
            ):
                class_marks = module_marks + _own_markers(obj)
                for mname, meth in vars(obj).items():
                    if mname.startswith("test") and callable(meth):
                        items.append(
                            _HookItem(
                                f"{rel}::{name}::{mname}",
                                mname,
                                class_marks + _own_markers(meth),
                            )
                        )
    return items


def _run_collection_hooks(conftests: list, test_modules: list) -> None:
    """Run every conftest's `pytest_collection_modifyitems`, then record the skips it produced.

    Suites gate optional backends here — `needs_postgres`, `needs_kuzu` — so without it those tests
    run anyway and die on a missing import. pytest reports them as skips; tiderace reported a red run
    for a dependency the suite deliberately made optional.

    Marks applied *directly* (`@pytest.mark.skip`) are folded into the same pass, so there is one
    place that decides a marker-driven skip rather than two that can disagree."""
    hooks = [
        (m, getattr(m, "pytest_collection_modifyitems", None))
        for m in conftests
    ]
    hooks = [(m, h) for m, h in hooks if h is not None]

    items = _enumerate_items(test_modules)
    config = _Config()
    for module, hook in hooks:
        try:
            hook(config=config, items=items)
        except TypeError:
            # Hooks may declare any subset of (session, config, items) — pytest matches by name.
            try:
                hook(config, items)
            except Exception as exc:  # noqa: BLE001
                _warn_hook_failed(module, exc)
        except Exception as exc:  # noqa: BLE001 — a hook we can't run must not abort discovery
            _warn_hook_failed(module, exc)

    for item in items:
        reason = _marker_skip_reason(item.own_markers)
        if reason is not None:
            _MARKER_SKIPS[item.nodeid] = reason


def _warn_hook_failed(module, exc: BaseException) -> None:
    _warn(f"pytest_collection_modifyitems in "
          f"{getattr(module, '__file__', '?')} failed: {exc!r} — its skips will not be applied")


class _TestRequest:
    """The `request` a TEST function sees — pytest's `FixtureRequest`, minus the fixture plumbing.

    Distinct from `_Request` (what a *parametrized fixture* sees, which is only `.param`). A test
    asking for `request` overwhelmingly wants `request.config.getoption(...)` to decide whether to
    run, so `config` is the part that has to be real; the identity attributes are cheap and come
    along for free."""

    __slots__ = ("config", "node", "function", "cls", "instance", "param", "fixturenames", "_finalizers")

    def __init__(self, node_id: str, func, instance=None):
        self.config = _Config()
        self.node = _node_for(node_id, func, instance)
        self.function = func
        self.instance = instance
        self.cls = type(instance) if instance is not None else None
        self.param = None  # only a parametrized *fixture* has one; a test's request never does
        self.fixturenames = [p for p in inspect.signature(func).parameters
                             if p not in ("self", "cls")]
        self._finalizers: list = []

    def addfinalizer(self, fn) -> None:
        """Run `fn` after this test's body, before its function-scoped fixtures tear down (TID-44)."""
        self._finalizers.append(fn)


def _with_request(func, args: dict, node_id: str, instance=None) -> tuple:
    """Add a `request` argument when the test asks for one (TID-14).

    `_bind_by_type` deliberately skips the name `request`, so it never resolves as a provider and
    the test was simply called without it — a `TypeError` about a missing positional argument. It
    is injected here instead of registered as a provider because it needs the node context that
    only the call site has."""
    if "request" in args or "request" not in inspect.signature(func).parameters:
        return args, None
    request = _TestRequest(node_id, func, instance)
    return {**args, "request": request}, request


def _test_finalizers(request) -> None:
    """A test request's finalizers — after the body has fully run, including an awaited one."""
    if request is not None:
        _run_finalizers(request._finalizers)


_MISSING = object()


def _safe_getattr(obj, name: str, default=None):
    """`getattr` that treats *any* exception as "absent", not only `AttributeError` (TID-43).

    Discovery probes every module-level value to see whether it is a fixture or a provider, and those
    values are arbitrary objects. Plenty of them raise something other than `AttributeError` when
    touched: flask's `request`, `g`, `session` and `current_app` are werkzeug `LocalProxy` objects
    that raise `RuntimeError: Working outside of request context`; Django's `SimpleLazyObject` can
    raise whatever its factory raises; a mock can have a side effect on attribute access.

    Plain `hasattr` and `getattr(obj, name, default)` only swallow `AttributeError`, so any of those
    escaped `_discover` and killed the shim before it was ready — which on flask made the default
    configuration hang forever (see `WellspringPool::launch`). pytest's discovery goes through
    `_pytest.compat.safe_getattr` for precisely this reason; this is the same contract.

    `BaseException` is deliberately *not* caught: `KeyboardInterrupt` and `SystemExit` from a probe
    mean the user or the interpreter wants out, and swallowing them would be its own bug."""
    try:
        return getattr(obj, name, default)
    except Exception:  # noqa: BLE001 — see the docstring; this is the point
        return default


def _safe_hasattr(obj, name: str) -> bool:
    return _safe_getattr(obj, name, _MISSING) is not _MISSING


def _fixture_marker(obj):
    """The `FixtureFunctionMarker` for a `@pytest.fixture`, on any pytest version, or None.

    pytest moved it in 8.4, and the old location is the only one many real suites have (TID-44):

    | pytest  | `@pytest.fixture` returns     | marker                     | real function            |
    | ------- | ----------------------------- | -------------------------- | ------------------------ |
    | < 8.4   | the function, wrapped         | `_pytestfixturefunction`   | `__pytest_wrapped__.obj` |
    | >= 8.4  | a `FixtureFunctionDefinition` | `_fixture_function_marker` | `_fixture_function`      |

    Only the new names were recognised, so on any suite pinning an older pytest *every* fixture was
    invisible and every test requesting one failed with a missing positional argument. click (pytest
    7.4) and flask (8.1) lost 363 and 387 tests that way. Nothing caught it because every corpus the
    engine had been validated against happened to run pytest 9.

    The marker itself is the same `FixtureFunctionMarker` with the same fields on both sides of the
    move, so only *finding* it differs."""
    marker = _safe_getattr(obj, "_fixture_function_marker", None)  # pytest >= 8.4
    if marker is None:
        marker = _safe_getattr(obj, "_pytestfixturefunction", None)  # pytest < 8.4
    return marker


def _fixture_function(obj):
    """The callable a fixture actually runs, on any pytest version, or None.

    Never the decorated object itself: on every pytest version that is a wrapper whose job is to
    raise `Failed: Fixture "x" called directly`. `Failed` derives from `BaseException`, so a caller
    catching `Exception` would not even see it happen."""
    func = _safe_getattr(obj, "_fixture_function", None)  # pytest >= 8.4
    if func is None:
        wrapped = _safe_getattr(obj, "__pytest_wrapped__", None)  # pytest < 8.4
        func = _safe_getattr(wrapped, "obj", None) if wrapped is not None else None
    return func


def _is_fixture(obj) -> bool:
    return _fixture_marker(obj) is not None and _fixture_function(obj) is not None


def _is_native_provider(obj) -> bool:
    """A tiderace-native provider (ADR-E012) — carries the tiderace-owned marker, not pytest's."""
    return _safe_hasattr(obj, "__tiderace_provider__")


def _safe_type_hints(func) -> dict:
    try:
        return typing.get_type_hints(func, include_extras=True)
    except Exception:  # noqa: BLE001 — an unresolved annotation ⇒ treat as untyped (name fallback)
        return {}


def _provider_for_type(annotation, type_index: dict):
    """The single provider name registered for `annotation`'s type, or None (0 or >1 ⇒ name fallback).
    `Annotated[T, "name"]` disambiguates. Strict ambiguity errors are the `tiderace` package's job at
    author time; the shim stays lenient so mixed/compat suites keep running."""
    key, want = annotation, None
    if typing.get_origin(annotation) is typing.Annotated:
        key, *meta = typing.get_args(annotation)
        want = next((m for m in meta if isinstance(m, str)), None)
    candidates = list(type_index.get(key, ()))
    if want is not None:
        candidates = [c for c in candidates if c == want]
    return candidates[0] if len(candidates) == 1 else None


def _bind_by_type(func, type_index: dict) -> dict:
    """`param_name -> provider_name`, wired by TYPE (ADR-E012). Falls back to the param *name* when the
    parameter is untyped or its type has no unique provider — which makes pytest-authored suites
    (untyped fixture args, empty type index) resolve exactly as before."""
    hints = _safe_type_hints(func)
    out = {}
    for pname in inspect.signature(func).parameters:
        if pname in ("self", "cls", "request"):
            continue
        annotation = hints.get(pname)
        provider = _provider_for_type(annotation, type_index) if annotation is not None else None
        out[pname] = provider if provider is not None else pname
    return out


def _native_fixture_def(obj, location: str, type_index: dict) -> FixtureDef:
    spec = obj.__tiderace_provider__
    return FixtureDef(
        name=spec.name,
        # B5: provider-level params fan the provider out (read via `request.param`); `()` ⇒ unparametrized.
        params=list(spec.params) if getattr(spec, "params", ()) else None,
        scope=spec.scope,
        autouse=spec.autouse,
        func=obj,
        location=location,
        bindings=_bind_by_type(obj, type_index),  # provider→provider deps, by type
        provides_type=spec.provides,
    )


def _fixture_def(obj, location: str, owner=None, attr_name: str | None = None) -> FixtureDef:
    # Both accessors handle pytest before and after 8.4 (TID-44); see `_fixture_marker`.
    marker = _fixture_marker(obj)
    func = _fixture_function(obj)
    # pytest names a fixture by `name=` when given, else by the **attribute** it is bound to in its
    # module or class — not by the function's `__name__`. `mocker = pytest.fixture()(_mocker)` and
    # its four scope-siblings are five fixtures wrapping one function (TID-87).
    return FixtureDef(
        name=getattr(marker, "name", None) or attr_name or func.__name__,
        scope=getattr(marker, "scope", "function"),
        params=getattr(marker, "params", None),
        autouse=getattr(marker, "autouse", False),
        func=func,
        location=location,
        param_ids=getattr(marker, "ids", None),
        owner=owner,
    )


# --------------------------------------------------------------------------- discovery
class Registry:
    """All discovered fixtures, indexed by name (a name may have several location-scoped defs)."""

    def __init__(self):
        self.by_name: dict[str, list[FixtureDef]] = {}
        self.by_type: dict[type, list[str]] = {}  # native: provided-type -> [provider name]

    def add(self, fdef: FixtureDef) -> None:
        self.by_name.setdefault(fdef.name, []).append(fdef)
        if fdef.provides_type is not None:
            self.by_type.setdefault(fdef.provides_type, []).append(fdef.name)

    def bind_params(self, func) -> dict:
        """`param_name -> provider_name` for a test/provider, wired by type (name fallback)."""
        return _bind_by_type(func, self.by_type)

    def is_provider(self, name) -> bool:
        """Whether `name` is a discovered provider (vs. a bare test param filled by @cases)."""
        return name in self.by_name

    def resolve(self, name: str, module_key: str, classes: tuple = (),
                below: int | None = None) -> FixtureDef | None:
        """Nearest-override: among defs of `name` visible here, pick the most specific.

        Order, narrowest first: a fixture defined in the test's own class (or a base of it) beats one
        defined at module level, which beats a conftest, and a deeper conftest beats a shallower one.
        `classes` is the test class's MRO names — pytest collects fixtures from base classes too.
        `below` looks *past* an override: a fixture may request the very name it overrides, and must
        then be given the definition it shadows rather than itself (TID-47)."""
        best = None
        for spec, d in self.visible(name, module_key, classes):
            if below is not None and spec >= below:
                continue  # looking *past* an override, for the def it shadows
            if best is None or spec > best[0]:
                best = (spec, d)
        return best[1] if best else None

    def visible(self, name: str, module_key: str, classes: tuple = ()):
        """`(specificity, def)` for every def of `name` in scope here — narrower is larger."""
        test_dir = _test_dir(module_key)
        for d in self.by_name.get(name, ()):
            if "::" in d.location:  # class fixture: visible only inside its own class
                owner_module, _, owner_cls = d.location.partition("::")
                if owner_module != module_key or owner_cls not in classes:
                    continue
                yield 20_000, d      # narrower than anything else that can define this name
            elif d.location.endswith(".py"):  # module fixture: visible only in its own module
                if d.location == module_key:
                    yield 10_000, d
            elif _is_ancestor_dir(d.location, test_dir):
                yield _location_depth(d.location), d  # deeper dir = more specific

    def specificity(self, fdef: FixtureDef, module_key: str, classes: tuple = ()) -> int | None:
        """How specific `fdef` is here — the ceiling to look below when it requests the name it
        overrides."""
        for spec, d in self.visible(fdef.name, module_key, classes):
            if d is fdef:
                return spec
        return None

    def autouse_for(self, module_key: str, classes: tuple = ()) -> list[FixtureDef]:
        """Every autouse fixture visible to `module_key` (and the test's class), widest scope first."""
        test_dir = _test_dir(module_key)
        out = []
        for defs in self.by_name.values():
            for d in defs:
                if not d.autouse:
                    continue
                if "::" in d.location:  # class fixture: autouse only inside its own class (TID-47)
                    owner_module, _, owner_cls = d.location.partition("::")
                    visible = owner_module == module_key and owner_cls in classes
                elif d.location.endswith(".py"):
                    visible = d.location == module_key
                else:
                    visible = _is_ancestor_dir(d.location, test_dir)
                if visible:
                    out.append(d)
        out.sort(key=lambda d: -d.rank)
        return out


# Files that mark a project root, in pytest's rootdir sense. The nearest ancestor holding one bounds
# how far up `conftest.py` collection reaches (pytest's confcutdir defaults to rootdir).
# `pytest.ini` is FIRST because it is first in pytest's own precedence — an explicit pytest config
# is the strongest statement about where a project's root is. Omitting it meant a suite laid out the
# conventional way, with `pytest.ini` and a suite-wide `conftest.py` above the test directory, found
# no rootdir at all: the ancestor walk stopped immediately and every session fixture in that conftest
# silently did not exist. That is how the repo's own `fx_corpus` became unrunnable (TID-34).
_ROOTDIR_MARKERS = ("pytest.ini", "pyproject.toml", "setup.cfg", "tox.ini", "setup.py")

# Ancestor conftests, memoised per run root. They must be *executed once*: a conftest's whole job is
# side effects (env defaults, warning filters, sys.path surgery), and running it twice would apply
# them twice. `serve()` warms this before `_preimport`; `_discover` then reads it back.
_ANCESTOR_CONFTESTS: dict[str, list] = {}


# The `-m` expression from the project's pytest config, or None when there is none (TID-32).
# Populated once during discovery; consulted per node in `Engine.run`.
_MARKER_EXPR = None
_KEYWORD_EXPR = None  # `-k EXPR` as a parsed tree, or None when no name filter applies (TID-63)
_FORCE_ASYNCIO = False  # pytest-asyncio's auto mode drives every async test, whatever anyio says
_DECLARED_MARKS: frozenset = frozenset()  # names the project declared via `markers = [...]`
_STRICT_MARKS = False  # --strict-markers: using an undeclared mark is an error, as in pytest


_IGNORED: tuple = ()  # absolute paths the project's own `addopts` excludes from collection


def _ignores(project: ProjectConfig) -> tuple:
    """`--ignore` / `--ignore-glob` paths out of the project's `addopts`, resolved to absolute paths
    against the config's own directory, as pytest resolves them.

    A project that excludes a directory from its default run means it: pirn-core's `--ignore=tests/perf`
    holds benchmarks that need the `pytest-benchmark` plugin, and collecting them anyway reported 23
    failures for tests pytest never runs. Ignored here rather than in the Rust collector because this
    is where the project's own config is already being read."""
    return tuple((os.path.abspath(os.path.join(project.dir, value)), glob)
                 for flag, glob in (("--ignore", False), ("--ignore-glob", True))
                 for value in project.opt_values(flag) if value)


def _is_ignored(path: str) -> bool:
    """Is `path` (absolute) excluded by the project's own `--ignore` / `--ignore-glob`?"""
    if not _IGNORED:
        return False
    # Absolute on both sides: the run root arrives as `.` as often as not, and a relative path never
    # matches a target resolved against the config's directory.
    path = os.path.abspath(path)
    for target, glob in _IGNORED:
        if glob:
            if fnmatch.fnmatch(path, target):
                return True
        elif path == target or path.startswith(target + os.sep):
            return True
    return False


# pytest's identifier class for `-m` / `-k`: a keyword may be a parametrize id, `test_x[1-a]`.
_SELECTION_IDENT = re.compile(r"[\w.:+\-\[\]\\/]+")


def _parse_selection_expr(expr: str):
    """pytest's selection grammar — identifiers, `and`, `or`, `not`, parentheses — as a tree.

    Shared by `-m` and `-k` (TID-63): it is one grammar over two predicates. Parsed by hand rather
    than with `ast`, which the `-m` path used to use: an `ast` identifier cannot contain `-` or
    `[`, and `-k "test_x[1-a]"` is the ordinary way to name one parametrize case. Raises
    `ValueError` on anything outside the grammar, so a filter we cannot read is reported once and
    selects everything rather than silently deselecting the wrong tests.

    Tree shape: `("ident", s)`, `("not", t)`, `("and", [ts])`, `("or", [ts])`."""
    tokens: list = []
    i = 0
    while i < len(expr):
        c = expr[i]
        if c.isspace():
            i += 1
            continue
        if c in "()":
            tokens.append(c)
            i += 1
            continue
        m = _SELECTION_IDENT.match(expr, i)
        if not m:
            raise ValueError(f"unexpected character {c!r} at position {i}")
        tokens.append(m.group(0))
        i = m.end()
    pos = 0

    def peek():
        return tokens[pos] if pos < len(tokens) else None

    def take():
        nonlocal pos
        pos += 1
        return tokens[pos - 1]

    def parse_or():
        items = [parse_and()]
        while peek() == "or":
            take()
            items.append(parse_and())
        return items[0] if len(items) == 1 else ("or", items)

    def parse_and():
        items = [parse_not()]
        while peek() == "and":
            take()
            items.append(parse_not())
        return items[0] if len(items) == 1 else ("and", items)

    def parse_not():
        if peek() == "not":
            take()
            return ("not", parse_not())
        t = peek()
        if t is None:
            raise ValueError("expected an identifier")
        if t == "(":
            take()
            inner = parse_or()
            if peek() != ")":
                raise ValueError("missing ')'")
            take()
            return inner
        if t in (")", "and", "or"):
            raise ValueError(f"unexpected {t!r}")
        return ("ident", take())

    tree = parse_or()
    if pos != len(tokens):
        raise ValueError(f"unexpected {tokens[pos]!r}")
    return tree


def _evaluate_selection(tree, resolve):
    """Evaluate a selection tree; `resolve(ident)` is True, False, or None for "cannot tell yet".

    Three-valued so a node can be decided *before* its parametrize cases exist (TID-63). A `-k`
    identifier that does not match the plain node id might still match a case id — `-k 1-a` against
    `test_x[1-a]` — so at node level a non-match is "unknown", not "no". Kleene's rules: `not` of
    unknown is unknown; `and` is False on any False, else unknown on any unknown; `or` is True on
    any True, else unknown on any unknown. A False here is a False for every case that node could
    produce, which is what makes deselecting it up front — before any fixture is built — sound."""
    kind = tree[0]
    if kind == "ident":
        return resolve(tree[1])
    if kind == "not":
        v = _evaluate_selection(tree[1], resolve)
        return None if v is None else not v
    values = [_evaluate_selection(t, resolve) for t in tree[1]]
    if kind == "and":
        if any(v is False for v in values):
            return False
        return None if any(v is None for v in values) else True
    if any(v is True for v in values):
        return True
    return None if any(v is None for v in values) else False


def _compile_selection_tree(expr: str, flag: str):
    try:
        return _parse_selection_expr(expr)
    except ValueError as exc:
        _warn(f"ignoring {flag} {expr!r}: {exc}")
        return None


def _keyword_path_names(module_key: str) -> tuple:
    """The names pytest's `-k` takes from a module's *path* (TID-100).

    pytest's `KeywordMatcher` takes the name of every node on the item's chain except the session
    and the root `Directory`, and a node's name is its path relative to its parent node's. Since
    pytest 8 every directory is a `Dir` / `Package` node, so the names are each directory below
    the rootdir and the module's file name — `-k unit` selects everything under `tests/unit/`,
    4,557 of pirn-core's tests, where matching the file and test names alone selected none.
    pytest 7 nests nothing: a module whose own directory holds an `__init__.py` sits under that
    one `Package`, named by its basename, and is named by its own; any other module sits under
    the session, named by its whole path from the rootdir — `tests/test_arguments.py`, which is
    how click's `-k tests` matches on 7. The rootdir — the directory the ini was read from, else
    the run root — is the root `Directory`, whose name pytest leaves out."""
    cached = _KEYWORD_PATH_NAMES.get(module_key)
    if cached is not None:
        return cached
    root = os.path.abspath(_ROOT or ".")
    rootdir = _PROJECT.dir if _PROJECT is not None else root
    module_path = os.path.join(root, module_key)
    try:
        rel = os.path.relpath(module_path, rootdir)
        if rel.startswith(os.pardir):  # the ini sits beside, not above: the run root is the rootdir
            rootdir, rel = root, os.path.relpath(module_path, root)
    except ValueError:  # Windows: the config and the run root on different drives — no common
        rootdir, rel = root, module_key  # ancestor; the run root is the rootdir then
    parts = [p for p in rel.replace("\\", "/").split("/") if p and p != os.curdir and p != os.pardir]
    if _pytest_major() >= 8:
        names = tuple(parts)
    elif len(parts) > 1 and os.path.exists(os.path.join(os.path.dirname(module_path), "__init__.py")):
        names = (parts[-2], parts[-1])
    else:
        names = ("/".join(parts),)
    _KEYWORD_PATH_NAMES[module_key] = names
    return names


_KEYWORD_PATH_NAMES: dict[str, tuple] = {}  # per module: fixed for the life of the process


def _keyword_names(node_id: str, marks: set) -> list:
    """What pytest's `-k` matches against: the names its path gives the node (TID-100), every
    `::` segment — class, function, the function with its parametrize id — and the node's mark
    names."""
    parts = node_id.split("::")
    return [*_keyword_path_names(parts[0]), *parts[1:], *sorted(marks)]


def _keyword_matches(ident: str, names: list) -> bool:
    """pytest's rule: a case-insensitive substring of any of the names."""
    needle = ident.lower()
    return any(needle in name.lower() for name in names)


def _keyword_verdict(node_id: str, marks: set, final: bool):
    """`_KEYWORD_EXPR` applied to a node: True (run it), False (deselect it), or None (decide per
    case). `final=True` — the id is a complete case id, or the node has no cases — turns every
    non-match into a No."""
    names = _keyword_names(node_id, marks)

    def resolve(ident: str):
        if _keyword_matches(ident, names):
            return True
        return False if final else None

    return _evaluate_selection(_KEYWORD_EXPR, resolve)


def _compile_marker_expr(expr: str):
    """A predicate over a set of mark names for one pytest `-m` expression.

    The same grammar `-k` uses (TID-63), over "is this identifier one of the node's marks"."""
    tree = _compile_selection_tree(expr, "-m")
    if tree is None:
        return None
    return lambda marks: bool(_evaluate_selection(tree, lambda ident: ident in marks))


def _rootdir(root: str) -> str | None:
    """The nearest ancestor of `root` holding a project marker, or None if there is none.

    This is the ceiling for ancestor-conftest collection. Returning None when nothing is found keeps
    a rootless tree behaving exactly as it did before ancestor collection existed, rather than
    walking to `/` and importing whatever happens to be up there."""
    cur = os.path.abspath(root)
    while True:
        parent = os.path.dirname(cur)
        if parent == cur:  # hit the filesystem root without finding a marker
            return None
        if any(os.path.exists(os.path.join(parent, m)) for m in _ROOTDIR_MARKERS):
            return parent
        cur = parent


def _load_ancestor_conftests(root: str) -> list:
    """Import every `conftest.py` between rootdir and the run root, shallowest first (TID-19).

    `os.walk(root)` only ever sees the tree at or below the run root, so a `conftest.py` beside
    `pyproject.toml` — the conventional home for suite-wide setup — was silently skipped. pytest
    collects conftests from rootdir down, and suites rely on it: env defaults, warning filters,
    `sys.path` surgery, plugin registration. Skipping it does not degrade gracefully; it surfaces
    later as a failure whose stated cause points nowhere near conftest discovery.

    Returns `[(module, location)]` where location is a `..`-relative dir (see `_location_depth`)."""
    key = os.path.abspath(root)
    cached = _ANCESTOR_CONFTESTS.get(key)
    if cached is not None:
        return cached

    out: list = []
    ceiling = _rootdir(root)
    if ceiling is not None:
        # rootdir → run root, shallowest first, so a nearer conftest's side effects win by running last.
        chain, cur = [], key
        while cur != ceiling and os.path.dirname(cur) != cur:
            cur = os.path.dirname(cur)
            chain.append(cur)
            if cur == ceiling:
                break
        for directory in reversed(chain):
            path = os.path.join(directory, "conftest.py")
            if not os.path.exists(path):
                continue
            location = os.path.relpath(directory, key).replace(os.sep, "/")
            module = _import_conftest(path, location)
            if module is not None:
                _collect_addoption(module)
                out.append((module, location))

    _ANCESTOR_CONFTESTS[key] = out
    return out


# Directories never descended into during discovery. Mirrors `RegexCollector::SKIP_DIRS` — the two
# walks must agree, or the shim reports fixtures for files collection never saw (and vice versa).
_SKIP_DIRS = frozenset({
    "__pycache__", ".git", ".venv", "venv", ".tox", ".nox", "site-packages",
    ".tiderace-spike-venv", ".tiderace-bench-venv", ".tiderace-fx-venv", ".tiderace-cache",
    ".pytest_cache", "node_modules", ".mypy_cache", ".ruff_cache",
})


def _walk_suite(root: str):
    """`os.walk` over the suite, skipping what is not part of it, in a deterministic order.

    Two things this gets right that the obvious spelling does not.

    **The prune has to happen during the walk.** `sorted(os.walk(root))` reads the *entire* tree
    before yielding anything, so assigning to `dirs` afterwards prunes nothing — the traversal has
    already been to every directory. Sorting `dirs` in place instead gives the same deterministic
    order and lets the prune actually take effect.

    **`.venv` is not part of the suite.** Without a skip list the walk finds and imports the
    *dependencies'* test suites: numpy ships its own, and `.venv/…/numpy/conftest.py` was imported on
    every run of any project with numpy installed. Slow, wrong, and a source of foreign collection
    state — a `pytestmark` from someone else's suite reaching our marker handling is how the scalar
    `pytestmark` crash above was found."""
    for current, dirs, files in os.walk(root):
        dirs[:] = sorted(d for d in dirs if d not in _SKIP_DIRS)
        yield current, dirs, files


def _discover(root: str) -> Registry:
    reg = Registry()
    # The project's own config, read before the walk: `--ignore` has to prune it, and the `-m` filter
    # below is read from the same place.
    project = load_project_config(root)
    global _IGNORED, _PROJECT
    _IGNORED = _ignores(project)
    _PROJECT = project
    native: list[tuple] = []  # (provider obj, location) — resolved in a second pass (see below)
    conftests: list = []  # every conftest module, for the collection hooks (TID-20)
    _CONFTEST_SCOPES.clear()  # rebuilt with them: which directory each one governs (TID-85)
    test_modules: list = []  # (module, rel path) — the items those hooks inspect
    # Ancestor conftests first: their fixtures are the widest in the tree, and `serve()` has already
    # executed them ahead of `_preimport` so their side effects precede every test-module import.
    for module, location in _load_ancestor_conftests(root):
        conftests.append(module)
        _CONFTEST_SCOPES.append((location, module))
        for attr, obj in list(vars(module).items()):
            if _is_native_provider(obj):
                native.append((obj, location))
            elif _is_fixture(obj):
                reg.add(_fixture_def(obj, location, attr_name=attr))
    for current, dirs, files in _walk_suite(root):
        rel_dir = os.path.relpath(current, root)
        rel_dir = "" if rel_dir == "." else rel_dir.replace(os.sep, "/")
        if _dir_skip(rel_dir) is not None:
            dirs[:] = []  # a skipped conftest's subtree is not collected at all, as in pytest
            continue
        if _is_ignored(current):
            dirs[:] = []
            continue
        # The directory's conftest before its test modules: it may skip the directory, and `sorted`
        # alone would put `a_test.py` ahead of `conftest.py`.
        for name in sorted(files, key=lambda n: (n != "conftest.py", n)):
            if _dir_skip(rel_dir) is not None:
                dirs[:] = []
                break
            if not name.endswith(".py"):
                continue
            path = os.path.join(current, name)
            if name == "conftest.py":
                module, location = _import_conftest(path, rel_dir), rel_dir
                if module is not None:
                    _collect_addoption(module)
                    conftests.append(module)
                    _CONFTEST_SCOPES.append((location, module))
            elif name.startswith("test_") or name.endswith("_test.py"):
                # Named through `_module_name`, exactly as execution names it (TID-37). The old
                # spelling was relative to the run *root*, which forced the run root itself onto
                # `sys.path` — and a run root that is a package is what shadowed the stdlib. It was
                # also a latent double-import: when the two spellings disagreed, the same file was
                # imported twice under two names, so a module-level fixture could register against
                # one copy while the test ran against the other.
                location = os.path.relpath(path, root).replace(os.sep, "/")
                if not _module_selected(location):
                    global _SKIPPED_AT_DISCOVERY
                    _SKIPPED_AT_DISCOVERY += 1
                    continue  # this run will not execute it (TID-75)
                rel = _module_name(location)
                try:
                    module = importlib.import_module(rel)
                except (Exception, *_SKIP_EXCEPTIONS):  # noqa: BLE001 — surfaces per-test, not at discovery
                    continue
                test_modules.append((module, location))
            else:
                continue
            if module is None:
                continue
            for attr, obj in list(vars(module).items()):
                if _is_native_provider(obj):  # native-first (ADR-E012); pytest is compat fallback
                    native.append((obj, location))
                elif _is_fixture(obj):
                    reg.add(_fixture_def(obj, location, attr_name=attr))
                elif isinstance(obj, type):
                    # Fixtures defined inside a test class. pytest scopes these to the class, where
                    # they commonly *override* a conftest fixture of the same name for that class
                    # alone — flask's `TestRoutes.app` is exactly that — so they are registered with
                    # a `module.py::Class` location rather than merged into the module (TID-47).
                    _register_class_fixtures(reg, obj, location)

    # After every conftest is loaded and every test module imported — the hooks need both, and the
    # marks they inspect only exist once the decorators have run.
    _run_collection_hooks(conftests, test_modules)

    # The project's own `-m` filter (TID-32). Read here rather than at each node so a malformed
    # expression is reported once.
    global _MARKER_EXPR, _DECLARED_MARKS, _STRICT_MARKS
    # The command line wins over the project's own `addopts`, as it does in pytest: a config filter is
    # the project's default, and `-m` on the command line is this run's intent (TID-59).
    expr = _env("TIDERACE_MARKER_EXPR") or project.opt("-m")
    _MARKER_EXPR = _compile_marker_expr(expr) if expr else None
    _DECLARED_MARKS, _STRICT_MARKS = _registered_marks(project)
    if _STRICT_MARKS:
        # Ask pytest for the plugins' marks *now*, in the process every worker is forked from: the
        # answer was fetched lazily by the first strict node each worker met, a 0.5s subprocess
        # per worker per run — and the largest single cost of a `-k` run on pirn-core (TID-91).
        _plugin_marks()
    # `-k EXPR`, same precedence (TID-63): the command line over the project's own `addopts`.
    global _KEYWORD_EXPR
    kexpr = _env("TIDERACE_KEYWORD_EXPR") or project.opt("-k")
    _KEYWORD_EXPR = _compile_selection_tree(kexpr, "-k") if kexpr else None
    # `asyncio_mode = "auto"` means pytest-asyncio claims *every* async test, including ones carrying
    # `@pytest.mark.anyio`. In that configuration pytest runs even a `[trio]`-labelled variant on an
    # asyncio loop — the id says trio and the loop never is. Emulating the suite's configured
    # toolchain is the job here, so the same thing happens: the expansion still produces one variant
    # per backend, as pytest's ids do, and they all run where pytest runs them (TID-54).
    global _FORCE_ASYNCIO
    _FORCE_ASYNCIO = False
    if any(str(v).strip().strip('"\'') == "auto" for v in project.values("asyncio_mode")):
        try:
            import pytest_asyncio  # noqa: F401 — only its presence matters
            _FORCE_ASYNCIO = True
        except Exception:  # noqa: BLE001 — declared but not installed: nothing claims the tests
            pass

    # Native providers wire by type, so provider→provider deps need the FULL type set first: build the
    # type index, then build the defs (a two-pass the name-DI pytest path doesn't need).
    type_index: dict = {}
    for obj, _loc in native:
        spec = obj.__tiderace_provider__
        type_index.setdefault(spec.provides, []).append(spec.name)
    for obj, location in native:
        reg.add(_native_fixture_def(obj, location, type_index))
    _register_builtins(reg)
    # Last, so everything above — a conftest at any depth, the builtins, the native anyio_backend —
    # takes precedence over a plugin's fixture of the same name, as in pytest (TID-87).
    _register_plugin_fixtures(reg, project, conftests)
    return reg


# --------------------------------------------------------------------------- plugin fixtures
# pytest's own plugins are what the shim replaces; their fixtures come from `tiderace.builtins`.
_PYTEST_OWN_PLUGINS = ("pytester", "_pytest", "pytest")


def _plugin_modules(project: ProjectConfig, conftests: list) -> list:
    """`(plugin name, module name)` for every pytest plugin the project would load (TID-87):
    the `pytest11` entry points of the installed distributions, `-p NAME` in `addopts`, and each
    conftest's `pytest_plugins` — minus `-p no:NAME`, minus pytest's own, and subject to
    `TIDERACE_PLUGINS` (`none`, or a comma-separated allow-list) or `[tool.tiderace] plugins`.
    `PYTEST_DISABLE_PLUGIN_AUTOLOAD` turns the entry points off, as it does for pytest; the
    explicit spellings still load."""
    allow = _env("TIDERACE_PLUGINS")
    if allow is None:
        configured = project.setting("plugins")
        if isinstance(configured, (list, tuple)):
            allow = ",".join(str(v) for v in configured) or "none"  # `plugins = []`: none at all
        elif isinstance(configured, str):
            allow = configured
    if allow is not None and allow.strip().lower() in ("none", ""):
        return []
    allowed = {n.strip() for n in allow.split(",") if n.strip()} if allow is not None else None
    explicit: list = []
    disabled: set = set()
    for value in project.opt_values("-p"):
        if value.startswith("no:"):
            disabled.add(value[3:])
        else:
            explicit.append(value)
    found: list = []
    seen: set = set()

    def take(name: str, module: str) -> None:
        if name in disabled or module in disabled or module in seen:
            return
        if any(module == own or module.startswith(own + ".") for own in _PYTEST_OWN_PLUGINS):
            return
        if allowed is not None and name not in allowed and module not in allowed:
            return
        seen.add(module)
        found.append((name, module))

    if not os.environ.get("PYTEST_DISABLE_PLUGIN_AUTOLOAD"):
        try:
            from importlib.metadata import entry_points
            eps = list(entry_points(group="pytest11"))
        except Exception:  # noqa: BLE001 — no metadata machinery: no entry points
            eps = []
        for ep in sorted(eps, key=lambda e: e.name):
            take(ep.name, ep.value.split(":", 1)[0].strip())
    for value in explicit:
        take(value, value)
    for module in conftests:
        declared = _safe_getattr(module, "pytest_plugins", None)
        if isinstance(declared, str):
            declared = [declared]
        for value in declared or ():
            if isinstance(value, str):
                take(value, value)
    return found


def _register_plugin_fixtures(reg: Registry, project: ProjectConfig, conftests: list) -> None:
    """Import each plugin module and register the fixtures it defines at the root location, after
    everything else (TID-87): a suite's own fixture of the same name — a conftest at any depth, a
    test module's — already outranks it, and a name the shim itself provides (a builtin, the native
    `anyio_backend`) is left alone. Only fixtures are taken; the plugin's hooks are never called,
    except `pytest_addoption`, which is recorded exactly as a conftest's is (TID-14) so its
    options and ini defaults read back through `config`."""
    for name, module_name in _plugin_modules(project, conftests):
        try:
            module = importlib.import_module(module_name)
        except (Exception, *_SKIP_EXCEPTIONS) as exc:  # noqa: BLE001 — one plugin, not the run
            _warn(f"pytest plugin {name!r} ({module_name}) not loaded: {exc!r}")
            continue
        _collect_addoption(module)
        for attr, obj in list(vars(module).items()):
            if not _is_fixture(obj):
                continue
            fdef = _fixture_def(obj, "", attr_name=attr)
            if any(d.location == "" for d in reg.by_name.get(fdef.name, ())):
                continue  # the shim's own, or a root conftest's: theirs wins
            reg.add(fdef)


def _register_class_fixtures(reg: Registry, cls: type, module_key: str) -> None:
    """Register every fixture a test class defines, including ones it inherits.

    pytest collects a class's fixtures from its whole MRO, so a base class holding shared fixtures
    works. Each def is filed under the class it is *looked up from*, which is what makes an override
    in a subclass beat its base."""
    names = _safe_getattr(cls, "__mro__", None) or ()
    for base in names:
        if base is object:
            continue
        for attr, obj in list(vars(base).items()):
            if not _is_fixture(obj):
                continue
            fdef = _fixture_def(obj, f"{module_key}::{cls.__name__}", owner=cls, attr_name=attr)
            # A subclass that redefines the name has already registered its own def for this class;
            # the base's copy would be an identical location and must not shadow it.
            if any(d.location == fdef.location for d in reg.by_name.get(fdef.name, ())):
                continue
            reg.add(fdef)


def _register_builtins(reg: Registry) -> None:
    """Register tiderace's always-available builtin resources (ROADMAP-v2 B1: monkeypatch/tmp_path/
    capsys/capfd/caplog) at the root location (""), so every test can request them — by type (the
    migrated form, `mp: MonkeyPatch`) or by name (the pytest form, `monkeypatch`), with no per-tree
    import.

    Staying best-effort is deliberate: a pure-pytest suite driven by a bare interpreter has no
    `tiderace` installed and must still run. But the failure is now **announced** (TID-21). Silence
    here meant every builtin was quietly missing while the suite stayed green, which is how the CI
    fixture venv went a long time with no builtin coverage at all and how `tmp_path` sat recorded as
    36 open errors months after it worked."""
    try:
        import tiderace.builtins as builtins_pkg
    except Exception as exc:  # noqa: BLE001 — tiderace not importable ⇒ no builtins
        _warn(f"builtin providers unavailable ({exc!r}) — monkeypatch/tmp_path/capsys/"
              f"capfd/caplog will not resolve. Install `tiderace` into this interpreter, or put "
              f"engine/py-tiderace on PYTHONPATH.")
        return
    # The builtins read the run root, the declared options and the ini values through one
    # accessor (TID-111); hand them this module, whose globals they used to reach with
    # `import shim`. Per interpreter: a sub-interpreter registers its own copy.
    builtins_pkg.set_context(builtins_pkg._runtime.ModuleContext(sys.modules[__name__]))
    for obj in builtins_pkg.providers():
        reg.add(_native_fixture_def(obj, "", {}))
    _register_anyio_backend(reg)


def _register_anyio_backend(reg: Registry) -> None:
    """Provide `anyio_backend` when anyio is installed, as anyio's own plugin would.

    The fixture a `@pytest.mark.anyio` test runs against is not declared by the suite — anyio's plugin
    declares it, parametrised over the backends that are actually installed:

        @pytest.fixture(scope="module", params=get_available_backends())
        def anyio_backend(request): return request.param

    Tiderace hosts no plugins, so nothing supplied it and every marked test ran **once**, on the
    default loop, where its author asked for one run per backend. The tests passed, which is what
    made it dangerous: half the intended coverage was missing and nothing said so (TID-54).

    Registered at the root location, so a suite that declares its own `anyio_backend` — anyio's test
    suite does, to add uvloop — still overrides this one by ordinary nearest-wins resolution."""
    try:
        import anyio  # noqa: F401 — presence is the question
    except Exception:  # noqa: BLE001 — no anyio, nothing to provide
        return
    backends: list = []
    try:
        from anyio.pytest_plugin import get_available_backends

        backends = list(get_available_backends())
    except Exception:  # noqa: BLE001 — older or restructured anyio: work it out directly
        backends = ["asyncio"]
        try:
            import trio  # noqa: F401

            backends.append("trio")
        except Exception:  # noqa: BLE001
            pass
    if not backends:
        return

    def anyio_backend(request):
        return request.param

    reg.add(FixtureDef(name="anyio_backend", scope="module", params=backends, autouse=False,
                       func=anyio_backend, location=""))


_DIR_SKIPS: dict[str, str] = {}  # suite-relative dir ("" = everything) -> why its conftest skipped it
# suite-relative dir ("" = everything) -> the traceback of its conftest's failed import (TID-72).
# pytest stops at collection with one error and runs nothing; every test under that conftest is
# reported here with the conftest's own traceback, which is the same verdict per test.
_DIR_ERRORS: dict[str, str] = {}


def _dir_mark(marks: dict, rel_path: str) -> str | None:
    """The mark (a skip reason, a conftest's import failure) covering `rel_path` — a suite-relative
    file or directory — from the nearest ancestor directory that carries one; `""` covers all."""
    if not marks:
        return None
    if "" in marks:
        return marks[""]
    parts = rel_path.split("/")
    for depth in range(len(parts), 0, -1):
        mark = marks.get("/".join(parts[:depth]))
        if mark is not None:
            return mark
    return None


def _dir_skip(rel_path: str) -> str | None:
    """The skip reason covering `rel_path`, if a conftest skipped it."""
    return _dir_mark(_DIR_SKIPS, rel_path)


def _dir_error(rel_path: str) -> str | None:
    """The conftest import failure covering `rel_path`, if one of its conftests did not import."""
    return _dir_mark(_DIR_ERRORS, rel_path)


def _skip_reason(exc: BaseException) -> str:
    return str(getattr(exc, "msg", None) or exc) or type(exc).__name__


def _import_conftest(path: str, rel_dir: str):
    # Ancestor dirs (TID-19) arrive as `..`, `../..`, … — dotted, non-identifier, and indistinguishable
    # from each other once punctuation is stripped. Name them by how far up they sit instead.
    suffix = f"up{len(rel_dir.split('/'))}" if rel_dir.startswith("..") else rel_dir.replace("/", "_")
    mod_name = "_fx_conftest_" + (suffix or "root")
    try:
        spec = importlib.util.spec_from_file_location(mod_name, path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[mod_name] = module
        spec.loader.exec_module(module)
        return module
    except _SKIP_EXCEPTIONS as exc:
        # `pytest.importorskip("ray")` at the top of a conftest skips that directory — pytest collects
        # nothing below it (TID-48). `Skipped` is a BaseException, so it used to sail past the handler
        # below and kill the shim during discovery: under the shared-import pool that was the pool
        # parent, and the entire run failed before a single test started.
        _DIR_SKIPS["" if rel_dir.startswith("..") else rel_dir] = _skip_reason(exc)
        return None
    except Exception as exc:  # noqa: BLE001 — a broken conftest is every test under it, not the run
        # A conftest that fails to import takes its fixtures and its side effects with it. The tests
        # below it used to run anyway and mostly pass — 508 of 511 on fx_corpus with a conftest that
        # raised on import — while the few that needed a fixture failed naming the fixture rather than
        # the cause (TID-72). pytest stops at collection with the conftest's error and runs nothing;
        # the per-test equivalent is every test under that conftest erroring with that traceback,
        # which `run()` reports the way it reports a conftest-level skip (TID-48).
        _warn(f"could not import {path}: {exc!r}")
        _DIR_ERRORS["" if rel_dir.startswith("..") else rel_dir] = (
            f"conftest {path} failed to import:\n"
            + "".join(traceback.format_exception(type(exc), exc, exc.__traceback__)))
        return None


# --------------------------------------------------------------------------- closure
def _closure(reg: Registry, module_key: str, requested: dict, extra: list | None = None,
             classes: tuple = ()) -> list[FixtureDef]:
    """Resolved fixture closure for a test, dependencies-before-dependents (topo). Includes
    requested fixtures (the provider names of `requested`'s param→provider bindings), `extra` provider
    names (e.g. `@tiderace.uses` — set up but not injected), all in-scope autouse fixtures, and their
    transitive deps."""
    ordered: list[FixtureDef] = []
    # Keyed by definition, not by name: `app` overriding `app` is two defs that both have to be set
    # up, outer first, so the override receives the value it wraps (TID-47).
    seen: set = set()
    visiting: set = set()

    def visit(name: str, below: int | None = None) -> None:
        d = reg.resolve(name, module_key, classes, below=below)
        if d is None:
            return  # unknown name (e.g. a non-fixture arg) — the body call will surface it
        key = (d.name, d.location)
        if key in seen or key in visiting:
            return
        visiting.add(key)
        for dep in d.deps:
            # A fixture requesting its own name wants the definition it overrides — pytest's
            # override-and-extend idiom, e.g. `def app(self, app)` inside a test class.
            visit(dep, below=reg.specificity(d, module_key, classes) if dep == d.name else None)
        visiting.discard(key)
        if key not in seen:
            seen.add(key)
            ordered.append(d)

    # pytest's closure order, which is also the order its parametrised-fixture axes take in a node
    # id: the autouse fixtures, then `usefixtures` (and what a marker implies — anyio's backend),
    # then the signature's arguments. anyio's `TestConnectedUDPSocket.test_iterate(family)` is
    # `[asyncio-ipv4]` under pytest, the backend the plugin's `usefixtures` injects before the
    # `family` the test asks for (TID-87).
    for d in reg.autouse_for(module_key, classes):
        visit(d.name)
    for provider_name in extra or ():
        visit(provider_name)
    for provider_name in requested.values():
        visit(provider_name)
    return ordered


# --------------------------------------------------------------------------- execution engine
class _Active:
    __slots__ = ("fdef", "key", "value", "gen")

    def __init__(self, fdef, key, value, gen):
        self.fdef = fdef
        self.key = key
        self.value = value
        self.gen = gen


def _instance_key(fdef: FixtureDef, node_id: str):
    s = fdef.scope
    if s == "session":
        return ("session", fdef.name)
    if s == "package":
        return ("package", fdef.name, fdef.location)
    if s == "module":
        return ("module", fdef.name, _module_key(node_id))
    if s == "class":
        return ("class", fdef.name, _module_key(node_id), _class_method(node_id)[0])
    return ("function", fdef.name, node_id)


def _setup_fixture(fdef: FixtureDef, args: dict, param):
    """Run a fixture body up to its first yield (or to completion). Returns (value, handle)."""
    call_args = dict(args)
    request = _Request(param, _CURRENT_NODE) if fdef.wants_request else None
    if request is not None:
        call_args["request"] = request
    if fdef.is_yield:
        gen = fdef.func(*_owner_args(fdef), **call_args)
        value, handle = next(gen), gen
    else:
        value, handle = fdef.func(*_owner_args(fdef), **call_args), None
    if request is not None:
        # Wrapped whenever the fixture takes a `request`, not only when it registered a finalizer
        # during its own body. A fixture that hands the test a callable — flask's `purge_module` is
        # the canonical shape — registers nothing at setup time and everything later, from inside the
        # test body. Deciding here whether to wrap therefore dropped exactly those finalizers, and a
        # module a test asked to have purged stayed in `sys.modules` for its neighbours (TID-56).
        # The list is shared by reference, so later appends are seen; an empty one tears down as
        # cheaply as before.
        handle = _FinalizingHandle(handle, request._finalizers)
    return value, handle


def _teardown(gen) -> None:
    if isinstance(gen, _FinalizingHandle):
        _teardown(gen.inner)  # the yield teardown is the newest finalizer, so it runs first
        _run_finalizers(gen.finalizers)
        return
    if gen is None:
        return
    try:
        next(gen)
    except StopIteration:
        pass
    except Exception:  # noqa: BLE001 — a teardown error must not abort remaining finalizers
        pass


# --------------------------------------------------------------------------- async providers (B5)
def _is_async_fixture(func) -> bool:
    """An `async def` provider (coroutine) or `async def ... yield` provider (async generator)."""
    return inspect.iscoroutinefunction(func) or inspect.isasyncgenfunction(func)


async def _setup_fixture_async(fdef: FixtureDef, args: dict, param):
    """Async-aware setup: drives sync *and* async providers up to their first (a)yield. Returns
    `(value, handle)` where handle is `None` | `("gen", g)` | `("agen", ag)` for teardown."""
    call_args = dict(args)
    request = _Request(param, _CURRENT_NODE) if fdef.wants_request else None
    if request is not None:
        call_args["request"] = request
    if inspect.isasyncgenfunction(fdef.func):
        ag = fdef.func(*_owner_args(fdef), **call_args)
        value, handle = await ag.__anext__(), ("agen", ag)
    elif inspect.iscoroutinefunction(fdef.func):
        value, handle = await fdef.func(*_owner_args(fdef), **call_args), None
    elif fdef.is_yield:  # a sync yield-fixture used alongside async ones
        gen = fdef.func(*_owner_args(fdef), **call_args)
        value, handle = next(gen), ("gen", gen)
    else:
        value, handle = fdef.func(*_owner_args(fdef), **call_args), None
    if request is not None:
        # Wrapped whenever the fixture takes a `request`, not only when it registered a finalizer
        # during its own body. A fixture that hands the test a callable — flask's `purge_module` is
        # the canonical shape — registers nothing at setup time and everything later, from inside the
        # test body. Deciding here whether to wrap therefore dropped exactly those finalizers, and a
        # module a test asked to have purged stayed in `sys.modules` for its neighbours (TID-56).
        # The list is shared by reference, so later appends are seen; an empty one tears down as
        # cheaply as before.
        handle = _FinalizingHandle(handle, request._finalizers)
    return value, handle


async def _teardown_async(handle) -> None:
    if isinstance(handle, _FinalizingHandle):
        await _teardown_async(handle.inner)
        _run_finalizers(handle.finalizers)
        return
    if handle is None:
        return
    kind, g = handle
    try:
        if kind == "agen":
            await g.__anext__()
        else:
            next(g)
    except (StopIteration, StopAsyncIteration):
        pass
    except Exception:  # noqa: BLE001 — a teardown error must not abort remaining finalizers
        pass


# --------------------------------------------------------------------------- static purity pre-filter
# Calls that touch PROCESS-GLOBAL state (a sufficient, conservative signal of impurity — no run needed).
_IMPURE_CALLS = frozenset({
    "os.chdir", "os.putenv", "os.unsetenv", "os.environ.update", "os.environ.pop",
    "os.environ.setdefault", "os.environ.clear", "random.seed", "numpy.random.seed", "np.random.seed",
    "locale.setlocale", "signal.signal", "sys.setrecursionlimit", "warnings.filterwarnings",
    "warnings.simplefilter", "setattr", "delattr", "globals", "__import__",
})


def _local_names(fn) -> set:
    """Names bound locally in `fn` (params + assignment/loop/with/comprehension targets) — used to tell
    a write to a *local* (fine) from a write to a *free* name (a module global / closure → impure)."""
    names = set()
    a = fn.args
    for arg in (*a.posonlyargs, *a.args, *a.kwonlyargs, a.vararg, a.kwarg):
        if arg is not None:
            names.add(arg.arg)
    for node in ast.walk(fn):
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store):
            names.add(node.id)
    return names


def _assign_root(target) -> str | None:
    """The root Name of an assignment target: `a` for `a`, `a[k]`, `a.b`, `a.b[k]` (None otherwise)."""
    while isinstance(target, (ast.Subscript, ast.Attribute)):
        target = target.value
    return target.id if isinstance(target, ast.Name) else None


def _dotted_call(call) -> str:
    """Dotted name of a call's callee: `os.chdir(...)` → 'os.chdir'."""
    node, parts = call.func, []
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
    return ".".join(reversed(parts))


def static_impurity(func) -> str | None:
    """A **sufficient** (conservative) static impurity test — decided WITHOUT running. Returns a reason
    when the source obviously mutates shared state (`global`, a write to a free/module name, env or
    process-global calls), else `None` (no obvious impurity ⇒ a no-fork *candidate*, to be confirmed by
    the runtime guard). Over-approximates impurity (the safe direction): a false 'impure' only costs a
    fork; it never wrongly green-lights an unsafe no-fork."""
    try:
        src = textwrap.dedent(inspect.getsource(func))
        fn = next(n for n in ast.walk(ast.parse(src)) if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef)))
    except (OSError, TypeError, SyntaxError, StopIteration):
        return None  # can't read source ⇒ no static verdict; the runtime guard decides
    local = _local_names(fn)
    for node in ast.walk(fn):
        if isinstance(node, ast.Global):
            return f"`global {', '.join(node.names)}`"
        if isinstance(node, (ast.Assign, ast.AugAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            for t in targets:
                if not isinstance(t, ast.Name):  # subscript/attr write — a mutation through the root
                    root = _assign_root(t)
                    if root and root not in local:
                        return f"writes to non-local `{root}`"
                elif isinstance(node, ast.AugAssign) and t.id not in local:
                    return f"augments non-local `{t.id}`"
        if isinstance(node, ast.Call) and _dotted_call(node) in _IMPURE_CALLS:
            return f"calls `{_dotted_call(node)}`"
    return None


# --------------------------------------------------------------------------- purity guard (→ batching)
_OPAQUE = object()
# The purity tri-state (`_UNKNOWN_PURITY`, `None`, a reason) is defined with its wire encoding in `results.py`.

# Windows has no `fork()`. The isolation ladder's bottom rung (fork an opaque module) therefore doesn't
# exist there, so the shim must decide what to do instead rather than call `os.fork` and raise.
_FORK_AVAILABLE = hasattr(os, "fork")

# A fork child that cannot send its result frame at all exits with this code, so the parent can say
# *that* happened rather than falling through to the generic "no result" (TID-15). Picked above the
# signal-exit band (128+N) so it can't be confused with a shell-reported death-by-signal.
_EXIT_UNREPORTABLE = 199


def _child_fault_detail(exc: BaseException) -> str:
    """The diagnostic a fork child sends when it fails OUTSIDE the test body — fixture setup or
    teardown, the coverage probe, the purity snapshot. `_invoke` already formats body failures; this
    is the path that used to vanish into `no result from child`, so it names the stage explicitly and
    carries the full traceback (the child is about to `_exit`, so this is the only chance to say it)."""
    label = "the test body was never reached" if isinstance(exc, Exception) else type(exc).__name__
    try:
        trace = "".join(traceback.format_exception(type(exc), exc, exc.__traceback__))
    except BaseException:  # noqa: BLE001 — a __repr__ that raises must not cost us the whole frame
        trace = "".join(traceback.format_exception_only(type(exc), exc))
    return f"fixture setup/teardown raised ({label}):\n{trace}"


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


# The test frameworks themselves. A suite mutating pytest's internals is not the leak this is for,
# and pytest is large: including it made the scan below cost 7ms, more than the tests it wraps.
_UNWATCHED_PACKAGES = frozenset({"pytest", "_pytest", "py", "tiderace", "unittest", "hypothesis"})

_WATCHED_PACKAGES: dict = {}  # module key -> the third-party packages its imports reach


def _watched_packages(module_key: str) -> tuple:
    """Top-level **non-stdlib** packages a test module imports, for registry watching (TID-46).

    Scoped deliberately. Watching every module in `sys.modules` would cost more than the tests do,
    and watching the standard library would demote constantly for no reason: `re._cache` is a
    module-level dict that grows the first time anything compiles a pattern, which is not a leak.
    A library the *test file itself* imports is where a registry mutation can plausibly come from —
    click's `_available_shells`, a codec or plugin table, a framework's app registry."""
    cached = _WATCHED_PACKAGES.get(module_key)
    if cached is not None:
        return cached
    roots: set = set()
    path = os.path.join(_ROOT or ".", module_key)
    for name, level in _imported_names(path):
        if level or not name:
            continue  # a relative import is the suite's own code, covered by the module snapshot
        root = name.partition(".")[0]
        if root and root not in sys.stdlib_module_names and root not in _UNWATCHED_PACKAGES:
            roots.add(root)
    result = tuple(sorted(roots))
    _WATCHED_PACKAGES[module_key] = result
    return result


# module key -> (sys.modules size, [(name, module)] watched, their namespace sizes, targets)
_REGISTRY_TARGETS: dict = {}


def _registry_targets(module_key: str) -> list:
    """The module-level containers worth watching for this test's module, found once.

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
    cached = _REGISTRY_TARGETS.get(module_key)
    if cached is not None and cached[0] == size:
        _, watched, sizes, targets = cached
        if (all(sys.modules.get(name) is module for name, module in watched)
                and tuple(len(vars(module)) for _, module in watched) == sizes):
            return targets
    roots = _watched_packages(module_key)
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
    _REGISTRY_TARGETS[module_key] = (size, watched, sizes, targets)
    return targets


def _registry_snapshot(module_key: str) -> dict:
    """Shallow copies of the module-level containers in the packages this test's module imports.

    Copied rather than merely sized, because detection alone does not help the *neighbours*: the
    offender can be re-run in the clean room, but the worker it polluted keeps serving tests, and
    they would go on seeing a registry entry that a finished test added. A shallow copy is enough to
    put the container back — the entries themselves are the library's, not ours to duplicate.

    Restored **in place** (`clear` + refill), never rebound: other modules hold references to that
    exact dict, and swapping in a new one would leave them looking at the polluted original — the
    same reason `_restore_in_place` exists (TID-22)."""
    out: dict = {}
    for label, container in _registry_targets(module_key):
        out[label] = (container, copy.copy(container))
    return out


def _is_test_owned(value, module_key: str) -> bool:
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
    if origin == _module_name(module_key):
        return True  # defined in this very test module, function-local classes included
    module = sys.modules.get(origin)
    file = _safe_getattr(module, "__file__", None) if module is not None else None
    if not file:
        return False
    name = os.path.basename(file)
    # A conftest counts: a fixture registering something for its tests is still test-side setup.
    return name == "conftest.py" or name.startswith("test_") or name.endswith("_test.py")


def _registry_delta(before: dict, module_key: str) -> str | None:
    """What the suite added to another module's containers since `before`, without touching it —
    the per-test verdict's view; `_restore_registries` puts it back at the module boundary."""
    changed = []
    for key, (container, saved) in before.items():
        try:
            if container == saved:
                continue
            if isinstance(container, dict):
                added = any(k not in saved and _is_test_owned(container[k], module_key) for k in container)
            else:
                added = any(v not in saved and _is_test_owned(v, module_key) for v in container)
        except Exception:  # noqa: BLE001 — an uncooperative container is not a verdict
            continue
        if added:
            changed.append(key)
    if not changed:
        return None
    shown = ", ".join(sorted(changed)[:3])
    more = "" if len(changed) <= 3 else f" (+{len(changed) - 3} more)"
    return f"{shown}{more}"


def _restore_registries(before: dict, module_key: str) -> str | None:
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
                for k in [k for k in container if k not in saved and _is_test_owned(container[k], module_key)]:
                    del container[k]
                    removed = True
            elif isinstance(container, list):
                keep = [v for v in container if v in saved or not _is_test_owned(v, module_key)]
                if len(keep) != len(container):
                    container[:] = keep
                    removed = True
            else:
                for v in [v for v in container if v not in saved and _is_test_owned(v, module_key)]:
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


def _drive_async(make_coro, backend=None):
    """Run an async body to completion on the backend the run asked for.

    `anyio_backend` carries either a name (`"trio"`) or a name and its options
    (`("asyncio", {"debug": True})`), which is what the suite's own fixture yields. anyio's public
    `run()` is used rather than a reimplementation: it is the same entry point the plugin uses, and it
    is only reached when the suite already depends on anyio.

    Everything else — the overwhelming majority — keeps the plain asyncio path it always had."""
    if backend is None or _FORCE_ASYNCIO:
        return asyncio.run(make_coro())
    name, options = (backend, None)
    if isinstance(backend, (tuple, list)) and backend:
        name = backend[0]
        options = backend[1] if len(backend) > 1 else None
    try:
        import anyio
    except ImportError:  # the fixture named a backend but anyio is gone: asyncio is the best guess
        return asyncio.run(make_coro())
    if not isinstance(name, str):
        return asyncio.run(make_coro())
    try:
        return anyio.run(make_coro, backend=name, backend_options=options or {})
    except TypeError:
        # An option this anyio does not accept (`loop_factory` came late) must not fail the test for
        # a reason the test has nothing to do with; the backend itself is what matters.
        return anyio.run(make_coro, backend=name)


def _test_is_async(node_id: str, style: str) -> bool:
    """Whether the test body is `async def` **and** tiderace is the thing that must await it.

    A `unittest` class drives its own coroutines — `IsolatedAsyncioTestCase.run()` builds the loop and
    calls `asyncSetUp` around the body — so those are never async-driven from here, however the
    collector labelled them."""
    return resolve_target(node_id, style, lenient=True).is_async


async def _invoke_async(node_id: str, style: str, args: dict) -> tuple[str, str]:
    """The async sibling of `_invoke`, with the same runtime-marker fold (TID-51)."""
    outcome, detail = await _invoke_async_body(node_id, style, args)
    return _runtime_outcome(_CURRENT_NODE, outcome, detail)


async def _invoke_async_body(node_id: str, style: str, args: dict) -> tuple[str, str]:
    """The async sibling of `_invoke`: call the test, `await` it if it's a coroutine, and map the same
    outcomes (incl. lazy RichDiff on `AssertionError`). Runs inside the per-test event loop, so it must
    `await` directly — never `asyncio.run` (which can't nest)."""
    node = resolve_target(node_id, style)
    module = node.module
    try:
        if style == "class_method":
            _xunit_class_setup(node.cls)
            instance = node.cls()
            bound = getattr(instance, node.name)
            target = bound
            call_args, request = _with_request(bound, args, node_id, instance)
        else:
            target = node.func
            call_args, request = _with_request(target, args, node_id)
        hooks = _xunit_test_hooks(module, style, node_id, target)
        try:
            _call_hook(*hooks[0]) if hooks[0] else None
            result = target(**call_args)
            if inspect.iscoroutine(result):
                await result
        finally:
            if hooks[1]:
                try:
                    _call_hook(*hooks[1])
                except Exception:  # noqa: BLE001 — teardown must not mask the body's outcome
                    pass
            _test_finalizers(request)  # after the await, or a coroutine's finalizers run before its body
        return "passed", ""
    except AssertionError as exc:
        plain = "".join(traceback.format_exception_only(type(exc), exc))
        rich = _introspect_assertion(exc)
        return "failed", (rich + plain) if rich else plain
    except _SKIP_EXCEPTIONS as exc:
        return "skipped", str(exc)
    except Exception as exc:  # noqa: BLE001 — a body that raises FAILED; it ran and came out wrong
        # pytest reserves `error` for a test it could not attempt — a fixture that raised, a module
        # that would not import — and calls anything the body raises a failure, assertion or not
        # (TID-30, verified against pytest directly). tiderace split on exception type instead, so
        # `raise RuntimeError` reported `error` where pytest reports `failed`. Both are red, but the
        # taxonomy leaked into the reporters and made the two runners impossible to reconcile.
        return "failed", "".join(traceback.format_exception_only(type(exc), exc))


# Per-module static import closure: module_key -> {rel_path, …} (TID-40). Built lazily, memoised for
# the process, and inherited by every forked child.
_IMPORT_CLOSURE: dict[str, frozenset] = {}
_FILE_DEPS: dict[str, tuple[str, ...]] = {}  # per source file: the in-tree files it imports (TID-76)
# The same, carried across runs (TID-82): `path -> [mtime_ns, size, sys.path hash, deps]`, loaded by
# the pool parent before it forks and extended by every worker at teardown. An entry is used only
# when the file is unchanged and the import roots are the ones it was resolved under.
_FILE_DEPS_CACHE: dict[str, list] = {}
_FILE_DEPS_NEW: dict[str, list] = {}  # what this process computed, to be written at teardown
_FILE_DEPS_STATS = {"hits": 0, "parsed": 0}
_RESOLVED: dict[tuple, str | None] = {}  # (dotted, level, importing dir) → file, memoised (TID-76)
# An import statement starts a line, or follows `;` or a compound statement's `:` on one. `yield from`
# and `from_x = ...` do not match. What this finds is parsed as a statement, so names are exact.
_IMPORT_STMT = re.compile(r"(?:^|[;:])[ \t]*(import|from)[ \t]")


def _imported_names(path: str) -> list[tuple[str, int]]:
    """Every module a file imports, as `(dotted_name, relative_level)`.

    AST rather than execution, because that is the whole point: a module's `import` lines run *once*,
    for whichever test happens to be first, so nothing that watches execution can see the imports of
    the nineteen tests that follow. Parsing sees all of them, in any order, every time.

    Parsing a whole file is 1.5ms; a closure walks ~100 of them and every worker walks the closures
    of every module it runs (TID-76). So this parses only the import *statements*: a scan finds the
    lines, each statement is parsed on its own, and the names are exactly what a full parse gives.
    The one thing a line scan cannot tell is whether a line sits inside a string, so a file whose
    candidate import lines fall inside a triple-quoted region takes the full parse instead — exact
    over fast, never a missed import."""
    try:
        with open(path, encoding="utf-8") as fh:
            src = fh.read()
    except (OSError, UnicodeDecodeError):
        return []  # unreadable ⇒ no closure; the runtime footprint still applies
    if "import" not in src:
        return []
    stmts = _scan_import_statements(src)
    if stmts is None:  # a candidate inside a string region: parse the whole file
        try:
            tree = ast.parse(src, filename=path)
        except SyntaxError:
            return []
        return _import_names_in(ast.walk(tree))
    out: list[tuple[str, int]] = []
    for stmt in stmts:
        try:
            out.extend(_import_names_in(ast.parse(stmt).body))
        except SyntaxError:
            continue  # not a statement after all (a comment, a fragment); contributes nothing
    return out


def _scan_import_statements(src: str) -> list[str] | None:
    """The import statements in `src`, each as its own parseable text, or None if any candidate
    lies inside a triple-quoted region (the caller then parses the whole file)."""
    lines = src.split("\n")
    stmts: list[str] = []
    in_string: str | None = None  # the delimiter of the triple-quoted region we are inside, if any
    i, n = 0, len(lines)
    while i < n:
        line = lines[i]
        i += 1
        candidate = "import" in line and _IMPORT_STMT.search(line)
        if candidate and in_string:
            return None
        if candidate:
            m = candidate
            stmt = line[m.start(1):]
            depth = stmt.count("(") - stmt.count(")")
            while (depth > 0 or stmt.rstrip().endswith("\\")) and i < n:
                nxt = lines[i]
                i += 1
                stmt = stmt.rstrip().rstrip("\\") + "\n" + nxt
                depth += nxt.count("(") - nxt.count(")")
            stmts.append(stmt)
        # Track triple-quoted regions after the line's own statement is taken: a docstring that
        # opens and closes on this line leaves the state as it was.
        for tq in ('"""', "'''"):
            if line.count(tq) % 2 == 1:
                if in_string == tq:
                    in_string = None
                elif in_string is None:
                    in_string = tq
    return stmts


def _import_names_in(nodes) -> list[tuple[str, int]]:
    out: list[tuple[str, int]] = []
    for node in nodes:
        if isinstance(node, ast.Import):
            out.extend((a.name, 0) for a in node.names)
        elif isinstance(node, ast.ImportFrom):
            base = node.module or ""
            out.append((base, node.level))
            # `from pkg import mod` may name a submodule rather than an attribute; both resolve
            # harmlessly, and a miss just contributes nothing.
            out.extend((f"{base}.{a.name}" if base else a.name, node.level) for a in node.names)
    return out


def _resolve_module_file(dotted: str, level: int, from_file: str, root: str) -> str | None:
    """The file a dotted import resolves to **inside the suite**, or None if it is external.

    Third-party and stdlib imports are deliberately dropped: a footprint exists to answer "did
    anything this test depends on change in this tree", and site-packages does not change between
    runs of the same checkout.

    Memoised on (name, level, importing directory): the same `import os` or `from pirn.x import y`
    appears in hundreds of files, and each resolution probes every `sys.path` entry (TID-76)."""
    key = (dotted, level, os.path.dirname(from_file) if level else "")
    if key in _RESOLVED:
        return _RESOLVED[key]
    resolved = _RESOLVED[key] = _resolve_module_file_uncached(dotted, level, from_file, root)
    return resolved


def _resolve_module_file_uncached(dotted: str, level: int, from_file: str, root: str) -> str | None:
    if level:  # relative import: resolve against the importing file's package
        base_dir = os.path.dirname(os.path.abspath(from_file))
        for _ in range(level - 1):
            base_dir = os.path.dirname(base_dir)
        candidates = [os.path.join(base_dir, *dotted.split(".")) if dotted else base_dir]
    else:
        candidates = [os.path.join(p, *dotted.split(".")) for p in sys.path if p]
    for stem in candidates:
        for candidate in (stem + ".py", os.path.join(stem, "__init__.py")):
            if os.path.isfile(candidate):
                abs_path = os.path.abspath(candidate)
                # Only what lives under the run root; anything else is not ours to invalidate on.
                if abs_path.startswith(os.path.abspath(root) + os.sep):
                    return abs_path
                return None
    return None


def _file_deps(path: str, root: str) -> tuple[str, ...]:
    """The in-tree files one source file imports, parsed and resolved once per process.

    The closures of different test modules overlap almost entirely — on pirn-core each one walks
    ~100 files, and nearly all of them are the same project files every time. Without this memo
    every module's closure re-parsed and re-resolved all of them: 230ms per module, 575 modules,
    once per worker, which was the whole of coverage's cost on a cold run (TID-76). `root` and
    `sys.path` are fixed for the life of a process, so the key is the file alone."""
    cached = _FILE_DEPS.get(path)
    if cached is None:
        cached = _file_deps_from_cache(path)
        if cached is None:
            deps: dict[str, None] = {}
            for dotted, level in _imported_names(path):
                resolved = _resolve_module_file(dotted, level, path, root)
                if resolved:
                    deps[resolved] = None
            cached = tuple(deps)
            _FILE_DEPS_STATS["parsed"] += 1
            try:
                st = os.stat(path)
                _FILE_DEPS_NEW[path] = [st.st_mtime_ns, st.st_size, _sys_path_key(), list(cached)]
            except OSError:
                pass
        _FILE_DEPS[path] = cached
    return cached


def _sys_path_key() -> str:
    """The import roots a resolution ran under, as one short token: a cached dependency list is only
    right for the `sys.path` that produced it."""
    return hashlib.sha1("\n".join(p for p in sys.path if p).encode("utf-8", "replace")).hexdigest()[:16]


def _file_deps_from_cache(path: str):
    entry = _FILE_DEPS_CACHE.get(path)
    if entry is None:
        return None
    try:
        st = os.stat(path)
    except OSError:
        return None
    mtime_ns, size, key, deps = entry
    if st.st_mtime_ns != mtime_ns or st.st_size != size or key != _sys_path_key():
        return None
    _FILE_DEPS_STATS["hits"] += 1
    return tuple(deps)


def _file_deps_cache_dir(root: str) -> str:
    return os.path.join(os.path.abspath(root), ".tiderace-cache", "file-deps")


def _load_file_deps_cache(root: str) -> None:
    """Read the index and every worker file left by earlier runs, fold them into one index, and
    drop the worker files. Called once per process that serves a run — in the pool that is the
    parent, and the workers inherit the result through the fork (TID-82). Any file that does not
    parse is ignored; a concurrent run can lose an entry, never hand us a corrupt one."""
    d = _file_deps_cache_dir(root)
    try:
        names = os.listdir(d)
    except OSError:
        return
    merged: dict[str, list] = {}
    worker_files = []
    for name in sorted(names):
        if not name.endswith(".json"):
            continue
        full = os.path.join(d, name)
        try:
            with open(full, encoding="utf-8") as fh:
                data = json.load(fh)
            if data.get("v") == 1 and isinstance(data.get("files"), dict):
                merged.update(data["files"])
        except (OSError, ValueError):
            pass
        if name != "index.json":
            worker_files.append(full)
    _FILE_DEPS_CACHE.update(merged)
    if worker_files:
        _write_file_deps_index(d, merged)
        for full in worker_files:
            try:
                os.unlink(full)
            except OSError:
                pass


def _write_file_deps_index(d: str, files: dict) -> None:
    tmp = os.path.join(d, f".index-{os.getpid()}.tmp")
    try:
        os.makedirs(d, exist_ok=True)
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump({"v": 1, "files": files}, fh)
        os.replace(tmp, os.path.join(d, "index.json"))
    except OSError:
        try:
            os.unlink(tmp)
        except OSError:
            pass


def _save_file_deps_cache() -> None:
    """What this process parsed, to its own file under the cache dir; the next run's parent folds it
    in. Nothing to write is nothing written."""
    if not _FILE_DEPS_NEW or not _ROOT:
        return
    d = _file_deps_cache_dir(_ROOT)
    try:
        os.makedirs(d, exist_ok=True)
        tmp = os.path.join(d, f".w-{os.getpid()}.tmp")
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump({"v": 1, "files": _FILE_DEPS_NEW}, fh)
        os.replace(tmp, os.path.join(d, f"w-{os.getpid()}.json"))
    except OSError:
        pass
    if _env("TIDERACE_TIMING"):
        _warn(f"closure cache: {_FILE_DEPS_STATS['hits']} files from cache, "
              f"{_FILE_DEPS_STATS['parsed']} parsed")


def _import_closure(module_key: str, root: str) -> frozenset:
    """Every in-tree file a module transitively imports, plus the conftests above it.

    This is the half of a test's dependency footprint that runtime coverage cannot produce (TID-40).
    Coverage sees a module's imports execute exactly once — for whichever test in it ran first — so
    on a twenty-test module the source under test appeared in one footprint out of twenty, and
    impact selection served the other nineteen from cache after that source changed. It reported a
    green suite that a full run reported as twenty failures.

    Conftests are included because a change to one alters fixtures for everything beneath it, and
    nothing in the runtime footprint necessarily mentions the conftest at all."""
    cached = _IMPORT_CLOSURE.get(module_key)
    if cached is not None:
        return cached
    root_abs = os.path.abspath(root)
    start = os.path.join(root_abs, module_key.replace("/", os.sep))
    seen: set[str] = set()
    queue = [start]
    while queue:
        current = queue.pop()
        for resolved in _file_deps(current, root):
            if resolved not in seen:
                seen.add(resolved)
                queue.append(resolved)
    # Every conftest from the run root down to this module's directory.
    directory = os.path.dirname(start)
    while directory.startswith(root_abs):
        conftest = os.path.join(directory, "conftest.py")
        if os.path.isfile(conftest):
            seen.add(os.path.abspath(conftest))
        if directory == root_abs:
            break
        directory = os.path.dirname(directory)
    closure = frozenset(os.path.relpath(p, root_abs).replace(os.sep, "/") for p in seen)
    _IMPORT_CLOSURE[module_key] = closure
    return closure


class _Coverage:
    """Per-test executed-source capture inside the fork child (ADR-E006, design 11). Uses PEP 669
    `sys.monitoring` on CPython 3.12+, falling back to `sys.settrace` on ≤3.11. Records
    `{rel_source_path: set(line)}` for `.py` files under `root` — the test's dependency footprint the
    impact selection and cache key consume. A no-op when disabled, so the default path is
    byte-identical to before.

    By default the footprint is **file-level**: one `PY_START` event per code object entered (module
    and class bodies are code objects too, so a dynamic import is seen), disabled after its first hit,
    and an empty line list per file — the convention the import closure already uses for "any change
    to this file counts". Nothing on a production path reads a line number (TID-76), so the default
    carries none; `lines=True` (`--coverage-lines`) keeps LINE capture for a consumer that wants it.
    (The cost of capture on a cold run was never the events or the lines — see `_file_deps`.)"""

    _TOOL_ID = 5  # sys.monitoring tool slot (0..5 available); 5 avoids coverage.py/profiler clashes

    def __init__(self, root: str | None, enabled: bool, lines: bool = False):
        self.enabled = enabled and root is not None
        self.lines = lines
        self.root = os.path.abspath(root) if root else ""
        self.touched: dict[str, set] = {}
        self._mon = getattr(sys, "monitoring", None) if self.enabled else None
        self._prev_trace = None
        self._stopped = False  # makes stop() idempotent (called once for the report, once in finally)

    def _want(self, path: str | None) -> bool:
        return bool(path) and path.endswith(".py") and os.path.abspath(path).startswith(self.root)

    def start(self) -> None:
        if not self.enabled:
            return
        if self._mon is not None:
            mon, tid, events = self._mon, self._TOOL_ID, self._mon.events

            def on_line(code, line_no):
                fn = code.co_filename
                if self._want(fn):
                    self.touched.setdefault(os.path.abspath(fn), set()).add(line_no)
                return mon.DISABLE  # per-location disable ⇒ each line fires at most once (cheap)

            def on_start(code, offset):
                fn = code.co_filename
                if self._want(fn):
                    self.touched.setdefault(os.path.abspath(fn), set())
                return mon.DISABLE  # per-code-object disable ⇒ each function fires at most once

            # PY_RESUME as well: a generator or coroutine created by an earlier test (or a fixture)
            # and resumed inside this one never *starts* here, but its file is still one this test
            # ran code in. Both events carry (code, offset) and both are per-code-object.
            file_events = events.PY_START | events.PY_RESUME

            mon.use_tool_id(tid, "tiderace")
            # `DISABLE` is per location and outlives `free_tool_id`; only this clears it. Without it
            # the first test in the process to enter a function is the only one ever credited with
            # its file — every later test in the same worker sees nothing there (TID-76).
            mon.restart_events()
            if self.lines:
                mon.register_callback(tid, events.LINE, on_line)
                mon.set_events(tid, events.LINE)
            else:
                mon.register_callback(tid, events.PY_START, on_start)
                mon.register_callback(tid, events.PY_RESUME, on_start)
                mon.set_events(tid, file_events)
        else:  # ≤3.11 fallback
            want_lines = self.lines

            def tracer(frame, event, arg):
                fn = frame.f_code.co_filename
                if event == "call":
                    if not self._want(fn):
                        return None  # nothing to learn from this frame's lines
                    if not want_lines:
                        self.touched.setdefault(os.path.abspath(fn), set())
                        return None
                elif event == "line" and self._want(fn):
                    self.touched.setdefault(os.path.abspath(fn), set()).add(frame.f_lineno)
                return tracer

            self._prev_trace = sys.gettrace()
            sys.settrace(tracer)

    def stop(self) -> dict:
        if not self.enabled or self._stopped:
            return self._report() if self.enabled else {}
        self._stopped = True
        if self._mon is not None:
            mon, tid = self._mon, self._TOOL_ID
            mon.set_events(tid, 0)
            for event in ((mon.events.LINE,) if self.lines
                          else (mon.events.PY_START, mon.events.PY_RESUME)):
                mon.register_callback(tid, event, None)
            mon.free_tool_id(tid)
        else:
            sys.settrace(self._prev_trace)
        return self._report()

    def _report(self) -> dict:
        # Forward slashes whatever the platform, as the import closure and the Rust side use: on
        # Windows the raw relpath put `src\thing.py` beside the closure's `src/thing.py`, so the
        # runtime half of a footprint never matched a changed file.
        return {os.path.relpath(p, self.root).replace(os.sep, "/"): sorted(lines)
                for p, lines in self.touched.items()}

    def report_with_imports(self, module_key: str) -> dict:
        """The runtime footprint plus the module's static import closure (TID-40).

        The two halves answer different questions and neither is sufficient alone. Coverage says
        what this test *executed*, which is the only way to know it reached a particular branch. The
        closure says what its module *depends on*, which is the only way to know about code that was
        imported before this test ran — i.e. everything, for every test after the first one in the
        module.

        Closure entries carry no line numbers. A footprint's line detail exists to narrow
        invalidation to the lines a test actually ran; for an import dependency there is nothing to
        narrow, and an empty list correctly means "any change to this file counts"."""
        report = self._report()
        if not self.enabled:
            return report
        for rel in _import_closure(module_key, self.root):
            report.setdefault(rel, [])
        return report


class Engine:
    """Parent-side scope state: wider-than-function fixtures live here, inherited by forked children."""

    def __init__(self, reg: Registry, no_fork: bool = False, root: str | None = None,
                 coverage: bool = False, purity_guard: bool = False, restore: bool = False,
                 coverage_lines: bool = False):
        self.reg = reg
        self.no_fork = no_fork  # no-COW fallback path (SubprocessWorker / Windows / --no-fork)
        self.root = root  # corpus root, for coverage path relativization
        self.coverage = coverage  # ADR-E006: capture per-test executed-source footprint
        self.coverage_lines = coverage_lines  # line numbers in the footprint (opt-in, TID-76)
        self.purity_guard = purity_guard  # detect shared-state mutation per test (→ pure-test batching)
        self._leaked = None          # this test's unmodelled state drift, if any (TID-33)
        self._state_disturbed = False  # …and whether the node should be forked from now on
        self._disturbance = None  # what moved, kept for the verdict the clean-room handoff reports
        self._timed_out = False  # the in-process deadline ended the case (TID-93): no re-run
        self.restore = restore  # snapshot/restore shared state around no-fork tests (isolation w/o fork)
        self._module_child = None  # the live child running an opaque module's tests, if any (TID-80)
        self._guard = None  # the in-process module's entry snapshot, restored when we leave it (TID-81)
        self._in_module_child = False  # set in that child: run everything in-process, never fork
        self.active: list[_Active] = []  # in setup order (widest → narrowest)

    def _value(self, name: str, module_key: str):
        # The most-recently set-up active instance of `name` is the one in scope for this test.
        for a in reversed(self.active):
            if a.fdef.name == name:
                return a.value
        raise KeyError(name)

    def _sync_wider(self, closure: list[FixtureDef], node_id: str) -> None:
        _node_for(node_id)  # a wider-scope fixture is built for the test that first needed it
        """Tear down active wider fixtures whose scope-instance no longer matches this test, then set
        up any missing wider fixtures the test needs (each exactly once per scope-instance)."""
        self._teardown_stale(node_id)
        # pytest runs xunit `setup_module` / `setUpModule` as the first module-scoped autouse fixture,
        # so it precedes every module-scoped fixture of the file: a client a fixture builds sees what
        # the hook put in place — a started mock's credentials, a stub in `sys.modules`. It ran on the
        # test's own path here, after the wider fixtures were already live, and a moto mock started
        # in `setup_module` never reached the fixture-built client (TID-79). Before any wider fixture,
        # once per module per process; the later call on the test path is then a no-op.
        _xunit_module_setup(_import_module(_module_key(node_id)))
        # Set up missing wider fixtures in topo order.
        live = {a.key for a in self.active}
        for d in closure:
            if d.rank == 0:
                continue
            key = _instance_key(d, node_id)
            if key in live:
                continue
            mk = _module_key(node_id)
            args = {param: self._value(prov, mk) for param, prov in d.bindings.items()}
            value, gen = _setup_fixture(d, args, None)
            self.active.append(_Active(d, key, value, gen))
            live.add(key)

    def _teardown_stale(self, node_id: str) -> None:
        """Tear down active wider fixtures whose scope-instance no longer matches this test, from the
        narrow end (active is ordered widest → narrowest). Then, if this test is the first of a new
        module, put back what the previous module changed (TID-81) — after its fixtures are gone,
        so a finalizer never runs against restored globals."""
        while self.active:
            top = self.active[-1]
            if top.key == _instance_key(top.fdef, node_id):
                break
            _teardown(top.gen)
            self.active.pop()
        if self._guard is not None and self._guard["module_key"] != _module_key(node_id):
            self._leave_module()

    def _enter_module(self, module_key: str) -> None:
        """Snapshot the module the worker is entering, once, before its first in-process test: its
        globals, `os.environ`, `sys.modules`, the interpreter state the fingerprint watches, and the
        library containers this module's imports reach (TID-81). `_leave_module` restores all of it."""
        if self._guard is not None:
            if self._guard["module_key"] == module_key:
                return
            self._leave_module()
        try:
            mod = _import_module(module_key)
        except Exception:  # noqa: BLE001 — nothing to snapshot; nothing to put back either
            return
        self._guard = {
            "module_key": module_key,
            "module": mod,
            "globals": _snapshot_shared(mod),
            "environ": dict(os.environ),
            "modules": dict(sys.modules),
            "state": _state_fingerprint(),
            "registries": _registry_snapshot(module_key),
        }

    def _leave_module(self) -> None:
        """Restore the entered module's snapshot: the next module on this worker starts from the
        state this one found, whatever its tests did in between (TID-81)."""
        guard, self._guard = self._guard, None
        if guard is None:
            return
        try:
            _restore_shared(guard["module"], guard["globals"], guard["environ"])
        except Exception:  # noqa: BLE001 — a global that will not restore must not take the worker
            pass
        _restore_modules(guard["modules"])
        # A container the library created *during* the module (TID-68) was not in the entry
        # snapshot; found now, it is restored against "empty", which pulls the suite's own entries
        # out and leaves the library's.
        registries = dict(guard["registries"])
        for label, (container, _) in _registry_snapshot(guard["module_key"]).items():
            if label not in registries:
                try:
                    registries[label] = (container, type(container)())
                except Exception:  # noqa: BLE001 — an exotic container stays as it is
                    pass
        _restore_registries(registries, guard["module_key"])
        _restore_state(guard["state"])

    def run(self, node_id: str, style: str, deadline_ms: int, force_no_fork: bool = False,
            trusted_pure: bool = False, recorded_must_fork: bool = False) -> dict:
        # `force_no_fork`: run THIS test in-process (no fork). On a trivial test that is ~90× cheaper than a
        # fork; on a real suite the win is smaller and depends on the parent's size (TID-18, TID-41).
        # The caller asserts it's pure (purity guard); the guard re-checks and flags any escapee.
        global _NODES_RUN
        _NODES_RUN += 1
        module_key = _module_key(node_id)
        # Under a directory whose conftest skipped itself (TID-48): pytest never collects these, so
        # nothing about the node — its class, its marks, its module — may be touched.
        if _is_ignored(os.path.join(_ROOT or ".", module_key)):
            return empty_expansion(node_id)
        # A conftest that did not import (TID-72), before the skip: a directory whose setup is broken
        # is broken for every test in it, and that is an error pytest would have stopped on.
        dir_error = _dir_error(module_key)
        if dir_error is not None:
            return errored(node_id, dir_error)
        dir_skip = _dir_skip(module_key)
        if dir_skip is not None:
            # `skip_origin` names the module that never imported, so the summary can report skips in
            # both dimensions (TID-55): a conftest's `importorskip` skips every test under it, and
            # "578 skipped" next to pytest's "94 skipped" reads as a defect until you can also say
            # how many *modules* those 578 came from. A per-test skip leaves this empty.
            return skipped(node_id, dir_skip, skip_origin=module_key)
        if style in ("inherited_methods", "unresolved_class"):
            return self._run_inherited(node_id, deadline_ms, force_no_fork, trusted_pure,
                                       own_too=style == "unresolved_class",
                                       recorded_must_fork=recorded_must_fork)
        # A `@pytest.fixture` whose name starts with `test` (anyio's `TestAsyncFile.testdata`) is
        # what the regex collector cannot tell from a test; pytest never collects it. Reported as
        # an empty expansion, like a deselected node: absent from the tally (TID-88).
        if self._is_fixture_node(node_id, style):
            return empty_expansion(node_id)
        # Deselected by the project's own `-m` filter (TID-32). Reported as an EMPTY expansion
        # rather than a skip: pytest deselects these, so they must not appear in the tally at all —
        # a skip would be a different, visible outcome.
        # A module that skips at import is skipped under any `-k` or `-m`: pytest's collection
        # skips it before either is consulted. The verdicts below used to come first, so a `-k`
        # that was a definite No at node level (`-k "not unit"` over `tests/unit/`) deselected
        # these where `-k nomatch` — undecided until the import — reported them; the daemon,
        # replaying the skip from its record (TID-102), reported them either way. The import is
        # the first thing now, as it is for pytest.
        try:
            _import_module(module_key)
        except _SKIP_EXCEPTIONS as exc:
            return skipped(node_id, _skip_reason(exc), skip_origin=module_key)
        except Exception:  # noqa: BLE001 — an unimportable module surfaces per node, below
            pass
        # Always, `-k` or not (TID-102): the names `-k` would match against are reported with the
        # result, so the daemon can take the verdict itself next time for a node nothing touched.
        names = _mark_names(node_id, style)
        if _STRICT_MARKS:
            # `--strict-markers`: a mark the project never declared is a typo far more often than an
            # intention, and pytest errors the item rather than running it. Silently ignoring the flag
            # meant `@pytest.mark.slwo` quietly ran a test its author had filtered out (TID-59).
            unknown = sorted(n for n in names if n and n not in _DECLARED_MARKS and n not in _BUILTIN_MARKS)
            if unknown:
                # Everything a plugin registered counts as declared — ask pytest rather than guess.
                # If it cannot tell us, enforce nothing: a false error on a valid mark fails a correct
                # suite, which is worse than missing a typo (TID-60).
                registered = _plugin_marks()
                unknown = [n for n in unknown if n not in registered] if registered is not None else []
            if unknown:
                return errored(node_id, f"{', '.join(unknown)} not found in `markers` configuration option")
        if _MARKER_EXPR is not None and not _MARKER_EXPR(names):
            return empty_expansion(node_id)
        # `-k` (TID-63), decided here when it can be: a No at node level is a No for every case the
        # node could produce, so it is deselected before a fixture is built or a skip mark is read —
        # pytest deselects at collection, and a deselected `@pytest.mark.skip` test is not a skip.
        # An "unknown" is settled per case once the case ids exist, below.
        keyword_verdict = _keyword_verdict(node_id, names, final=False) if _KEYWORD_EXPR is not None else True
        if keyword_verdict is False:
            return empty_expansion(node_id, keywords=_keyword_names(node_id, names))
        try:
            node = resolve_target(node_id, style)
            requested = self._requested(node)
            marks = self._marks(node)
            # Inside the same guard as its siblings. It used to sit outside, so a failure expanding this
            # node's parametrize cases escaped `run()` and killed the whole worker — every other test on
            # it was lost and the run reported `shim closed mid-run` (TID-43). Whatever the next unsafe
            # probe turns out to be, it now costs this node an error rather than costing the worker.
            raw_cases = self._cases(node)
        except _GenerateTestsError as exc:
            return errored(node_id, str(exc))
        except _SKIP_EXCEPTIONS as exc:
            # A module-level `pytest.importorskip` / `pytest.skip(allow_module_level=True)`. Not an
            # `Exception`, so without this it escaped `run()` and took the worker with it (TID-48).
            # `skip_origin`: this module is the unit pytest would have reported one skip for (TID-55).
            return skipped(node_id, _skip_reason(exc), skip_origin=module_key)
        except Exception as exc:  # noqa: BLE001 — import/collection failure for this node
            return errored(node_id, "".join(traceback.format_exception_only(type(exc), exc)))

        # Native marks first, then anything a `@pytest.mark.skip` or a collection hook decided
        # (TID-20). Both short-circuit BEFORE any fixture setup — a test skipped for a missing
        # backend must not pay to build one.
        skip_reason = _skip_decision(marks) or _MARKER_SKIPS.get(node_id)
        # Applied once the case ids exist, below: pytest collects a skip-marked parametrized test
        # as one variant per case and skips each, so `test_lchmod[asyncio]`, `[trio]`, … are what
        # the tally holds — not one un-expanded `test_lchmod` (TID-88). Nothing is set up on the
        # way there: the ids come from the marks and the registry, never from a fixture. And while
        # `-k` is still undecided (TID-63) the skip waits for the same ids: pytest deselects at
        # collection, before it reads a skip mark, so a skip-marked test `-k` does not select is
        # absent from the tally rather than a skip in it.

        # Split requested params: fixtures (resolved by the graph) vs. bare params filled positionally
        # by @tiderace.cases. Without this, a parametrized test's params look like missing fixtures.
        #
        # A name the parametrize supplies is NOT a fixture request, even when a fixture of that name
        # exists: pytest's rule is that direct parametrization wins, and the value the author wrote
        # beside the test is the one that runs (TID-57). The collision is easy to hit — `history`,
        # `client`, `config` are ordinary words — and it only appears once the run root is wide enough
        # to have discovered the other module's fixture, so the same test passes on a narrow root and
        # errors on the whole package.
        parametrized = {name for case, *_ in raw_cases if isinstance(case, dict) for name in case}
        indirect = set(self._indirect(node))
        # A parametrized name that is not one of the function's parameters but names a fixture —
        # anyio's `@pytest.mark.parametrize("anyio_backend", ["asyncio"])` on a test that takes no
        # argument — sets that fixture's `request.param`: pytest routes it as an indirect
        # parametrize when the fixture is in the closure, and errors otherwise. Inferred here so
        # the closure is built with it, confirmed against the closure below (TID-88).
        inferred = {n for n in parametrized if n not in requested and self.reg.is_provider(n)}
        indirect |= inferred
        parametrized -= indirect  # indirect values go to the fixture, not the test
        fixture_requested = {
            p: t for p, t in requested.items()
            if p not in parametrized and self.reg.is_provider(t)
        }
        case_params = [p for p in requested if p not in fixture_requested]
        # `@tiderace.cases` yields positional variants; `@pytest.mark.parametrize`
        # yields name→value maps (argnames need not follow the signature order).
        case_kwargs_list = [
            c if isinstance(c, dict) else dict(zip(case_params, c.values))
            for c, *_ in raw_cases
        ] or [{}]
        # Author-supplied ids, aligned with `case_kwargs_list`; `None` ⇒ generate one. And each
        # value's position in its own parametrize axis, for the generated ids (TID-86).
        case_ids = [cid for _, cid, *_ in raw_cases] or [None]
        case_pos_maps = [(rest[0] if rest else None) for _, _, *rest in raw_cases] or [None]


        uses = self._uses(node)  # @tiderace.uses: set up by type, not injected (B2)
        # `@pytest.mark.usefixtures("a", "b")` — on the function, its class or its module — sets those
        # fixtures up around the test without passing them (TID-86). click's shell-completion tests
        # snapshot and restore a registry through exactly this, and without it the registry entry a
        # test adds is still there for the next.
        uses = list(uses) + [
            name for mark in _pytest_markers(node)
            if getattr(mark, "name", "") == "usefixtures"
            for name in getattr(mark, "args", ()) if isinstance(name, str) and name not in uses
        ]
        # A marker can imply a fixture request. `@pytest.mark.anyio` means "run me on the backends
        # `anyio_backend` describes" — the anyio plugin wires that up, and a test never names the
        # fixture itself. Adding it to the closure is enough to get the expansion: `anyio_backend` is
        # an ordinary parametrised fixture, so the combos below turn one test into one per backend,
        # with the suite's own ids. Without it each test ran once, silently covering one backend
        # where its author asked for three (TID-54).
        # Async tests only: the marker parametrises *how a coroutine is run*, so a synchronous test
        # in an anyio-marked module is one test, not one per backend — which is how pytest collects
        # it too.
        if (node.is_async and "anyio" in names
                and self.reg.is_provider("anyio_backend") and "anyio_backend" not in uses
                and "anyio_backend" not in requested and "anyio_backend" not in parametrized):
            uses = list(uses) + ["anyio_backend"]
        closure = _closure(self.reg, module_key, fixture_requested, uses, self._test_classes(node))
        if inferred:
            # Not in the closure after all: pytest reports "function uses no argument"; here the
            # value reaches the test as a keyword it never declared, which fails the same way.
            present = {d.name for d in closure}
            indirect -= {n for n in inferred if n not in present}
        # A fixture the test parametrizes *indirectly* takes the case's value as `request.param`;
        # its own `params` do not fan out as well — pytest yields `test[asyncio]` for an
        # `indirect=True` parametrize of `anyio_backend`, not one case per backend times one (TID-86).
        parametrized = [d for d in closure if d.params and d.name not in indirect]
        if parametrized:
            axes = [
                [(d.name, _param_value(p), _fixture_param_id(d, i, p), i)
                 for i, p in enumerate(d.params)]
                for d in parametrized
            ]
            product = list(itertools.product(*axes))
            combos = [{n: v for n, v, _, _ in c} for c in product]
            # Aligned with `combos`: the author's id per axis, or None where one must be generated,
            # and each value's position in its own axis — what pytest numbers an unprintable value
            # by (`bucket0-trio`, not the case's position across the product) (TID-86).
            combo_id_maps = [{n: i for n, _, i, _ in c} for c in product]
            combo_pos_maps = [{n: pos for n, _, _, pos in c} for c in product]
        else:
            combos = [{}]
            combo_id_maps = [{}]
            combo_pos_maps = [{}]

        outcomes: list[tuple[str, str]] = []
        coverage: dict[str, set] = {}  # union of touched lines across all variants of this node
        impurity = None  # first impurity reason across variants (any impure ⇒ the node is impure)
        node_pure = None  # tri-state across variants: None (unmeasured), True (all measured pure), False
        node_must_fork = False  # any case disturbed interpreter state ⇒ fork the whole node (TID-33)
        # `--strategy subprocess` runs in-process by configuration; there the restore is the remedy
        # and there is nothing better to hand the node to.
        force_no_fork_only = self.no_fork
        # Per-variant results, reported alongside the aggregate (TID-25). Each case already gets its
        # own `_fork_run`, so collapsing them to one outcome discarded results that had already been
        # paid for — the tally lost the passes, and a node with several failures kept one detail.
        parametrized_node = bool(combos != [{}] or case_kwargs_list != [{}])
        variants: list[dict] = []
        # Ids are computed for the WHOLE node up front: pytest indexes every member of a colliding
        # group, which cannot be decided while walking the variants one at a time.
        specs = [
            (combo, combo_ids, case_pos, case_kwargs, combo_pos)
            for combo, combo_ids, combo_pos in zip(combos, combo_id_maps, combo_pos_maps)
            for case_pos, case_kwargs in enumerate(case_kwargs_list)
        ]
        variant_ids = [
            # Brackets whenever the node IS parametrized, even when the id text is empty: a case
            # whose only value is `""` is `test_x[]` in pytest, which is not the same as an
            # unparametrized `test_x`.
            f"{node_id}[{text}]" if parametrized_node else node_id
            for text in _disambiguate([
                _variant_parts(combo, combo_ids, case_kwargs, i, case_ids[case_pos],
                               combo_pos, case_pos_maps[case_pos])
                for i, (combo, combo_ids, case_pos, case_kwargs, combo_pos) in enumerate(specs)
            ])
        ]
        # The cases `-k` keeps (TID-63): every one when the node was already a Yes, else each case
        # judged on its full id — `test_x[1-a]` is what `-k 1-a` was written to name.
        selected = set(range(len(variant_ids)))
        if keyword_verdict is None:
            selected = {i for i, vid in enumerate(variant_ids)
                        if _keyword_verdict(vid, names, final=True)}
            if not selected:
                return empty_expansion(node_id, keywords=_keyword_names(node_id, names))
        if skip_reason is not None:  # the skip deferred above, one per selected variant (TID-88)
            if not parametrized_node:
                return skipped(node_id, skip_reason, keywords=_keyword_names(node_id, names))
            return skipped(node_id, skip_reason, keywords=_keyword_names(node_id, names),
                           variants=[variant(vid, Outcome.SKIPPED, skip_reason, 0,
                                             keywords=_keyword_names(vid, names))
                                     for i, vid in enumerate(variant_ids) if i in selected])
        # Only now — after `-k` has chosen and a whole-node skip has returned — does the node's
        # *route* get decided (TID-99). It used to sit above the case expansion, so every node
        # `-k` was about to deselect first paid the restorability snapshot (a deepcopy of its
        # module's shared state), and one in an opaque module was forked into a module child
        # only to be deselected there. Nothing below the old position set anything up: the
        # closure, the combos and the ids are graph and registry reads.
        # Soundness gate for BOTH in-process paths. A module whose shared state we can't snapshot/restore
        # (opaque globals — an open file, a generator, a live socket) must fork: running it in-process
        # leaks whatever the test mutated into the next test on the same module.
        #
        # This applies to `--no-fork` mode too, not just an optimistic per-test request. It previously
        # checked only `force_no_fork`, so whole-run no-fork (`--no-fork`, i.e. the SubprocessWorker /
        # Windows path) ran opaque modules in-process regardless and silently produced wrong results —
        # e.g. a module-level generator stayed advanced across tests. See
        # `py-tiderace/proof_windows_opaque_fork.py`.
        #
        # A trusted-pure test skips the check: known pure ⇒ it won't mutate, so restorability is moot.
        must_fork = False
        if self.restore and not trusted_pure and (force_no_fork or self.no_fork):
            try:
                must_fork = not _restorable(_import_module(module_key))
            except Exception:  # noqa: BLE001 — can't import/inspect ⇒ be safe, fork
                must_fork = True
            if must_fork:
                force_no_fork = False
        # A recorded state-disturber (TID-33) is denied the in-process tier the same way, and takes
        # the same route as an opaque module below (TID-96). It used to fall through to a fork per
        # test, each child a fresh process in which a unittest class's `setUpClass` had not run —
        # pirn-agents' cross-process replay class paid its 5s set-up seven times, where pytest and
        # the module child pay it once. With the ladder off (`TIDERACE_FORCE_FORK=1`) nothing is
        # recorded as a disturber, so that mode keeps its fork per test.
        if recorded_must_fork and self.restore and not self.no_fork:
            must_fork = True
            force_no_fork = False
        # And a module child already open for this file takes the rest of the file: only the
        # method that left the thread behind is recorded, its siblings arrive unflagged, and sending
        # them back to the worker would run them in a second process — a unittest class's
        # `setUpClass` a second time. TID-80 made the child the boundary between modules; a file
        # whose tests are split across two processes is what pytest never does (TID-96).
        if (self._module_child is not None and not self._in_module_child and _FORK_AVAILABLE
                and self._module_child.module_key == module_key and self.restore
                and not self.no_fork):
            must_fork = True
            force_no_fork = False
        # An opaque module's tests run in ONE forked child, sequentially, for as long as the batch
        # stays on that module (TID-80). Forking per test kept the module's own tests apart, which
        # pytest never does: an object one test put into a module-scoped moto mock was gone for the
        # next, because it lived and died in that test's child. The child is the isolation boundary
        # between modules, which is what opacity is about; inside it the file behaves as under
        # pytest. Fewer forks, too.
        if must_fork and _FORK_AVAILABLE and not self._in_module_child:
            return self._module_child_run(node_id, style, deadline_ms)
        variant_index = 0
        per_combo = len(case_kwargs_list)
        for combo, combo_ids in zip(combos, combo_id_maps):
            if not any(i in selected for i in range(variant_index, variant_index + per_combo)):
                variant_index += per_combo  # nothing here survives `-k`: build none of its fixtures
                continue
            try:
                self._sync_wider(closure, node_id)
            except BaseException as exc:  # noqa: BLE001
                # A fixture that cannot be set up is an ordinary condition — pytest errors that test
                # and carries on. Letting it escape here killed the whole worker: every *other* test
                # on it was lost, and the run reported `shim closed mid-run`, naming the transport
                # rather than the fixture (TID-34). Same lesson as TID-15, one level up.
                return errored(node_id, "error setting up fixtures: "
                               + "".join(traceback.format_exception_only(type(exc), exc)))
            for case_pos, case_kwargs in enumerate(case_kwargs_list):
                if variant_index not in selected:
                    variant_index += 1  # deselected by `-k`: absent from the tally, as in pytest
                    continue
                started = time.perf_counter()
                self._state_disturbed = False
                self._disturbance = None
                self._timed_out = False
                # `indirect=` routes a case's value to the *fixture* of that name, as `request.param`,
                # and the test receives whatever the fixture returns (TID-58). The per-fixture param
                # map is what `combo` already is, so an indirect value simply joins it — and must be
                # kept out of the test's own kwargs, or the raw value would shadow the fixture's.
                case_combo, test_kwargs = combo, case_kwargs
                if indirect and case_kwargs:
                    routed = {k: v for k, v in case_kwargs.items() if k in indirect}
                    if routed:
                        case_combo = {**combo, **routed}
                        test_kwargs = {k: v for k, v in case_kwargs.items() if k not in indirect}
                oc, detail, cov, purity = self._fork_run(
                    node_id, style, fixture_requested, closure, case_combo, deadline_ms, test_kwargs,
                    force_no_fork, trusted_pure, must_fork, variant_ids[variant_index])
                # Per case, because only some cases of a parametrized node may trip (TID-33).
                disturbed = self._state_disturbed
                node_must_fork = node_must_fork or disturbed
                if parametrized_node:
                    case = variant(variant_ids[variant_index], oc, detail,
                                   int((time.perf_counter() - started) * 1000),
                                   keywords=_keyword_names(variant_ids[variant_index], names))
                    if cov:
                        case["coverage"] = {p: sorted(l) for p, l in cov.items()}
                    with_purity(case, purity)
                    if disturbed:
                        case["must_fork"] = True
                    variants.append(case)
                variant_index += 1
                outcomes.append((oc, detail))
                for path, lines in cov.items():
                    coverage.setdefault(path, set()).update(lines)
                if purity is _UNKNOWN_PURITY:
                    continue  # this variant measured nothing — leave the node verdict as-is
                if purity is None:  # measured pure
                    if node_pure is None:
                        node_pure = True
                else:  # measured impure
                    node_pure = False
                    if impurity is None:
                        impurity = purity
        outcome, detail = _aggregate(outcomes)
        outcome, detail = _apply_xfail(marks, outcome, detail)
        # pytest's own `@pytest.mark.xfail` / `skip`, which `_apply_xfail` above does not see: it
        # reads tiderace's native marks. Without this a test the author marked as expected-to-fail
        # was reported as a failure — one of click's two remaining divergences (TID-63).
        outcome, detail = _fold_pytest_marks(_pytest_markers(node), outcome, detail)
        resp = response(node_id, outcome, detail=detail, keywords=_keyword_names(node_id, names))
        # Additive and omitted for an unparametrized node, so its frame stays byte-identical.
        if variants:
            resp["variants"] = variants
        if coverage:  # additive field (Phase-3 CONTRACT §6); omitted when capture is off/empty
            resp["coverage"] = {path: sorted(lines) for path, lines in coverage.items()}
        if node_pure is not None:  # additive: purity was measured (guard or restore) — record the verdict
            resp["pure"] = node_pure
            if impurity is not None:
                resp["impurity"] = impurity
        if node_must_fork:  # additive; omitted for the overwhelming majority that never trip
            resp["must_fork"] = True
        # A node that disturbed interpreter state has an in-process result nobody should trust, and
        # this process is no longer a safe thing to fork. Re-run it in the clean room and report that
        # instead — the pristine image is the only place the answer is both correct and reachable
        # without deadlocking (TID-50).
        # …unless the deadline is what ended it (TID-93): a re-run would block again, cost a second
        # deadline, and replace the timeout's own message with the child path's; the error stands,
        # and the must-fork verdict is what changes the next run.
        if (node_must_fork and _CLEAN_ROOM is not None and not self.no_fork
                and not force_no_fork_only and not self._timed_out):
            _warn(f"re-running {node_id} from a clean image — it disturbed interpreter state")
            clean = _clean_room_run(node_id, style, deadline_ms)
            if clean is not None:
                clean["must_fork"] = True
                # The clean run cannot observe what the first attempt did, and the verdict is about
                # the test, not about where it finally ran: it disturbed state, so it is impure and
                # must not take the in-process path again.
                clean["pure"] = False
                if self._disturbance:
                    clean["impurity"] = f"disturbed interpreter state: {self._disturbance}"
                return _note_import_history(clean, pristine=True)
        return _note_import_history(resp)

    def _run_inherited(self, node_id: str, deadline_ms: int, force_no_fork: bool,
                       trusted_pure: bool, own_too: bool = False,
                       recorded_must_fork: bool = False) -> dict:
        """Run the test methods a class INHERITS rather than defines (TID-26).

        Collection scans source text, so `class TestKuzuConformance(GraphStoreConformance)` looks
        like a class with no tests — on a real corpus that silently dropped 129 tests, every backend
        conformance suite among them, and the run stayed green. Only something holding the live class
        can see through to the base, so the shim resolves it here and reports one result per method.

        Methods defined in the class's OWN body are excluded by default: the source scan already
        collected those, and running them here too would double-count them. `own_too` inverts that
        for a class the scan did not recognise at all (`unresolved_class`), where it collected
        nothing and this is the only report of the class's tests."""
        module_key = _module_key(node_id)
        cls_name = node_id.partition("::")[2]
        try:
            module = _import_module(module_key)
            cls = getattr(module, cls_name)
        except Exception as exc:  # noqa: BLE001 — a class we can't resolve contributes nothing
            return expansion(node_id, Outcome.ERROR,
                             "".join(traceback.format_exception_only(type(exc), exc)), [])

        # pytest's rule: a `Test*` class, or any `unittest.TestCase` subclass whatever its name.
        # `PackOverridesBuiltinTests` is the second kind, which is why the name scan missed it.
        if own_too and not (
            cls.__name__.startswith("Test") or issubclass(cls, unittest.TestCase)
        ):
            return empty_expansion(node_id)

        own = set() if own_too else set(vars(cls))
        inherited = sorted(
            name for name in dir(cls)
            if name.startswith("test") and name not in own and callable(getattr(cls, name, None))
        )
        # `expanded` says "these variants are the whole answer", so an empty list means this class
        # contributes nothing — distinct from a node that simply isn't parametrized.
        if not inherited:
            return empty_expansion(node_id)

        style = "unittest_method" if issubclass(cls, unittest.TestCase) else "class_method"
        variants = []
        for name in inherited:
            child = f"{module_key}::{cls_name}::{name}"
            started = time.perf_counter()
            res = self.run(child, style, deadline_ms, force_no_fork, trusted_pure, recorded_must_fork)
            # A parametrized inherited method expands again; splice its cases in rather than nesting.
            if res.get("variants"):
                variants.extend(res["variants"])
                continue
            # A child that expanded to nothing was deselected — `-m` or `-k` said no, and `run()`
            # answered with the empty expansion pytest's absence-from-the-tally means. Building a
            # variant from that answer's placeholder outcome reported every deselected inherited
            # method as a pass that never ran: 101 of them on pirn-agents under `-k nomatch` (TID-74).
            if res.get("expanded"):
                continue
            child_result = variant(child, res["outcome"], res.get("detail", ""),
                                   int((time.perf_counter() - started) * 1000))
            if res.get("coverage"):
                child_result["coverage"] = res["coverage"]
            if "pure" in res:
                child_result["pure"] = res["pure"]
            if res.get("must_fork"):
                child_result["must_fork"] = True
            if res.get("keywords"):
                child_result["keywords"] = res["keywords"]
            variants.append(child_result)
        # Every child deselected ⇒ the class contributes nothing, exactly as an inherited-nothing
        # class does above. `_aggregate` of an empty list is `max()` of nothing, and that exception
        # escaping `run()` took the whole worker down — "shim closed mid-run" for a `-k` that matched
        # no inherited method (TID-74, the shape TID-43 was about).
        if not variants:
            return empty_expansion(node_id)
        worst_outcome, worst_detail = _aggregate([(v["outcome"], v.get("detail", "")) for v in variants])
        return expansion(node_id, worst_outcome, worst_detail, variants)

    # ------------------------------------------------------------------ module child (TID-80)
    def _module_child_run(self, node_id: str, style: str, deadline_ms: int) -> dict:
        """Run this node in the live child for its module, forking one if there is none (or the live
        one serves another module). Everything the child does not report is reported here: a death
        names its exit, a hang its timeout, and either drops the child so the next node gets a fresh
        one rather than a dead pipe."""
        module_key = _module_key(node_id)
        child = self._module_child
        if child is not None and child.module_key != module_key:
            self._module_child_close()
            child = None
        if child is None:
            # Stale wider fixtures go before the fork, in the process that owns them: the child must
            # never tear down what the parent will tear down again.
            self._teardown_stale(node_id)
            child = self._module_child_spawn(module_key)
        try:
            _write_frame(child.req_w, {"node_id": node_id, "style": style, "deadline_ms": deadline_ms})
        except OSError:
            status = self._module_child_reap()
            return errored(node_id, "the module's child process was gone before this test could be "
                           f"sent to it ({_exit_text(status)})")
        data, timed_out = _read_frame_by(child.resp_r, time.monotonic() + deadline_ms / 1000.0)
        if timed_out:
            self._module_child_kill()
            return errored(node_id, "timeout")
        if data is None:  # EOF without a frame: the child died on this test
            status = self._module_child_reap()
            return errored(node_id, f"the module's child process died running this test "
                           f"({_exit_text(status)}); the module's remaining tests run in a fresh one")
        try:
            return json.loads(data.decode())
        except (ValueError, UnicodeDecodeError) as exc:
            self._module_child_kill()
            return errored(node_id, f"child sent an unreadable result frame ({exc}); "
                           f"{len(data)} bytes received")

    def _module_child_spawn(self, module_key: str):
        req_r, req_w = os.pipe()
        resp_r, resp_w = os.pipe()
        pid = os.fork()
        if pid == 0:  # ---- CHILD: this module's tests, in-process, until the parent closes the pipe
            os.close(req_w)
            os.close(resp_r)
            self._in_module_child = True
            self.restore = False  # pytest's semantics inside the file: nothing is undone between tests
            self.purity_guard = False
            self._module_child = None
            inherited = len(self.active)  # the parent's fixtures: its to tear down, not ours
            done_before = set(_XUNIT_DONE)  # likewise the parent's xunit hooks
            code = 0
            try:
                while True:
                    req = _read_frame(req_r)
                    if req is None:
                        break
                    try:
                        resp = self.run(req["node_id"], req["style"], req.get("deadline_ms", 5000),
                                        force_no_fork=True)
                    except BaseException as exc:  # noqa: BLE001 — report it; never die silently
                        resp = errored(req["node_id"], _child_fault_detail(exc)[:4000])
                    _write_frame(resp_w, resp)
            except BaseException:  # noqa: BLE001 — an unsendable frame or a closed parent
                code = _EXIT_UNREPORTABLE
            finally:
                try:
                    while len(self.active) > inherited:
                        _teardown(self.active.pop().gen)
                    for key in done_before:
                        _XUNIT_DONE.discard(key)
                    _xunit_class_teardown()
                    _xunit_module_teardown()
                except BaseException:  # noqa: BLE001 — a teardown fault must not mask the results
                    pass
                os._exit(code)
        os.close(req_r)
        os.close(resp_w)
        self._module_child = _ModuleChild(module_key, pid, req_w, resp_r)
        return self._module_child

    def _module_child_close(self) -> None:
        """End the live child gracefully: EOF on its request pipe, its teardown, its exit."""
        child = self._module_child
        if child is None:
            return
        try:
            os.close(child.req_w)
        except OSError:
            pass
        deadline_at = time.monotonic() + 30.0
        while time.monotonic() < deadline_at:
            pid, status = os.waitpid(child.pid, os.WNOHANG)
            if pid:
                break
            time.sleep(0.01)
        else:
            try:
                os.kill(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(child.pid, 0)
        try:
            os.close(child.resp_r)
        except OSError:
            pass
        self._module_child = None

    def _module_child_kill(self) -> int:
        child = self._module_child
        if child is None:
            return 0
        try:
            os.kill(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        return self._module_child_reap()

    def _module_child_reap(self) -> int:
        child = self._module_child
        if child is None:
            return 0
        _, status = os.waitpid(child.pid, 0)
        for fd in (child.req_w, child.resp_r):
            try:
                os.close(fd)
            except OSError:
                pass
        self._module_child = None
        return status

    def _fork_run(self, node_id, style, requested, closure, combo, deadline_ms, case_kwargs=None,
                  force_no_fork=False, trusted_pure=False, must_fork=False, variant_id=None) -> tuple:
        """Run one (combo, case) variant; returns `(outcome, detail, coverage, purity)` where purity is a
        reason string (impure), `None` (measured pure), or `_UNKNOWN_PURITY` (not measured). `force_no_fork`
        runs it in THIS process (the pure-test fast path) without forking; `trusted_pure` additionally
        skips the snapshot (bare no-fork). `must_fork` means the caller determined the module is not
        snapshot-restorable, so isolation REQUIRES a fork — it overrides both in-process paths."""
        case_kwargs = case_kwargs or {}

        # No fork on this platform (Windows) and the module needs one to be isolated. Refuse rather
        # than run it: in-process would leak un-restorable state into the next test on this module, and
        # a wrong green is worse than a reported error. Previously this fell through to `os.fork()` and
        # raised an uncaught AttributeError, killing the worker.
        if must_fork and not _FORK_AVAILABLE:
            return ("error",
                    f"cannot isolate {node_id}: its module has state that can't be snapshot-restored, "
                    f"so it requires fork() — unavailable on this platform. Make the module's globals "
                    f"deep-copyable, or mark the test pure if it doesn't mutate shared state.",
                    {}, _UNKNOWN_PURITY)

        if (self.no_fork or force_no_fork) and not must_fork:
            # No-COW fallback: run the test in THIS process (no isolation, but the same fixture
            # engine → result-identical outcomes; §8 boundary 3). Function fixtures are set up and
            # torn down per test in-process; wider scopes still live once in the parent.
            self._leaked = None
            try:
                # The deadline holds here too (TID-93): a forked child is killed when it overruns,
                # but a test that blocks on this tier used to block the worker, and the run.
                with _in_process_deadline(deadline_ms):
                    result = self._child_exec(node_id, style, requested, closure, combo, case_kwargs,
                                              variant_id=variant_id,
                                              in_process=True, trusted_pure=trusted_pure)
            except _InProcessTimeout as exc:
                # The test was interrupted mid-body: whatever it held is not torn down, so this
                # process is not to be trusted with the next in-process test — the node forks from
                # now on (TID-33's must-fork), where the deadline can kill instead of interrupt.
                self._state_disturbed = True
                self._disturbance = str(exc)
                self._timed_out = True
                return "error", str(exc), {}, _UNKNOWN_PURITY
            except BaseException as exc:  # noqa: BLE001 — any in-process test error → Outcome::Error
                return "error", "".join(traceback.format_exception_only(type(exc), exc)), {}, _UNKNOWN_PURITY
            # `_child_exec` sets this when the fingerprint moved in a way nothing undid (TID-33), so
            # the in-process result cannot be trusted and neither can this process. Re-run the test
            # in a fork — a pristine copy — and report THAT, which fixes the current run rather than
            # only teaching the next one. `_FORK_AVAILABLE` is false on Windows, where there is
            # nothing better to fall back to, so the in-process answer stands there. Neither is
            # `--strategy subprocess`, where running in-process is the configured strategy rather
            # than this run's optimistic guess: there the restore above is the whole remedy.
            drift, self._leaked = self._leaked, None
            if drift is not None:
                # Recorded even on the tiers that cannot act on it now (`--strategy subprocess`,
                # Windows): the fact is true about the test, and a later run under the ladder is
                # exactly who needs it. Note this is NOT the `must_fork` parameter above, which says
                # the test's *module* is unrestorable; this says the test disturbed the interpreter.
                self._state_disturbed = True
                self._disturbance = drift
            if drift is not None and _CLEAN_ROOM is not None and not must_fork and not self.no_fork:
                # The clean room re-runs the whole node from a pristine image; `run()` above does the
                # handoff and reports THAT. Forking here would fork the process this test just
                # dirtied — if what it leaked was a thread, straight into a deadlock (TID-50).
                return result
            if drift is not None and _FORK_AVAILABLE and not must_fork and not self.no_fork:
                _warn(f"re-running {node_id} in a fork — it {drift}")
                oc, detail, cov, _ = self._fork_run(
                    node_id, style, requested, closure, combo, deadline_ms, case_kwargs,
                    force_no_fork=False, trusted_pure=False, must_fork=True, variant_id=variant_id)
                # Keep the impurity verdict: the point is that this node must not take the
                # in-process path again, and the forked run cannot observe what the first one did.
                return oc, detail, cov, f"disturbed interpreter state: {drift}"
            return result

        read_fd, write_fd = os.pipe()
        pid = os.fork()
        if pid == 0:  # ---- CHILD: pristine COW copy with all wider fixtures already warm ----
            os.close(read_fd)
            try:
                outcome, detail, coverage, purity = self._child_exec(
                    node_id, style, requested, closure, combo, case_kwargs, variant_id=variant_id)
                payload = {"outcome": outcome, "detail": detail[:4000]}
                if coverage:
                    payload["coverage"] = coverage
                # Carry the purity tri-state across the pipe: pure=True/False when measured (guard on),
                # omitted when unknown (the default forked path measures nothing).
                with_purity(payload, purity, reason_key="impurity")
            except BaseException as exc:  # noqa: BLE001 — report it; never die silently (TID-15)
                # `_invoke` guards the test BODY only, so anything raised by fixture setup/teardown,
                # the coverage probe, or the purity snapshot lands here. Swallowing it exited 0 with an
                # empty pipe, and the parent could say no more than "no result from child" — a defect
                # indistinguishable, from the outside, from a test that genuinely failed. Send the
                # traceback back instead so the failure names its own cause.
                payload = {"outcome": "error", "detail": _child_fault_detail(exc)[:4000]}
            try:
                os.write(write_fd, json.dumps(payload).encode())
            except BaseException:  # noqa: BLE001 — payload itself is unsendable
                # Serialising or writing the frame failed (an unserialisable coverage map, a closed
                # pipe). Exit non-zero so the parent reports `child exited N` against a documented
                # code rather than the bare generic; a silent 0 would look like a lost result.
                try:
                    os.close(write_fd)
                except BaseException:  # noqa: BLE001
                    pass
                os._exit(_EXIT_UNREPORTABLE)
            os.close(write_fd)
            os._exit(0)

        os.close(write_fd)
        # The deadline covers the WHOLE exchange, not just the first byte (TID-31). `select` used to
        # guard only the initial wait; a child that wrote part of its frame and then hung satisfied
        # it, and the parent blocked in `os.read` forever — taking the worker, and every remaining
        # test in its batch, with it, silently. A frame larger than the 64 KB pipe buffer (a long
        # traceback, a rich diff, a wide coverage map) is written across several `write` calls, so
        # this is reachable rather than theoretical.
        deadline_at = time.monotonic() + deadline_ms / 1000.0
        data = b""
        timed_out = False
        while True:
            remaining = deadline_at - time.monotonic()
            if remaining <= 0:
                timed_out = True
                break
            ready, _, _ = select.select([read_fd], [], [], remaining)
            if not ready:
                timed_out = True
                break
            chunk = os.read(read_fd, 65536)
            if not chunk:
                break  # EOF: the child closed the pipe, so the frame is whatever we have
            data += chunk
        if timed_out:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
            os.close(read_fd)
            if data:
                # Distinct from a silent timeout on purpose: a child that produced half a frame is a
                # different fault from one that produced nothing, and saying which is the whole
                # point of TID-15.
                return ("error",
                        f"timeout after writing {len(data)} bytes of a partial result frame — the "
                        f"child began reporting and then stopped", {}, _UNKNOWN_PURITY)
            return "error", "timeout", {}, _UNKNOWN_PURITY
        os.close(read_fd)
        _, status = os.waitpid(pid, 0)
        if not data:
            if os.WIFSIGNALED(status):
                return "error", f"child killed by signal {os.WTERMSIG(status)}", {}, _UNKNOWN_PURITY
            code = os.WEXITSTATUS(status) if os.WIFEXITED(status) else None
            if code == _EXIT_UNREPORTABLE:
                return ("error",
                        "child ran the test but could not serialise its result frame — the outcome is "
                        "lost. Most likely an unserialisable coverage map or a detail string that is "
                        "not valid JSON.", {}, _UNKNOWN_PURITY)
            if code:
                return "error", f"child exited {code}", {}, _UNKNOWN_PURITY
            # The child now reports its own faults (TID-15), so reaching here means it left without
            # running the handler at all — `os._exit`/`os.abort` from inside the test, or the runtime
            # dying between fork and the first frame.
            return ("error",
                    "child exited 0 without sending a result — the test process terminated itself "
                    "(os._exit/os.abort) or the interpreter died before the result frame was written.",
                    {}, _UNKNOWN_PURITY)
        try:
            res = json.loads(data.decode())
        except (ValueError, UnicodeDecodeError) as exc:
            # A truncated or corrupt frame (child killed mid-write) must stay a reported error — letting
            # it raise here would take the worker down with it and lose the whole batch, not one test.
            return ("error",
                    f"child sent an unreadable result frame ({exc}); {len(data)} bytes received",
                    {}, _UNKNOWN_PURITY)
        # Reconstruct the purity tri-state from the pipe (`purity_from`: omitted ⇒ unknown).
        return res["outcome"], res.get("detail", ""), res.get("coverage", {}), purity_from(res)

    def _child_exec(self, node_id, style, requested, closure, combo, case_kwargs=None, variant_id=None,
                    in_process=False, trusted_pure=False) -> tuple:
        """In the forked child: set up function-scope fixtures (incl. parametrized + reinit-after-fork
        resources, which thus get a FRESH handle per child), run the body, tear down in reverse.
        `case_kwargs` are the @tiderace.cases values bound to the test's bare params. Returns
        `(outcome, detail, coverage)` where coverage is `{rel_path: [lines]}` (empty unless enabled)."""
        module_key = _module_key(node_id)
        local: dict[str, object] = {}
        gens: list = []

        def value_of(name: str):
            if name in local:
                return local[name]
            return self._value(name, module_key)

        cov = _Coverage(self.root, self.coverage, self.coverage_lines)
        cov.start()  # capture the per-test footprint: fixture setup + body, this test only (ADR-E006)
        # B5: async test body or any function-scope async provider ⇒ run setup+body+teardown on ONE
        # event loop (objects created on a loop must be awaited on the same loop). Sync path untouched.
        if _test_is_async(node_id, style) or any(
            _is_async_fixture(d.func) for d in closure if d.rank == 0
        ):
            try:
                outcome, detail = _drive_async(
                    lambda: self._child_exec_async(node_id, style, requested, closure, combo,
                                                   case_kwargs),
                    combo.get("anyio_backend"),
                )
                cov.stop()
                # Closure merged in here, not inside `_Coverage`, so the capture object stays purely
                # about what executed and the module attribution is visible at the call site.
                return (outcome, detail, cov.report_with_imports(module_key),
                        _UNKNOWN_PURITY)  # async purity not measured
            finally:
                cov.stop()
        # Named as pytest names it, parametrize id included: a fixture keying a resource off
        # `request.node.name` needs `test_x[case]`, not `test_x`, or every case collides (TID-51).
        _node_for(variant_id or node_id)
        # `setUpModule` / `setup_module` before any fixture or test body: a suite uses it to put
        # something in place for the whole file — stubbing an optional SDK in `sys.modules`, say —
        # and without it every test in that file fails on the thing it was meant to provide (TID-60).
        _xunit_module_setup(_import_module(module_key))
        try:
            for d in closure:
                if d.rank != 0:
                    continue  # wider scopes are already live in inherited parent memory
                args = {param: value_of(prov) for param, prov in d.bindings.items()}
                val, gen = _setup_fixture(d, args, combo.get(d.name))
                local[d.name] = val
                gens.append(gen)
            test_args = {param: value_of(prov) for param, prov in requested.items()}
            if case_kwargs:
                test_args.update(case_kwargs)
            # Purity guard / restore: snapshot shared state right before the body, compare right after.
            # When running in-process (no fork) with `restore`, undo any mutation so the next test is
            # isolated WITHOUT a fork — the snapshot/restore fast path for impure tests too.
            # `trusted_pure` (TID-1): a recorded-pure, unchanged test skips the snapshot entirely and runs
            # BARE no-fork — no measurement, no restore, no isolation. Worth ~3.4× where it applies and
            # usually applies to few tests: anything recording into shared state is not pure (TID-41).
            # Otherwise snapshot to measure/restore.
            need_snap = (self.purity_guard or (self.restore and in_process)) and not trusted_pure
            if self.restore and in_process:
                self._enter_module(module_key)
            mod = _import_module(module_key) if need_snap else None
            before = _snapshot_shared(mod) if mod is not None else None
            env_before = dict(os.environ) if mod is not None else None
            # Tracked independently of the per-module snapshot: `sys.modules` is interpreter-global,
            # and a test can swap a library module without touching a single global of its own
            # (TID-27). Cheap enough to do unconditionally on the in-process path.
            modules_before = dict(sys.modules) if (self.restore and in_process) else None
            # Everything restore does NOT model (TID-33). Identities and counts only, so it is
            # affordable per test, and it is the only thing here that can notice a category nobody
            # has thought of yet.
            state_before = _state_fingerprint() if (self.restore and in_process) else None
            # A registry inside an imported library is state the module snapshot cannot see: it does
            # not live in this test's module, and restore has no way to put it back (TID-46).
            registry_before = _registry_snapshot(module_key) if state_before is not None else None
            outcome, detail = _invoke(node_id, style, test_args)
            # Everything below MEASURES; nothing here restores. A file's tests run in one process
            # in file order, and what one leaves behind is there for the next, as under pytest
            # (TID-80). The restore that stands in for a fork now happens when this worker leaves
            # the module (`_leave_module`), against the snapshot taken when it entered, so the
            # next module starts clean and this one behaves as its author saw it under pytest
            # (TID-81). The per-test verdict still says what each test touched: it decides the
            # bare tier (TID-1) and it is what a reader of `--report` wants to know.
            purity = _purity_verdict(mod, before, env_before) if mod is not None else _UNKNOWN_PURITY
            if modules_before is not None:
                replaced = [name for name, module in modules_before.items()
                            if sys.modules.get(name) is not None and sys.modules.get(name) is not module]
                if replaced:
                    # Impure whatever the globals said: `_purity_verdict` cannot see this, and a test
                    # recorded pure would later take the BARE no-fork tier, which skips the snapshot
                    # entirely and would leave the swap in place for good (TID-1).
                    shown = ", ".join(sorted(replaced)[:3])
                    more = f" (+{len(replaced) - 3} more)" if len(replaced) > 3 else ""
                    purity = f"replaced modules in sys.modules: {shown}{more}"
            if state_before is not None:
                drift = _fingerprint_delta(state_before, _state_fingerprint())
                registry_drift = _registry_delta(registry_before, module_key)
                if registry_drift is not None and purity is None:
                    purity = f"mutated another module's state: {registry_drift}"
                if drift is not None:
                    if purity is None:
                        purity = f"changed interpreter state: {drift}"
                    # A thread left running is the one thing no restore can undo, at the boundary
                    # or anywhere: it keeps executing in this process, and in every child forked
                    # from it. That test should never have run in-process — `_leaked` tells the
                    # caller to discard this result and re-run it forked (TID-33, TID-50), and it
                    # forks from now on. Everything else the fingerprint watches is put back when
                    # the module is left, and inside the module it is what pytest would show too.
                    residue = _fingerprint_delta(
                        {k: v for k, v in state_before.items() if k == "threads"},
                        {k: v for k, v in _state_fingerprint().items() if k == "threads"})
                    if residue is not None:
                        self._leaked = f"{drift} (unrestorable: {residue})"
                        purity = f"disturbed interpreter state: {self._leaked}"
            cov.stop()
            return outcome, detail, cov.report_with_imports(module_key), purity
        finally:
            cov.stop()  # idempotent — frees the monitoring tool id even if setup raised
            for gen in reversed(gens):
                _teardown(gen)

    async def _child_exec_async(self, node_id, style, requested, closure, combo, case_kwargs=None) -> tuple:
        """The async sibling of the function-scope portion of `_child_exec` (B5): sets up function-scope
        fixtures (sync or async) on this loop, runs the (possibly async) body, tears down in reverse.
        Wider-scope fixtures are inherited from the parent as usual; only function-scope async providers
        are driven here (a wider-scope async provider is an unsupported edge — none in the corpus)."""
        module_key = _module_key(node_id)
        local: dict[str, object] = {}
        handles: list = []

        def value_of(name: str):
            if name in local:
                return local[name]
            return self._value(name, module_key)

        try:
            for d in closure:
                if d.rank != 0:
                    continue
                args = {param: value_of(prov) for param, prov in d.bindings.items()}
                val, handle = await _setup_fixture_async(d, args, combo.get(d.name))
                local[d.name] = val
                handles.append(handle)
            test_args = {param: value_of(prov) for param, prov in requested.items()}
            if case_kwargs:
                test_args.update(case_kwargs)
            return await _invoke_async(node_id, style, test_args)
        finally:
            for handle in reversed(handles):
                await _teardown_async(handle)

    def _is_fixture_node(self, node_id: str, style: str) -> bool:
        """Whether the object a node names is a fixture rather than a test (TID-88)."""
        if style == "unittest_method":
            return False
        try:
            obj = resolve_target(node_id, style).func
        except (Exception, *_SKIP_EXCEPTIONS):  # noqa: BLE001 — a module that skips itself at
            return False  # import raises a BaseException here; the run below reports it (TID-48)
        return _is_fixture(obj)

    def _requested(self, node: Target) -> dict:
        """The resources a test requests, as `param_name -> provider_name` bindings. Native params
        resolve by **type** (ADR-E012); untyped params fall back to name (the pytest path), so a
        pytest-authored test with `(db, cache)` args binds identically to before."""
        if node.style == "unittest_method":
            return {}  # unittest methods drive their own setUp/tearDown; no DI in Phase 3
        return self.reg.bind_params(node.func)

    def _marks(self, node: Target) -> list:
        """The native marks (`__tiderace_marks__`) on a test, read by attribute — the tiderace-owned
        analogue of pytest's marker read. unittest methods carry none."""
        if node.style == "unittest_method":
            return []
        return list(getattr(node.func, "__tiderace_marks__", ()))

    def _test_classes(self, node: Target) -> tuple:
        """The names in the test class's MRO, narrowest first — empty for a plain function.

        Fixtures defined inside a test class are visible to that class and its subclasses only, so the
        closure needs to know which class the node belongs to (TID-47)."""
        if node.cls is None:
            return ()
        mro = _safe_getattr(node.cls, "__mro__", None) or ()
        return tuple(c.__name__ for c in mro)

    def _indirect(self, node: Target) -> set:
        """Argnames this node's `parametrize` marks route through a fixture (`indirect=`)."""
        if node.style == "unittest_method":
            return set()
        hook_marks = self._hook_marks(node)
        if node.style == "class_method":
            return _indirect_names(node.func, node.cls, node.module, hook_marks=hook_marks)
        return _indirect_names(node.func, node.module, hook_marks=hook_marks)

    def _uses(self, node: Target) -> list:
        """Provider names a test depends on via `@tiderace.uses(Type, ...)` — resolved by type, set up
        in the closure but never passed as args (the native `usefixtures`). unittest carries none."""
        if node.style == "unittest_method":
            return []
        names = []
        for t in getattr(node.func, "__tiderace_uses__", ()):
            provs = self.reg.by_type.get(t, [])
            if len(provs) == 1:  # unambiguous; ambiguity is the author's to disambiguate
                names.append(provs[0])
        return names

    def _cases(self, node: Target) -> list:
        """The variants of a test: native `@tiderace.cases`, else `@pytest.mark.parametrize`.

        unittest has neither — pytest cannot parametrize a `TestCase` method
        either, so the early return matches the oracle.
        """
        if node.style == "unittest_method":
            return []
        native = list(getattr(node.func, "__tiderace_cases__", ()))
        if native:
            return [(c, None, None) for c in native]  # native cases carry no author-supplied id
        # The class and the module too: pytest applies their marks to every test they hold (TID-53).
        owner = node.cls if node.style == "class_method" else None
        return _parametrize_cases(node.func, *(o for o in (owner, node.module) if o is not None),
                                  hook_marks=self._hook_marks(node))

    def _hook_marks(self, node: Target) -> list:
        """The parametrize axes this node's `pytest_generate_tests` hooks declare (TID-85). Nothing
        to run ⇒ nothing computed: a suite without the hook pays a dictionary lookup."""
        node_id, module, func = node.node_id, node.module, node.func
        owner = node.cls if node.style == "class_method" else None
        if node_id in _HOOK_MARKS:
            return _HOOK_MARKS[node_id]
        if _safe_getattr(module, "pytest_generate_tests", None) is None and not any(
                _safe_getattr(m, "pytest_generate_tests", None) is not None for _, m in _CONFTEST_SCOPES):
            return []
        requested = self._requested(node)  # param → provider (or the bare name)
        names = list(requested)
        try:
            providers = {p: t for p, t in requested.items() if self.reg.is_provider(t)}
            closure = _closure(self.reg, node.module_key, providers, [], self._test_classes(node))
            names = list(dict.fromkeys(names + [d.name for d in closure]))
        except Exception:  # noqa: BLE001 — an unresolvable request is the test's problem, later
            pass
        return _generate_tests_marks(node_id, func, module, owner, names,
                                     _own_markers(module, owner, func))

    def teardown_all(self) -> None:
        _save_file_deps_cache()  # what this worker parsed, for the next run (TID-82)
        self._module_child_close()  # its module's tests are done: its fixtures, hooks and exit (TID-80)
        while self.active:
            _teardown(self.active.pop().gen)
        _xunit_class_teardown()  # tearDownClass / teardown_class, once per class (TID-64)
        _xunit_module_teardown()  # tearDownModule / teardown_module, once this worker is done


# Every conftest `_discover` imported, with the directory it governs: `""`/`"."` for the run root, a
# `..`-relative location for an ancestor (which governs everything), else a root-relative directory.
_CONFTEST_SCOPES: list = []
_HOOK_MARKS: dict[str, list] = {}  # node id → the parametrize marks its generate_tests hooks produced


class _GenerateTestsError(Exception):
    """A `metafunc.parametrize` that pytest would refuse — reported on the test, as pytest reports it."""


class _SyntheticMark:
    """A `parametrize` mark that came from `metafunc.parametrize` rather than a decorator; the same
    shape `_parametrize_cases` and `_indirect_names` already read."""

    __slots__ = ("name", "args", "kwargs")

    def __init__(self, argnames, argvalues, ids, indirect):
        self.name = "parametrize"
        self.args = (argnames, list(argvalues))
        self.kwargs = {"ids": ids, "indirect": indirect}


class _MetaFunc:
    """What a `pytest_generate_tests(metafunc)` hook is handed (TID-85): the test's fixture names,
    its function/module/class, a `definition` that answers marker queries, a `config`, and
    `parametrize`, which records an axis for the test exactly as a `@pytest.mark.parametrize` would."""

    def __init__(self, node_id: str, func, module, cls, fixturenames: list, markers: list):
        self.function = func
        self.module = module
        self.cls = cls
        self.fixturenames = list(fixturenames)
        self.config = _Config()
        self.definition = _HookItem(node_id, node_id.rsplit("::", 1)[-1], markers)
        self._marks: list = []

    def parametrize(self, argnames, argvalues, indirect=False, ids=None, scope=None, *,
                    _param_mark=None):
        names = ([n.strip() for n in argnames.split(",") if n.strip()]
                 if isinstance(argnames, str) else list(argnames))
        for name in names:
            if name not in self.fixturenames:
                raise _GenerateTestsError(
                    f"In {self.definition.name}: function uses no argument '{name}'")
        self._marks.append(_SyntheticMark(names, argvalues, ids, indirect))


def _conftests_governing(module_key: str) -> list:
    """The conftest modules whose directory holds `module_key`, deepest first — pytest's calling
    order for their hooks (a later-registered plugin is called first)."""
    module_dir = module_key.rsplit("/", 1)[0] if "/" in module_key else ""
    governing = []
    for location, module in _CONFTEST_SCOPES:
        loc = "" if location in (".", "") else location.replace(os.sep, "/")
        if loc.startswith(".."):
            depth = -1  # an ancestor: governs everything, called after every in-tree conftest
        elif loc == "" or module_dir == loc or module_dir.startswith(loc + "/"):
            depth = loc.count("/") + 1 if loc else 0
        else:
            continue
        governing.append((depth, module))
    governing.sort(key=lambda d: -d[0])
    return [m for _, m in governing]


def _generate_tests_marks(node_id: str, func, module, cls, fixturenames: list, markers: list) -> list:
    """Run the `pytest_generate_tests` hooks that apply to this test — its module's own first, then
    its conftests deepest to root — and return the parametrize marks they declared, in pytest's
    order (which is the order their ids appear in the node id). Cached per node: hooks are
    deterministic and `_cases`/`_indirect` both ask."""
    cached = _HOOK_MARKS.get(node_id)
    if cached is not None:
        return cached
    hooks = []
    own = _safe_getattr(module, "pytest_generate_tests", None)
    if callable(own):
        hooks.append(own)
    for conftest in _conftests_governing(_module_key(node_id)):
        hook = _safe_getattr(conftest, "pytest_generate_tests", None)
        if callable(hook):
            hooks.append(hook)
    marks: list = []
    if hooks:
        metafunc = _MetaFunc(node_id, func, module, cls, fixturenames, markers)
        for hook in hooks:
            hook(metafunc)
        marks = metafunc._marks
    _HOOK_MARKS[node_id] = marks
    return marks


def _indirect_names(func, *outer, hook_marks=None) -> set:
    """Argnames a `parametrize` marks as **indirect**, anywhere in the owner chain — or in a
    `pytest_generate_tests` hook's call (TID-85).

    `indirect=True` (or a list of names) does not hand the value to the test: pytest gives it to the
    *fixture* of that name as `request.param`, and the test receives whatever the fixture returns. So
    an indirect name stays a fixture request, where a direct one overrides any fixture sharing its
    name (TID-57). Getting this backwards hands the test the raw parametrize value — typically the
    fixture function itself, which then fails on the first attribute the test touches."""
    out: set = set()
    for mark in list(hook_marks or ()) + _own_markers(func, *outer):
        if getattr(mark, "name", "") != "parametrize":
            continue
        indirect = (getattr(mark, "kwargs", None) or {}).get("indirect")
        if not indirect:
            continue
        names = mark.args[0]
        names = ([n.strip() for n in names.split(",") if n.strip()] if isinstance(names, str)
                 else list(names))
        out.update(names if indirect is True else
                   [n for n in names if n in set(indirect)])
    return out


def _parametrize_cases(func, *outer, hook_marks=None) -> list[dict]:
    """Expand `@pytest.mark.parametrize` on ``func`` — and on its class and module — into cases.

    The corpus is authored against pytest, so a test whose arguments come from
    `parametrize` looks, to a runner that only knows fixtures, like a test
    requesting fixtures nobody provides — it is then called bare and dies on
    "missing 1 required positional argument". Reading the marker turns those
    into ordinary cases, which the existing `case_kwargs` path already runs.

    Values are returned as name→value maps rather than positionally, because
    `parametrize`'s argnames need not match the signature order.

    Stacked marks multiply, as in pytest. Each case carries its **explicit id** when the author gave
    one — `ids=[...]`, `ids=callable`, or `pytest.param(..., id=...)` — because those ids are
    selectors, and a generated `[size1]` where pytest prints `[decimal]` cannot be pasted from one
    runner into the other. Returns `(kwargs, explicit_id_or_None)` per case.

    Stacked marks with ids on only *some* axes fall back to generated ids for the whole case rather
    than splicing the two schemes, which would produce an id matching neither runner.

    `outer` is the rest of the owner chain, narrowest first: the class, then the module. pytest applies
    a mark on a class to every method the class collects, and the same for a module-level `pytestmark`
    — reading only the function missed both. A class parametrized with 5 values and holding 4 methods
    is 20 tests in pytest and was 4 here, each failing on the argument nobody supplied (TID-53).

    Order matters and is pytest's, not ours: axes run narrowest first, so `test[1-A]` puts the
    function's own parameter before the class's, and the class's value varies fastest across the
    generated cases. `_own_markers` reports widest first, which is why the chain is passed in reverse
    here.
    """
    # Hook-declared axes first: pytest calls `pytest_generate_tests` hooks before it applies the
    # decorator marks (its own mark handling is one such hook, registered earliest and so called
    # last), and an id is the axes in call order — `test_x[sqlite-1]` for a conftest's `backend` and
    # the function's own `n` (TID-85).
    marks = list(hook_marks or ()) + [
        m for m in _own_markers(func, *outer) if getattr(m, "name", "") == "parametrize"
    ]
    if not marks:
        return []
    axes: list[list[tuple]] = []
    for mark in marks:
        names = mark.args[0]
        names = (
            [n.strip() for n in names.split(",") if n.strip()] if isinstance(names, str)
            else list(names)
        )
        ids_kw = (getattr(mark, "kwargs", None) or {}).get("ids")
        axis: list[tuple] = []
        for position, entry in enumerate(mark.args[1]):
            # `pytest.param(...)` carries `.values`/`.marks`; both are checked so a
            # plain dict argvalue (which has a `.values` *method*) is not mistaken for one.
            explicit = None
            # Safe probes: `entry` is an arbitrary parametrize value, and a lazy proxy passed as one raises
            # on attribute access exactly as it does as a module global (TID-43).
            if _is_param_set(entry):
                raw = tuple(entry.values)
                explicit = _safe_getattr(entry, "id", None)
            elif len(names) == 1:
                raw = (entry,)
            else:
                raw = tuple(entry)
            if explicit is None and ids_kw is not None:
                explicit = _explicit_id(ids_kw, raw, position)
            axis.append((dict(zip(names, raw)), None if explicit is None else str(explicit), position))
        axes.append(axis)
    cases: list[tuple] = []
    for combo in itertools.product(*axes):
        merged: dict = {}
        pieces: list[tuple] = []  # per axis: (its argnames, the author's id or None)
        positions: dict = {}  # per argname: the value's position in its axis (TID-86)
        for piece, piece_id, axis_pos in combo:
            merged.update(piece)
            pieces.append((tuple(piece), piece_id))
            positions.update({name: axis_pos for name in piece})
        if pieces and all(pid is not None for _, pid in pieces):
            case_id = "-".join(pid for _, pid in pieces)
        elif any(pid is not None for _, pid in pieces):
            # Mixed: an explicit id on some axes and none on others. pytest keeps the explicit
            # piece and generates the rest per axis — `test_x[EU-sqlite]` for `ids=["EU", "US"]`
            # on `region` and a bare `backend` (TID-85) — so hand `_variant_parts` the axes.
            case_id = pieces
        else:
            case_id = None
        cases.append((merged, case_id, positions))
    return cases


def _explicit_id(ids_kw, values: tuple, position: int):
    """The author-supplied id for one case, from `ids=[...]` or `ids=callable`.

    A callable is applied per value and joined, as pytest does; a callable that returns `None` for a
    value means "generate this part", and since the parts cannot be mixed here that degrades the
    whole case to a generated id rather than a half-built one."""
    try:
        if callable(ids_kw):
            produced = [ids_kw(v) for v in values]
            return None if any(p is None for p in produced) else "-".join(str(p) for p in produced)
        return ids_kw[position]
    except Exception:  # noqa: BLE001 — a malformed `ids` must not take the test down
        return None


def _pytest_markers(node: Target) -> list:
    """The `@pytest.mark.*` objects on a test, from its module, class and function."""
    return list(_own_markers(*node.owners))


def _mark_names(node_id: str, style: str) -> set:
    """Every selectable mark name on a test — pytest's and tiderace's own.

    Selection has to mean the same thing in both dialects, so `-m "not slow"` deselects a
    `@pytest.mark.slow` test and a `@tiderace.mark.slow` one alike. pytest marks are read from the
    module, the class and the function; native tags are read from the function's
    `__tiderace_marks__` (TID-59)."""
    try:
        owners = resolve_target(node_id, style, lenient=True).owners
    except (Exception, *_SKIP_EXCEPTIONS):  # noqa: BLE001 — an unimportable module, or one that
        return set()  # skips at import (a BaseException), surfaces per node, not here (TID-102)
    names = {getattr(m, "name", "") for m in _own_markers(*owners)}
    for owner in owners:
        for m in _safe_getattr(owner, "__tiderace_marks__", None) or ():
            if getattr(m, "kind", "") == "tag" and getattr(m, "name", ""):
                names.add(m.name)
    return names


# Marks pytest itself defines; `--strict-markers` never complains about these.
_BUILTIN_MARKS = frozenset({
    "skip", "skipif", "xfail", "parametrize", "usefixtures", "filterwarnings", "tryfirst", "trylast",
})
_PLUGIN_MARKS: frozenset | None = None  # markers the installed plugins register; None = not asked yet
_PLUGIN_MARKS_ASKED = False  # `_plugin_marks` asks pytest once per process, whatever it answers


def _plugin_marks() -> frozenset | None:
    """Every marker the installed pytest plugins register, or `None` if we could not find out.

    A plugin registers its markers at runtime — pytest-timeout's `timeout`, pytest-benchmark's
    `benchmark`, pytest-django's `django_db` — by calling `addinivalue_line("markers", ...)` from
    `pytest_configure`. None of that is in the project's own `markers` list, and tiderace does not run
    plugins, so a hand-written allowlist would flag every one of them as a typo. pirn-core showed
    exactly that: 60 tests erroring on `@pytest.mark.timeout`, which pytest accepts without comment
    (TID-60).

    So ask pytest, which already knows: `--markers` prints the registered set, plugins included. One
    subprocess, only when a project has turned strict checking on, cached for the run.

    `None` means we could not get an answer, and the caller must then **not** enforce: a false error
    on a valid mark is worse than a missed typo, because it fails a suite that is correct."""
    global _PLUGIN_MARKS, _PLUGIN_MARKS_ASKED
    if _PLUGIN_MARKS_ASKED:
        return _PLUGIN_MARKS  # asked once per process — a "could not find out" included (TID-91)
    _PLUGIN_MARKS_ASKED = True
    try:
        out = subprocess.run(
            [sys.executable, "-m", "pytest", "--markers"],
            capture_output=True, text=True, timeout=60, cwd=_ROOT or None,
        ).stdout
    except Exception:  # noqa: BLE001 — no pytest, or it refused to start
        return None
    names = frozenset(re.findall(r"^@pytest\.mark\.(\w+)", out, re.M))
    if not names:
        return None  # an empty answer is not an answer
    _PLUGIN_MARKS = names
    return _PLUGIN_MARKS


def _registered_marks(project: ProjectConfig) -> tuple:
    """`(declared names, strict)` — which marks the project declared, and whether it wants them checked.

    pytest projects declare marks as `markers = ["slow: ...", ...]` in their config and opt into
    validation with `--strict-markers`; a tiderace-native project says the same thing under
    `[tool.tiderace]`. Both are read, because a suite mid-migration has both kinds of test in it."""
    names: set = set()
    strict = project.flag("--strict-markers") or project.flag("--strict")
    for raw in project.values("markers"):
        # pytest's spelling is "name: description" or a bare name; only the name selects.
        name = str(raw).split(":", 1)[0].strip()
        if name:
            names.add(name.partition("(")[0].strip())  # `name(args)` in a few suites
    if project.values("strict_markers"):
        strict = True
    # The native declaration surface (TID-67): `tiderace.mark.register("slow", ...)` in a conftest.
    # Every conftest has been imported by the time this runs, so whatever they registered is here.
    # Without it a suite written natively — no `import pytest` anywhere — still needed a *pytest*
    # config block to declare its own marks, or strict checking rejected them.
    try:
        import tiderace
        names.update(tiderace.mark.registered())
    except Exception:  # noqa: BLE001 — no native package on this interpreter ⇒ no native marks
        pass
    # `--strict-markers` on the command line (TID-67), for the same reason `-m` and `-k` travel
    # this way: a project with no config file at all has nowhere else to say it.
    if _env_flag("TIDERACE_STRICT_MARKERS"):
        strict = True
    return frozenset(names), strict


def _skip_decision(marks: list):
    """The skip reason if any `skip` / active `skip_if` mark applies, else None."""
    for m in marks:
        if m.kind == "skip" or (m.kind == "skip_if" and m.condition):
            return m.reason or m.kind
    return None


def _apply_xfail(marks: list, outcome: str, detail: str):
    """Fold an `xfail` mark into the outcome: a fail/error becomes `xfail`; a pass becomes `xpass`
    (or `failed` when the mark is `strict`). No xfail mark ⇒ unchanged."""
    xf = next((m for m in marks if m.kind == "xfail"), None)
    if xf is None:
        return outcome, detail
    if outcome in ("failed", "error"):
        return "xfail", xf.reason or detail
    if outcome == "passed":
        if xf.strict:
            return "failed", f"[xpass strict] {xf.reason}".strip()
        return "xpass", xf.reason
    return outcome, detail  # skipped stays skipped


def _invoke(node_id: str, style: str, args: dict) -> tuple[str, str]:
    """Run the test, then fold in any marker it or its fixtures attached while running (TID-51)."""
    outcome, detail = _invoke_body(node_id, style, args)
    return _runtime_outcome(_CURRENT_NODE, outcome, detail)


def _invoke_body(node_id: str, style: str, args: dict) -> tuple[str, str]:
    node = resolve_target(node_id, style, lenient=style == "unittest_method")
    module = node.module
    try:
        if node.is_unittest:
            return _invoke_unittest(module, node_id)
        if style == "class_method":
            _xunit_class_setup(node.cls)  # pytest's `setup_class`, once per class per process (TID-60)
            instance = node.cls()
            bound = getattr(instance, node.name)
            call_args, request = _with_request(bound, args, node_id, instance)
            setup, teardown = _xunit_test_hooks(module, style, node_id, bound)
            try:
                if setup:
                    _call_hook(*setup)  # `setup_method(self, method)`
                _maybe_await(bound(**call_args))
            finally:
                if teardown:
                    try:
                        _call_hook(*teardown)
                    except Exception:  # noqa: BLE001 — teardown must not mask the body's outcome
                        pass
                _test_finalizers(request)
            return "passed", ""
        func = node.func
        call_args, request = _with_request(func, args, node_id)
        setup, teardown = _xunit_test_hooks(module, style, node_id, func)
        try:
            if setup:
                _call_hook(*setup)  # `setup_function(function)`
            _maybe_await(func(**call_args))
        finally:
            if teardown:
                try:
                    _call_hook(*teardown)
                except Exception:  # noqa: BLE001 — teardown must not mask the body's outcome
                    pass
            _test_finalizers(request)
        return "passed", ""
    except AssertionError as exc:
        plain = "".join(traceback.format_exception_only(type(exc), exc))
        rich = _introspect_assertion(exc)  # lazy: only a FAILED assert pays this (ADR-E009)
        return "failed", (rich + plain) if rich else plain
    except _SKIP_EXCEPTIONS as exc:
        return "skipped", str(exc)
    except Exception as exc:  # noqa: BLE001 — a body that raises FAILED; it ran and came out wrong
        # pytest reserves `error` for a test it could not attempt — a fixture that raised, a module
        # that would not import — and calls anything the body raises a failure, assertion or not
        # (TID-30, verified against pytest directly). tiderace split on exception type instead, so
        # `raise RuntimeError` reported `error` where pytest reports `failed`. Both are red, but the
        # taxonomy leaked into the reporters and made the two runners impossible to reconcile.
        return "failed", "".join(traceback.format_exception_only(type(exc), exc))


def _maybe_await(result):
    """Drive an `async def test_*` to completion (Phase 4). A sync test returns a plain value (passed
    straight through); a coroutine is run on a fresh event loop per test — isolation is free since each
    test is its own fork child. Async *providers* are deferred to Track B (B5)."""
    if inspect.iscoroutine(result):
        asyncio.run(result)


class _SkipAwareResult(unittest.TestResult):
    """A `TestResult` that recognises pytest's `Skipped` as a skip rather than an error (TID-16).

    `unittest`'s executor special-cases exactly one skip type, `unittest.SkipTest`. pytest's `skip()`
    and `importorskip()` raise `_pytest.outcomes.Skipped`, which derives from `BaseException` and so
    falls through to the executor's bare `except:` and is recorded via `addError`. The result: on a
    `TestCase` (including `IsolatedAsyncioTestCase`), `pytest.importorskip("optional_dep")` — the
    standard way to skip when an extra is absent — reported as an ERROR, turning a clean run red for
    something that is not a defect.

    `addError` receives the live `(type, value, tb)`, so the verdict is made on the exception object
    itself; the alternative, reading it back out of `result.errors`, only ever sees a formatted
    string. Covers skips raised from `setUp` and `tearDown` too, which route here just the same."""

    def addError(self, test, err):  # noqa: N802 — unittest's own casing
        if isinstance(err[1], _SKIP_EXCEPTIONS):
            self.addSkip(test, str(err[1]))
            return
        super().addError(test, err)


_NODES_RUN = 0  # how many nodes this process has run — whether a test here has neighbours (TID-70)

_IMPORT_HISTORY_NOTE = (
    "\n(tiderace) this assertion reads import history — which tests ran earlier in this process, and "
    "in what order, is not pytest's file order and is not promised to be; a test that depends on it "
    "is order-dependent under pytest too. See the execution-model docs, \"What the engine does not "
    "promise\"."
)


def _note_import_history(result: dict, *, pristine: bool = False) -> dict:
    """Append a one-line explanation to a failure that reads `sys.modules` (TID-70).

    The one pirn-agents divergence in the whole benchmark was `assert "chromadb" not in sys.modules`
    — true only if no earlier test in the same process imported it. That is not a defect in the
    runner; it is a test asserting on something no runner promises, and pytest's own `-p randomly`
    breaks it the same way. But a bare `AssertionError` against a runner the author has just
    switched to reads as the runner's bug, so the failure now says what it depends on. Only when the
    dependence is real: this process ran other tests before this one, or it is the clean room's
    re-run of a demoted test, where the image is pristine and nothing was ever imported."""
    if not pristine and _NODES_RUN <= 1:
        return result
    for record in [result, *result.get("variants", ())]:
        detail = record.get("detail") or ""
        if record.get("outcome") in ("failed", "error") and "sys.modules" in detail \
                and _IMPORT_HISTORY_NOTE not in detail:
            record["detail"] = detail + _IMPORT_HISTORY_NOTE
    return result


_XUNIT_DONE: set = set()  # (kind, qualified name) of module/class setups this process has run
_XUNIT_CLASSES: dict = {}  # class key -> the class object, so its teardown can be found at worker end
# class key -> (outcome, detail) when the class's own setup did not complete (TID-64). A setUpClass
# that skips or raises decides the outcome of EVERY method in the class, not only the one that
# happened to trigger it — which is what once-per-class means when the first attempt fails.
_XUNIT_CLASS_FAILED: dict = {}


def _xunit_class_key(cls) -> tuple:
    return ("class", f"{_safe_getattr(cls, '__module__', '')}.{_safe_getattr(cls, '__name__', '')}")


def _call_hook(owner, names: tuple, *args) -> bool:
    """Call the first hook of `names` that `owner` defines, passing `args` if it accepts them.

    Both dialects are looked for at every level, because a suite mid-migration has files in each:
    unittest spells it `setUpModule`, pytest's xunit style spells it `setup_module`."""
    for name in names:
        hook = _safe_getattr(owner, name, None)
        if hook is None or not callable(hook):
            continue
        try:
            signature = inspect.signature(hook)
            accepted = len([p for p in signature.parameters.values()
                            if p.kind in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD)])
        except (TypeError, ValueError):  # a builtin or C-level callable
            accepted = len(args)
        hook(*args[:accepted])
        return True
    return False


def _xunit_module_setup(module) -> None:
    """`setUpModule` / `setup_module`, once per module per process.

    Once per *process*, not per test: a forked run re-enters it in each child, which is the right
    reading since every child is its own interpreter, but repeating it for every in-process test
    would run a non-idempotent hook many times. Its teardown runs when the worker finishes with the
    module, via `teardown_all` (TID-60)."""
    key = ("module", _safe_getattr(module, "__name__", ""))
    if key in _XUNIT_DONE:
        return
    _XUNIT_DONE.add(key)
    _call_hook(module, ("setUpModule", "setup_module"), module)


def _xunit_class_teardown() -> None:
    """`tearDownClass` / `teardown_class` for every class this process set up, once each (TID-64).

    Runs before the module teardowns, since a class's teardown may still need its module. Only
    classes whose setup *completed* are torn down: one that skipped or raised never acquired
    whatever its teardown releases."""
    for key, cls in list(_XUNIT_CLASSES.items()):
        if key not in _XUNIT_CLASS_FAILED:
            try:
                _call_hook(cls, ("tearDownClass", "teardown_class"), cls)
            except Exception:  # noqa: BLE001 — a teardown fault must not mask the run's results
                pass
        _XUNIT_CLASSES.pop(key, None)
        _XUNIT_DONE.discard(key)


def _xunit_module_teardown() -> None:
    """`tearDownModule` / `teardown_module` for every module this process set up."""
    for kind, name in list(_XUNIT_DONE):
        if kind != "module":
            continue
        module = sys.modules.get(name)
        if module is not None:
            try:
                _call_hook(module, ("tearDownModule", "teardown_module"), module)
            except Exception:  # noqa: BLE001 — a teardown fault must not mask the run's results
                pass
        _XUNIT_DONE.discard((kind, name))


def _xunit_test_hooks(module, style: str, node_id: str, target) -> tuple:
    """`(setup, teardown)` call specs for the per-test xunit hooks, or `(None, None)`.

    A method's hooks live on its class and receive the method; a module-level test's live on the
    module and receive the function. unittest's own `setUp`/`tearDown` are deliberately absent:
    `TestCase.run()` calls those itself, and calling them here would double every one."""
    if style == "unittest_method":
        return (None, None)
    if style == "class_method":
        cls = _safe_getattr(module, _class_method(node_id)[0], None)
        if cls is None:
            return (None, None)
        owner = target.__self__ if hasattr(target, "__self__") else cls
        return ((owner, ("setup_method",), target), (owner, ("teardown_method",), target))
    return ((module, ("setup_function",), target), (module, ("teardown_function",), target))


def _xunit_class_setup(cls) -> None:
    """pytest's `setup_class`, once per class per process. unittest's `setUpClass` is run by
    `_invoke_unittest`, which needs it inside its own result handling."""
    key = _xunit_class_key(cls)
    if key in _XUNIT_DONE:
        return
    _XUNIT_DONE.add(key)
    _XUNIT_CLASSES[key] = cls  # so `teardown_class` runs at worker end (TID-64) — it never did before
    _call_hook(cls, ("setup_class",), cls)


def _invoke_unittest(module, node_id: str) -> tuple[str, str]:
    """Run one `unittest.TestCase` method with fuller fidelity (Phase 4): honor `setUpClass`/
    `tearDownClass` (which `TestCase.run()` alone does NOT call), and map `@expectedFailure` /
    unexpected-success / `subTest` to the right node outcome.

    `setUpClass` runs once per class per *process* and `tearDownClass` once at worker end, the
    contract unittest's own runner keeps (TID-64). They used to run around every method — right when
    every test forked, since each child was its own process, and wrong under the in-process ladder,
    where a class's methods share one process: N× the setup cost, and a `setUpClass` that opens a
    database or counts its own calls behaved differently from `python -m unittest`. A forked child
    inherits `_XUNIT_DONE`, so a class the parent set up is not set up again there either."""
    cls_name, method = _class_method(node_id)
    cls = module.__dict__[cls_name]
    key = _xunit_class_key(cls)
    if key in _XUNIT_CLASS_FAILED:
        return _XUNIT_CLASS_FAILED[key]
    if key not in _XUNIT_DONE:
        _XUNIT_DONE.add(key)
        _XUNIT_CLASSES[key] = cls
        try:
            cls.setUpClass()
        except _SKIP_EXCEPTIONS as exc:  # setUpClass may skip the whole class
            _XUNIT_CLASS_FAILED[key] = ("skipped", str(exc))
            return _XUNIT_CLASS_FAILED[key]
        except Exception as exc:  # noqa: BLE001 — unittest errors every method of the class
            _XUNIT_CLASS_FAILED[key] = (
                "error", "".join(traceback.format_exception_only(type(exc), exc)))
            return _XUNIT_CLASS_FAILED[key]
    result = _SkipAwareResult()
    try:
        cls(method).run(result)
    except _SKIP_EXCEPTIONS as exc:
        return "skipped", str(exc)

    if result.errors:
        # unittest files body, setUp and tearDown exceptions all under `errors`, and pytest reports
        # every one of those as FAILED — checked against pytest rather than assumed (TID-30). Only a
        # fixture fault stays an error, and that path never reaches here.
        return "failed", result.errors[0][1]
    if result.failures:  # includes subTest failures (each recorded with its sub-description)
        return "failed", result.failures[0][1]
    if getattr(result, "unexpectedSuccesses", None):
        return "failed", "unexpected success: a test marked @expectedFailure passed"
    if getattr(result, "expectedFailures", None):
        return "xfail", result.expectedFailures[0][1]
    if result.skipped:
        return "skipped", result.skipped[0][1]
    return "passed", ""


# --------------------------------------------------------------------------- lazy assertion introspection
_CMP_OPS = {
    ast.Eq: "==", ast.NotEq: "!=", ast.Lt: "<", ast.LtE: "<=", ast.Gt: ">", ast.GtE: ">=",
    ast.In: "in", ast.NotIn: "not in", ast.Is: "is", ast.IsNot: "is not",
}


def _introspect_assertion(exc: AssertionError) -> str | None:
    """Rich diff for a failed bare `assert`, built by RE-EVALUATING the failing expression once with
    the live frame's locals/globals (ADR-E009 — lazy: passes cost nothing). Returns a formatted block
    (operand source + values + an element/line diff), or `None` to fall back to the plain message when
    it is unsafe/unsupported (re-eval raises → side-effecting or non-reproducing; not a single compare).
    """
    tb = exc.__traceback__
    if tb is None:
        return None
    while tb.tb_next is not None:  # deepest frame = where the assert raised
        tb = tb.tb_next
    frame, lineno, filename = tb.tb_frame, tb.tb_lineno, tb.tb_frame.f_code.co_filename

    node = _find_assert(filename, lineno)
    if node is None or not isinstance(node.test, ast.Compare) or len(node.test.ops) != 1:
        return None  # only single comparisons are introspected in this pass
    cmp = node.test
    op = _CMP_OPS.get(type(cmp.ops[0]))
    if op is None:
        return None
    try:
        left = _eval_stable(cmp.left, frame, filename)
        right = _eval_stable(cmp.comparators[0], frame, filename)
    except Exception:  # noqa: BLE001 — re-eval failed/unstable (impure/non-reproducing) → fall back
        return None

    lines = [
        "assertion failed (tiderace rich diff):",
        f"    {ast.unparse(cmp.left)} {op} {ast.unparse(cmp.comparators[0])}",
        f"    left  = {_short_repr(left)}",
        f"    right = {_short_repr(right)}",
    ]
    diff = _value_diff(left, right)
    if diff:
        lines.append("    diff:")
        lines.extend(f"      {d}" for d in diff)
    return "\n".join(lines) + "\n"


def _find_assert(filename: str, lineno: int):
    """The `ast.Assert` node at (or spanning) `lineno` in `filename`, or None."""
    src = "".join(linecache.getlines(filename))
    if not src:
        return None
    try:
        tree = ast.parse(src)
    except SyntaxError:
        return None
    for node in ast.walk(tree):
        if isinstance(node, ast.Assert):
            end = getattr(node, "end_lineno", node.lineno)
            if node.lineno <= lineno <= (end or node.lineno):
                return node
    return None


class _NonReproducing(Exception):
    """Raised when an operand yields a different value on re-eval (side-effecting / nondeterministic),
    so the introspector falls back to the plain message instead of reporting a misleading diff."""


def _eval_stable(node, frame, filename):
    """Evaluate one operand in the failing frame's scope, **twice**, and only trust it if both evals
    agree — the ADR-E009 purity guard. A differing value (e.g. a counter/RNG/clock call) means the
    expression doesn't reproduce, so we refuse to build a diff that would misreport what failed."""
    code = compile(ast.Expression(body=node), filename, "eval")
    first = eval(code, frame.f_globals, frame.f_locals)  # noqa: S307 — our own re-eval, same scope
    second = eval(code, frame.f_globals, frame.f_locals)  # noqa: S307
    if not _reproduces(first, second):
        raise _NonReproducing()
    return first


def _reproduces(a, b) -> bool:
    """Whether two re-evals are equal. Conservative: any `==` that raises ⇒ treat as non-reproducing."""
    try:
        return bool(a == b)
    except Exception:  # noqa: BLE001
        return False


def _short_repr(value, limit: int = 300) -> str:
    try:
        r = repr(value)
    except Exception:  # noqa: BLE001
        r = f"<unreprable {type(value).__name__}>"
    return r if len(r) <= limit else r[:limit] + f"… (+{len(r) - limit} chars)"


def _value_diff(left, right) -> list[str]:
    """A small per-element / per-line diff for the common container/string cases (empty otherwise)."""
    if isinstance(left, str) and isinstance(right, str):
        d = list(difflib.unified_diff(left.splitlines(), right.splitlines(), "left", "right", lineterm=""))
        return d[:40]
    if isinstance(left, (list, tuple)) and isinstance(right, (list, tuple)):
        out = []
        if len(left) != len(right):
            out.append(f"length {len(left)} != {len(right)}")
        for i, (a, b) in enumerate(zip(left, right)):
            if a != b:
                out.append(f"[{i}] {_short_repr(a, 80)} != {_short_repr(b, 80)}")
            if len(out) >= 20:
                break
        return out
    if isinstance(left, dict) and isinstance(right, dict):
        out = []
        for k in sorted(set(left) | set(right), key=repr):
            if left.get(k) != right.get(k):
                out.append(f"[{_short_repr(k, 40)}] {_short_repr(left.get(k), 60)} != {_short_repr(right.get(k), 60)}")
            if len(out) >= 20:
                break
        return out
    return []


# --------------------------------------------------------------------------- parametrization ids
def _id_part(value, argname: str, index: int) -> str:
    """One parameter's contribution to a pytest-style `[...]` id, matching pytest's own spelling.

    Parity matters here rather than being cosmetic: these ids are selectors. Someone who copies
    `test_rejected[(SELECT 1)]` out of a pytest run and pastes it into tiderace has to hit the same
    test, so this follows `_pytest.python._idval` rather than inventing a scheme:

    * strings keep printable ASCII verbatim (spaces, quotes, brackets and all) and escape the rest,
      which is exactly `ascii_escaped` — `"таблица"` becomes `\\u0442\\u0430\\u0431\\u043b\\u0438\\u0446\\u0430`;
    * scalars print as themselves (`3`, `True`, `None`, `1.5`);
    * anything else is `argname` + its index, because a `repr` would embed addresses and stop being
      stable between runs.
    """
    if isinstance(value, enum.Enum):
        return str(value)  # `OpaquePolicy.REPR_CONTENT`; before the int branch, since IntEnum is one
    if isinstance(value, str):
        return value.encode("unicode_escape").decode("ascii")
    if isinstance(value, bytes):
        # pytest's `ascii_escaped` for bytes: non-ASCII as `\xNN`, then anything non-printable the
        # same way, so `b"\x1b[45m123\x1b[0m"` is `\x1b[45m123\x1b[0m` and `b"\xff"` is `\xff` —
        # not `expect0`, and not the doubly-escaped `\\xff` (TID-86).
        text = value.decode("ascii", "backslashreplace")
        return "".join(c if c.isprintable() else f"\\x{ord(c):02x}" for c in text)
    if value is None or isinstance(value, (bool, int, float)):
        return str(value)
    # Classes and functions id by name in pytest. A parametrize value is an arbitrary object, and a lazy
    # proxy passed as one raises on attribute access just as it does as a module global (TID-43).
    name = _safe_getattr(value, "__name__", None)
    if isinstance(name, str):
        return name
    return f"{argname}{index}"


def _is_param_set(value) -> bool:
    """A `pytest.param(...)` — `ParameterSet` — probed safely: a lazy proxy raises on attribute access."""
    return _safe_hasattr(value, "values") and _safe_hasattr(value, "marks") and _safe_hasattr(value, "id")


def _param_value(value):
    """What a fixture's `request.param` is for one entry of `params=`: the value itself, or, for a
    `pytest.param(...)`, its values — one value unwrapped, several as a tuple — as pytest hands it
    over. anyio's `anyio_backend` is `pytest.param(("asyncio", {...}), id="asyncio")`, and every
    test on it received the ParameterSet instead of the tuple (TID-86)."""
    if _is_param_set(value):
        values = tuple(value.values)
        return values[0] if len(values) == 1 else values
    return value


def _fixture_param_id(fdef, index: int, value):
    """The author-supplied id for one parametrized-FIXTURE case, or None to generate one.

    `@pytest.fixture(params=[...], ids=[...])` takes the same shapes `parametrize` does, so this
    mirrors `_explicit_id`. Kept separate because a fixture's ids live on its definition rather than
    on a mark, and the two are resolved at different points."""
    # `pytest.param(..., id="asyncio")` as a fixture param carries its own id (TID-86); `ids=` on
    # the fixture applies to the rest.
    if _is_param_set(value) and _safe_getattr(value, "id", None) is not None:
        return str(value.id)
    ids = getattr(fdef, "param_ids", None)
    if ids is None:
        return None
    try:
        produced = ids(value) if callable(ids) else ids[index]
    except Exception:  # noqa: BLE001 — a malformed `ids` must not take the test down
        return None
    return None if produced is None else str(produced)


def _variant_parts(combo: dict, combo_ids: dict, case_kwargs: dict, index: int,
                   explicit=None, combo_pos: dict | None = None, case_pos: dict | None = None) -> str:
    """The inside of a variant's `[...]`, before duplicates are disambiguated.

    Parametrized-fixture values come first, then the test's own `parametrize` values, each in
    declaration order, and an author-supplied id wins over anything generated. `explicit` is the
    whole case's id (a string), none (generate every part), or a list of per-axis
    `(argnames, id-or-None)` — an explicit piece where the author gave one, generated where not."""
    cpos = combo_pos or {}
    kpos = case_pos or {}
    parts = [
        combo_ids.get(k) if combo_ids.get(k) is not None else _id_part(v, k, cpos.get(k, index))
        for k, v in combo.items()
    ]
    if isinstance(explicit, str):
        parts.append(explicit)
    elif explicit is not None:
        for names, piece in explicit:
            if piece is not None:
                parts.append(piece)
            else:
                parts += [_id_part(case_kwargs[n], n, kpos.get(n, index))
                          for n in names if n in case_kwargs]
    else:
        parts += [_id_part(v, k, kpos.get(k, index)) for k, v in case_kwargs.items()]
    return "-".join(parts)


def _disambiguate(parts: list) -> list:
    """Suffix colliding ids the way pytest does — **every** member of a clash, indexed from 0.

    Two cases whose values print alike produce the same text, and an id that collides cannot select.
    pytest turns `as_tool, as_tool` into `as_tool0, as_tool1`; suffixing only the second (leaving the
    first bare) is the obvious alternative and does not match, so a copied id would miss. An id that
    ends in a digit takes a `_` before the index under pytest 8 and later — `1_0`, `1_1` rather than
    `10`, `11` — and does not under 7 (TID-88); the suite's own pytest decides."""
    seen: dict[str, int] = {}
    counts: dict[str, int] = {}
    for text in parts:
        counts[text] = counts.get(text, 0) + 1
    out = []
    underscore = _pytest_major() >= 8
    for text in parts:
        if counts[text] == 1:
            out.append(text)
            continue
        n = seen.get(text, 0)
        seen[text] = n + 1
        sep = "_" if underscore and text and text[-1].isdigit() else ""
        out.append(f"{text}{sep}{n}")
    return out


@functools.lru_cache(maxsize=1)
def _pytest_major() -> int:
    """The major version of the pytest the suite's interpreter has, or the current one's behaviour
    (a large number) when there is none to ask."""
    try:
        import pytest
        return int(str(pytest.__version__).split(".")[0])
    except Exception:  # noqa: BLE001 — no pytest, or an unparsable version
        return 99


def _aggregate(outcomes: list[tuple[str, str]]) -> tuple[str, str]:
    """Collapse parametrization variants into one node outcome (worst wins — `Outcome.worst`)."""
    return Outcome.worst(outcomes)


# --------------------------------------------------------------------------- serve loop
def _preimport(root: str) -> None:
    for current, _dirs, files in _walk_suite(root):  # never warm a dependency's own suite
        for name in files:
            if name.endswith(".py") and (name.startswith("test_") or name.endswith("_test.py")):
                if _is_ignored(os.path.join(current, name)):
                    continue
                rel = os.path.relpath(os.path.join(current, name), root).replace(os.sep, "/")
                if not _module_selected(rel):
                    continue  # this run will not execute it (TID-75)
                try:
                    # Named as `_discover` and execution name it (TID-37); a module-level
                    # `importorskip` is a skip, not a reason to take the pool parent down (TID-48).
                    _import_module(rel)
                except (Exception, *_SKIP_EXCEPTIONS):  # noqa: BLE001
                    pass


def _probe_module_safe(module_key: str, paths: list) -> dict:
    """Sub-interpreter safety probe (ADR-E015, TID-9). Import the module (and thus its transitive
    closure) in a **fresh isolated sub-interpreter** (`concurrent.interpreters`, PEP 734 / per-interpreter
    GIL); if it loads there the module is *safe* to run on the sub-interpreter tier, otherwise not (e.g.
    a single-phase-init C-extension like numpy: `... does not support loading in subinterpreters`).
    Reports `safe=None` when the API is unavailable (< CPython 3.14) so the caller falls back."""
    module_name = _module_name(module_key)
    try:
        from concurrent import interpreters
    except Exception:  # noqa: BLE001 — no sub-interpreter API ⇒ undeterminable, caller falls back to fork
        return {"module": module_key, "safe": None, "reason": "concurrent.interpreters unavailable (CPython < 3.14)"}
    # Process-global state is shared across sub-interpreters: the working directory, the
    # environment `putenv` reaches, signal handlers, the umask. A module whose tests move any of
    # it is unsafe there whatever it imports — click's `monkeypatch.chdir` into a temp directory
    # that another interpreter's teardown then removed left every other interpreter with no
    # working directory at all, 306 errors and a hung pool (TID-104). Found by text, since the
    # import probe cannot see what a test body will do.
    global_touch = _touches_process_globals(module_key)
    if global_touch:
        return {"module": module_key, "safe": False,
                "reason": f"touches process-global state ({global_touch}), shared across sub-interpreters"}
    interp = interpreters.create()
    try:
        interp.exec("import sys\nsys.path[:0] = %r\nimport %s\n" % (paths, module_name))
        return {"module": module_key, "safe": True}
    except Exception as exc:  # noqa: BLE001 — an import failure in the sub-interp ⇒ unsafe (the point)
        text = str(exc).strip()
        reason = text.splitlines()[-1][:200] if text else type(exc).__name__
        return {"module": module_key, "safe": False, "reason": reason}
    finally:
        try:
            interp.close()
        except Exception:  # noqa: BLE001
            pass


_PROCESS_GLOBAL_CALLS = ("chdir(", "isolated_filesystem(", "putenv(", "unsetenv(", "setenv(",
                         "delenv(", "os.environ[", "signal.signal(", "umask(")


def _touches_process_globals(module_key: str) -> str:
    """The first process-global call named in a test module's source, or in the `conftest.py`
    beside it, or `""` — the text check behind the sub-interpreter probe (TID-104)."""
    root = os.path.abspath(_ROOT or ".")
    candidates = [os.path.join(root, module_key),
                  os.path.join(root, os.path.dirname(module_key), "conftest.py")]
    for path in candidates:
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                text = fh.read()
        except OSError:
            continue
        for call in _PROCESS_GLOBAL_CALLS:
            if call in text:
                return f"{call[:-1] if call.endswith('(') else call} in {os.path.basename(path)}"
    return ""


def probe() -> int:
    """`--probe` mode: classify each requested module as sub-interpreter-safe (ADR-E015 detection).
    Same framed pipe as `serve`: reads `{"module": "<rel/path.py>"}` frames, replies
    `{"module", "safe": true|false|null, "reason"?}`. No tests run — this only decides eligibility."""
    root = sys.argv[1]
    global _ROOT
    _ROOT = root
    set_run_root(root)
    _insert_run_root(root)
    paths = list(sys.path)  # the sub-interpreter inherits the same import roots (root + site-packages + …)
    _write_frame(_STDOUT, {"ready": True, "pid": os.getpid()})
    while True:
        req = _read_frame(_STDIN)
        if req is None:
            return 0
        _write_frame(_STDOUT, _probe_module_safe(req["module"], paths))


# Runs INSIDE each pool sub-interpreter (ADR-E015 Phase 2). Builds its own warm Engine — `restore=True`
# gives per-test isolation *within* the interpreter, and the sub-interpreter boundary isolates it from
# the other workers. Pulls tasks off the shared queue, runs them in-process, pushes results back.
_SUBINTERP_WORKER_LOOP = """
import sys
sys.path[:0] = list(_paths)
from tiderace_shim import _shim
_eng = _shim.Engine(_shim._discover(_root), root=_root, no_fork=True, restore=True)
try:
    while True:
        _task = _in_q.get()
        if _task is None:
            break
        try:
            _r = _eng.run(_task["node_id"], _task["style"], _task.get("deadline_ms", 5000),
                          force_no_fork=True)
            # The whole response — expansion, variants, skips, keywords — so the engine reads it
            # as it reads every other transport's (TID-104): an empty expansion is a deselected
            # node, not a pass, and a parametrized node is its cases.
            _r.setdefault("node_id", _task["node_id"])
            _out_q.put(_r)
        except BaseException as _exc:  # noqa: BLE001 — never drop a task's response
            _out_q.put(_shim.errored(_task["node_id"], repr(_exc)))
finally:
    _eng.teardown_all()
"""


def subinterp() -> int:
    """`--subinterp` mode (ADR-E015 Phase 2): run a batch of *safe* tests across a pool of isolated
    sub-interpreters, parallel via per-interpreter GILs (PEP 684). Batch protocol: read one
    `{"batch": [{node_id, style, deadline_ms}, …]}` frame, reply one `{"results": [{node_id, outcome,
    detail}, …]}` frame (input order). The caller only routes sub-interpreter-safe modules here."""

    from concurrent import interpreters  # 3.14+; the caller probes first, so this is expected present

    root = sys.argv[1]
    global _ROOT, _STDOUT
    _ROOT = root
    set_run_root(root)
    # As in `serve()` (TID-103): the protocol owns a private duplicate of fd 1, and fd 1 goes to
    # stderr. Every sub-interpreter's `sys.stdout` is fd 1, so a test that printed put its bytes
    # into the engine's result stream — read as a frame length, waited on forever; click's suite
    # left the engine waiting on an idle pool (TID-104).
    _STDOUT = os.dup(1)
    os.dup2(2, 1)
    _insert_run_root(root)
    paths = list(sys.path)
    workers = max(1, int(_argv_option(sys.argv[2:], "--pool-size") or os.cpu_count() or 4))

    in_q = interpreters.create_queue()
    out_q = interpreters.create_queue()
    pool, threads = [], []
    for _ in range(workers):
        it = interpreters.create()
        it.prepare_main(_paths=tuple(paths), _root=root, _in_q=in_q, _out_q=out_q)
        t = threading.Thread(target=it.exec, args=(_SUBINTERP_WORKER_LOOP,), daemon=True)
        t.start()
        pool.append(it)
        threads.append(t)

    _write_frame(_STDOUT, {"ready": True, "pid": os.getpid()})
    try:
        while True:
            req = _read_frame(_STDIN)
            if req is None:
                return 0
            batch = req.get("batch", [])
            for task in batch:
                in_q.put(task)
            collected = {}
            # Each result is waited for at most the deadline plus the margin the engine allows a
            # silent worker (TID-104). A test blocked in a sub-interpreter cannot be interrupted
            # — no signal lands there, and a watchdog thread cannot be a daemon — so a result
            # that does not come is reported for every task still outstanding, naming them, and
            # this process exits: the engine launches a fresh pool for the next batch.
            budget = max((t.get("deadline_ms", 5000) for t in batch), default=5000) / 1000 + 10
            for _ in range(len(batch)):
                try:
                    r = out_q.get(timeout=budget)
                except Exception:  # noqa: BLE001 — QueueEmpty on timeout, whatever its spelling
                    pending = [t["node_id"] for t in batch if t["node_id"] not in collected]
                    detail = (f"no result within {budget:g}s — a test in this batch blocked in a "
                              f"sub-interpreter, where nothing can interrupt it (TID-104); "
                              f"outstanding: {', '.join(pending)}")
                    for node in pending:
                        collected[node] = errored(node, detail)
                    _write_frame(_STDOUT, {"results": [collected[t["node_id"]] for t in batch]})
                    os._exit(1)  # the blocked interpreter cannot be joined; the pool is done
                collected[r["node_id"]] = r
            _write_frame(_STDOUT, {"results": [collected[t["node_id"]] for t in batch]})
    finally:
        for _ in pool:
            in_q.put(None)  # stop each worker
        for t in threads:
            t.join(timeout=5)


_CLEAN_ROOM = None  # socket to a pristine helper process that re-runs demoted tests (TID-50)


def _start_clean_room(engine: "Engine") -> None:
    """Fork a helper that keeps a pristine copy of this worker's image, for re-running demoted tests.

    A test that disturbs interpreter state is re-run in a fork so the *current* run reports the right
    answer (TID-33). The fork used to be taken from the worker that had just run it — a process now
    holding whatever the test leaked. When what leaked is a **thread**, that fork is the classic POSIX
    hazard: the child gets the one calling thread and inherits every object the others owned, so a
    re-run that waits on a background worker waits forever. Three dask tests in one real suite hung
    exactly there, each burning the full 60-second deadline: 54 of that run's 73 seconds were the
    engine waiting on tests whose work takes milliseconds.

    The helper is forked *before* this worker runs anything, so its image is clean, and it never
    executes test code itself — each request is run in a grandchild it forks on demand. That keeps it
    pristine for the life of the run no matter what the worker does to itself.

    Cheap by construction: one extra process per worker, copy-on-write, idle until something trips."""
    global _CLEAN_ROOM
    if not _FORK_AVAILABLE:
        return

    ours, theirs = socket.socketpair()
    pid = os.fork()
    if pid == 0:  # ---- helper: pristine, and it stays that way ----
        ours.close()
        try:
            _clean_room_serve(theirs, engine)
        finally:
            os._exit(0)  # never unwind past the fork point in a child
    theirs.close()
    _CLEAN_ROOM = ours


def _clean_room_serve(sock, engine: "Engine") -> None:
    """Serve node re-runs from a pristine image: one grandchild per request, nothing run in here.

    Running the node here instead would set its wider-scope fixtures up in *this* process, and a
    fixture that starts a thread would dirty the one image the run has left. Forking per request costs
    a fork — on the rare path this exists for — and keeps the guarantee absolute."""
    fd = sock.fileno()
    while True:
        req = _read_frame(fd)
        if req is None:
            return
        read_fd, write_fd = os.pipe()
        pid = os.fork()
        if pid == 0:  # ---- grandchild: has the clean image, may dirty itself freely ----
            os.close(read_fd)
            try:
                # `_CLEAN_ROOM` is None in here (it is set in the worker only, after this helper was
                # forked), so a demotion inside this run takes the ordinary local fork and cannot
                # bounce back to us.
                resp = engine.run(req["node_id"], req["style"], req.get("deadline_ms", 5000))
                payload = json.dumps(resp).encode()
            except BaseException as exc:  # noqa: BLE001 — report it; never die silently (TID-15)
                payload = json.dumps(errored(req["node_id"], _child_fault_detail(exc)[:4000])).encode()
            try:
                os.write(write_fd, payload)
            except BaseException:  # noqa: BLE001
                pass
            os.close(write_fd)
            os._exit(0)
        os.close(write_fd)
        # Wait no longer than the node's own deadline plus slack: a re-run that hangs here must not
        # hang the worker waiting on it, which is the whole failure this exists to end.
        budget = req.get("deadline_ms", 5000) / 1000.0 + 5.0
        chunks, deadline = [], time.monotonic() + budget
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            ready, _, _ = select.select([read_fd], [], [], remaining)
            if not ready:
                break
            chunk = os.read(read_fd, 65536)
            if not chunk:
                break
            chunks.append(chunk)
        os.close(read_fd)
        if not chunks:
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        try:
            os.waitpid(pid, 0)
        except OSError:
            pass
        raw = b"".join(chunks)
        try:
            resp = json.loads(raw) if raw else None
        except ValueError:
            resp = None
        if resp is None:
            resp = errored(req["node_id"], "timeout (clean re-run produced no result)")
        _write_frame(fd, resp)


class _InProcessTimeout(BaseException):
    """Raised in the main thread by the in-process deadline's signal handler (TID-93), or in the
    test's thread by its watchdog (TID-98). A `BaseException`, so a test's `except Exception`
    cannot swallow it. The watchdog delivers the class, not an instance, so the message lives
    beside it."""

    def __str__(self) -> str:
        return self.args[0] if self.args else _WATCHDOG_MESSAGE


_WATCHDOG_MESSAGE = "timeout on the in-process tier"


class _in_process_deadline:
    """Arm the per-test deadline around an in-process run (TID-93).

    `SIGALRM` through `setitimer`: the handler raises `_InProcessTimeout` in the main thread, which
    ends any wait CPython lets a signal interrupt — a lock, a sleep, a socket read, a thread join.
    A wait it cannot interrupt (inside a C extension that never returns to the interpreter) is the
    engine's job: its read on the worker times out and the worker is killed. A test's own
    `SIGALRM` handler is put back afterwards.

    Where there is no `setitimer` (Windows), or this is not the main thread (signals land only
    there — a sub-interpreter's tests, say), a watchdog thread delivers the same exception with
    `PyThreadState_SetAsyncExc` (TID-98). It lands at the next bytecode boundary: a busy test is
    ended, a wait inside a C call — a `sleep`, a socket read — is not, and that one is the
    engine's read budget's to end. Off when there is no deadline."""

    def __init__(self, deadline_ms: int):
        self.seconds = max(deadline_ms, 0) / 1000.0
        self.armed = False
        self.previous = None
        self.timer = None
        self.target = 0
        self.fired = False

    def __enter__(self):
        if self.seconds <= 0:
            return self
        seconds = self.seconds
        # `TIDERACE_DEADLINE_WATCHDOG=1` takes the watchdog on a platform that has the signal:
        # the way to exercise the Windows path on Linux.
        if (not hasattr(signal, "setitimer")
                or threading.current_thread() is not threading.main_thread()
                or _env_flag("TIDERACE_DEADLINE_WATCHDOG")):
            try:
                return self._arm_watchdog(seconds)
            except Exception as exc:  # noqa: BLE001 — no deadline is better than no test
                _warn(f"in-process deadline not armed: {exc!r}")
                self.timer = None
                return self

        def on_alarm(_signum, _frame):
            raise _InProcessTimeout(
                f"timeout after {seconds:g}s on the in-process tier — the test was still running; "
                f"it forks from the next run on, where the deadline kills instead of interrupts")

        try:
            self.previous = signal.signal(signal.SIGALRM, on_alarm)
            signal.setitimer(signal.ITIMER_REAL, seconds)
            self.armed = True
        except (ValueError, OSError):  # not the main thread after all, or no timers here
            self.armed = False
        return self

    def _arm_watchdog(self, seconds: float):
        global _WATCHDOG_MESSAGE
        _WATCHDOG_MESSAGE = (
            f"timeout after {seconds:g}s on the in-process tier — the test was still running; "
            f"it forks from the next run on, where the deadline kills instead of interrupts")
        self.target = threading.get_ident()
        self.fired = False

        def fire() -> None:
            self.fired = True
            _set_async_exc(self.target, _InProcessTimeout)

        self.timer = threading.Timer(seconds, fire)
        try:
            self.timer.daemon = True  # the setter itself raises in a sub-interpreter (3.14)
        except RuntimeError:  # daemon threads are disabled there: a plain thread, cancelled or
            self.timer.daemon = False  # fired by __exit__, so it never outlives the test
        self.timer.start()
        return self

    def __exit__(self, exc_type, *_exc):
        if self.armed:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, self.previous if self.previous is not None
                          else signal.SIG_DFL)
        if self.timer is not None:
            self.timer.cancel()
            # Fired as the test was ending, with its exception not yet delivered: it would land in
            # the shim's own next bytecode. Withdraw it.
            if self.fired and exc_type is not _InProcessTimeout:
                _set_async_exc(self.target, None)
        return False


def _set_async_exc(thread_ident: int, exc_class) -> None:
    """`PyThreadState_SetAsyncExc`: raise `exc_class` in the thread at its next bytecode boundary;
    `None` withdraws a raise still pending. The class is passed by address and stays alive — it
    is a module global — and `None` is the NULL the API documents."""
    import ctypes

    api = ctypes.pythonapi.PyThreadState_SetAsyncExc
    api.argtypes = [ctypes.c_ulong, ctypes.c_void_p]
    api.restype = ctypes.c_int
    api(ctypes.c_ulong(thread_ident), None if exc_class is None else id(exc_class))


def _clean_room_run(node_id: str, style: str, deadline_ms: int) -> dict | None:
    """Ask the clean room to run a node from a pristine image. `None` if it cannot (caller falls back).

    A helper that has died takes the channel with it; the caller then forks locally, which is what it
    would have done anyway before this existed."""
    global _CLEAN_ROOM
    if _CLEAN_ROOM is None:
        return None
    fd = _CLEAN_ROOM.fileno()
    try:
        _write_frame(fd, {"node_id": node_id, "style": style, "deadline_ms": deadline_ms})
        resp = _read_frame(fd)
    except BaseException:  # noqa: BLE001 — a broken channel costs the clean re-run, not the run
        resp = None
    if resp is None:
        try:
            _CLEAN_ROOM.close()
        except BaseException:  # noqa: BLE001
            pass
        _CLEAN_ROOM = None
    return resp


def _serve_pool(size: int, socket_path: str, engine_args: dict) -> int:
    """Import once in this process, then fork `size` workers that each serve their own connection.

    The pool exists because the per-worker *import* was being paid N times (TID-4). Every wellspring
    in the old pool was an independent `python shim.py`, so an 8-worker run imported the project
    eight times — on a large-import corpus that is ~2.6s of CPU each, ~21s of the ~34s that eight
    workers add. Wall clock hid it, because the imports overlap; a CI runner billed for CPU does not.

    The fix is the same primitive the engine already runs on. `_preimport`/`_discover` happen once,
    *here*, and then `fork()` hands every worker a copy-on-write view of the result for free. Each
    child runs the ordinary `serve` loop unchanged — the only difference is which fd it talks over.

    Workers connect *back* to a listening socket rather than being handed inherited fds. That keeps
    the whole thing dependency-free on both sides: no `SCM_RIGHTS`, no `dup2`, and nothing for the
    Rust side to do beyond accepting `size` connections.
    """
    if size == 0:
        return _serve_pool_persistent(engine_args)
    children = _fork_pool_workers(size, socket_path, engine_args)
    # Parent: nothing to serve. Hold the imported image alive — the children are COW views of it —
    # and reap them so no worker is orphaned if the run is cut short.
    status = 0
    for pid in children:
        _, st = os.waitpid(pid, 0)
        if os.WIFEXITED(st) and os.WEXITSTATUS(st) != 0:
            status = os.WEXITSTATUS(st)
    return status


def _serve_pool_persistent(engine_args: dict) -> int:
    """The warm image (TID-84): import once, then serve the Rust side's requests over stdin/stdout
    for as long as it stays connected — `{"spawn": n, "connect": path}` forks `n` workers that
    connect to `path` and serve one run each; `{"ping": true}` answers `{"pong": true}`; EOF ends
    the process. Every run forks fresh workers from the one imported image, so the second run pays
    no import at all. Finished workers are reaped before each spawn; the rest at exit."""
    _write_frame(_STDOUT, {"ready": True, "pid": os.getpid()})
    live: list = []
    try:
        while True:
            req = _read_frame(_STDIN)
            if req is None:
                break
            live = [pid for pid in live if os.waitpid(pid, os.WNOHANG)[0] == 0]
            if req.get("ping"):
                _write_frame(_STDOUT, {"pong": True, "pid": os.getpid(), "workers": len(live)})
                continue
            n = int(req.get("spawn", 0))
            if n:
                live.extend(_fork_pool_workers(n, req["connect"], engine_args,
                                               req.get("selection")))
                _write_frame(_STDOUT, {"spawned": n})
    finally:
        for pid in live:
            try:
                os.waitpid(pid, 0)
            except ChildProcessError:
                pass
    return 0


def _apply_selection(selection: dict | None) -> None:
    """This run's `-k` / `-m` / `--strict-markers`, in a worker forked off a warm image (TID-90).

    The image read its selection from the environment at start-up — the project's own `addopts`,
    since a persistent parent is launched with none of this run's — and the three are consulted
    per node in `run()`, so setting them after the fork is the whole job. A field left out keeps
    the image's value; an explicit empty string clears it (a `-k ""`)."""
    global _KEYWORD_EXPR, _MARKER_EXPR, _STRICT_MARKS
    if not selection:
        return
    # A field that is absent *or null* keeps the image's value: the daemon serialises the run's
    # selection with every field present, `null` for the ones the run did not give, and reading
    # `null` as "clear it" dropped the project's own `addopts -m` on every `-k` run through the
    # daemon — 32 tests pirn-core's config deselects ran (TID-102).
    if selection.get("keyword") is not None:
        kexpr = selection["keyword"]
        _KEYWORD_EXPR = _compile_selection_tree(kexpr, "-k") if kexpr else None
    if selection.get("marker") is not None:
        expr = selection["marker"]
        _MARKER_EXPR = _compile_marker_expr(expr) if expr else None
    if selection.get("strict_markers"):
        _STRICT_MARKS = True


def _fork_pool_workers(size: int, socket_path: str, engine_args: dict,
                       selection: dict | None = None) -> list:
    """Fork `size` workers off this (imported) process, each connecting to `socket_path` and
    serving the ordinary single-worker loop until its connection closes. Returns their pids.
    `selection` is this run's `-k` / `-m` / `--strict-markers`, applied in each child (TID-90)."""

    children = []
    for _ in range(size):
        pid = os.fork()
        if pid == 0:
            # Child: take a connection of our own and become an ordinary single worker. Anything the
            # parent is holding is irrelevant to us and closing it keeps the parent's exit clean.
            global _STDIN, _STDOUT
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.connect(socket_path)
            _STDIN = _STDOUT = sock.fileno()
            _apply_selection(selection)
            engine = Engine(**engine_args)
            _start_clean_room(engine)  # before a single test runs: the image is pristine now (TID-50)
            _write_frame(_STDOUT, {"ready": True, "pid": os.getpid()})
            try:
                while True:
                    req = _read_frame(_STDIN)
                    if req is None:
                        return 0
                    _write_frame(
                        _STDOUT,
                        engine.run(req["node_id"], req["style"], req.get("deadline_ms", 5000),
                                   req.get("force_no_fork", False), req.get("trusted_pure", False),
                                   req.get("must_fork", False)),
                    )
            finally:
                engine.teardown_all()
                os._exit(0)  # never unwind past the fork point in a child
        children.append(pid)
    return children


def serve() -> int:
    root = sys.argv[1]
    global _ROOT, _STDOUT
    _ROOT = root
    set_run_root(root)
    # The protocol owns a private duplicate of fd 1, and fd 1 itself — what `print()`, a C
    # extension and a subprocess's inherited stdout reach — is pointed at stderr (TID-103). The
    # frames are length-prefixed, so one stray byte on the stream desynchronises it for good: in
    # the warm image (TID-84) the parent's stdout is the daemon's control pipe, shared by
    # inheritance with every forked worker, and the first test that printed put its bytes in
    # front of the next spawn's acknowledgement — read as a frame length and waited on forever.
    # click's suite hung the daemon on its second run, deterministically. A one-shot worker's
    # stdout is the engine's result stream, with the same exposure; its stray output now reaches
    # the engine's stderr instead.
    _STDOUT = os.dup(1)
    os.dup2(2, 1)
    no_fork = "--no-fork" in sys.argv[2:]
    coverage = "--coverage" in sys.argv[2:] or _env_flag("TIDERACE_COVERAGE")
    coverage_lines = ("--coverage-lines" in sys.argv[2:]
                      or _env_flag("TIDERACE_COVERAGE_LINES"))
    purity = "--purity" in sys.argv[2:] or _env_flag("TIDERACE_PURITY")
    restore = "--restore" in sys.argv[2:] or _env_flag("TIDERACE_RESTORE")
    _insert_run_root(root)
    _load_file_deps_cache(root)  # earlier runs' import closures, before anything computes one (TID-82)
    # Ancestor conftests before `_preimport` (TID-19): a root conftest exists to set things up that
    # must already be true when test modules import — env defaults, warning filters, `sys.path`. pytest
    # loads conftests first for the same reason. `_discover` reads the memoised result back.
    modules_file = _argv_option(sys.argv[2:], "--modules")
    if modules_file:
        _select_modules(modules_file)  # before anything is imported (TID-75)
    # `TIDERACE_TIMING=1` prints how long each start-up phase took, to stderr. The start-up is a
    # fixed cost every run pays before a worker exists; knowing which phase is the cost is what
    # decides what to do about it (TID-75).
    _phase = _PhaseTimer()
    _load_ancestor_conftests(root)
    _phase.mark("ancestor conftests")
    _preimport(root)
    _phase.mark("pre-import test modules")
    reg = _discover(root)
    _phase.mark("discover (conftests, fixtures, hooks, marks)")
    if _phase.on and _SKIPPED_AT_DISCOVERY:
        _warn(f"start-up: {_SKIPPED_AT_DISCOVERY} test modules not imported (unselected)")
    engine_args = dict(reg=reg, no_fork=no_fork, root=root, coverage=coverage,
                       coverage_lines=coverage_lines,
                       purity_guard=purity, restore=restore)
    # Pool mode (TID-4): fork the workers from this one imported image instead of importing per
    # worker. Every worker below is created *after* the fork, so its fixture state is its own and
    # the semantics match N separate wellsprings exactly.
    pool = _argv_option(sys.argv[2:], "--pool")
    conn = _argv_option(sys.argv[2:], "--connect")
    if pool and conn:
        return _serve_pool(int(pool), conn, engine_args)
    engine = Engine(**engine_args)
    _start_clean_room(engine)  # before a single test runs: the image is pristine now (TID-50)
    _write_frame(_STDOUT, {"ready": True, "pid": os.getpid()})
    try:
        while True:
            req = _read_frame(_STDIN)
            if req is None:
                return 0
            _write_frame(
                _STDOUT,
                engine.run(req["node_id"], req["style"], req.get("deadline_ms", 5000),
                           req.get("force_no_fork", False), req.get("trusted_pure", False),
                           req.get("must_fork", False)),
            )
    finally:
        engine.teardown_all()


def main() -> int:
    """The shim's modes, dispatched on argv: `<root> --probe`, `<root> --subinterp`, else serve.
    Run through the entry file (`py-shim/shim.py`, staged as `tiderace/_shim/shim.py` in the
    wheel) or as `python -m tiderace_shim`."""
    # `tiderace.builtins` used to reach back into the running shim with `import shim`; TID-111
    # replaced that with `set_context()`, and this alias stays for a package too old to use it,
    # until phase 6e removes the shim's globals altogether.
    sys.modules.setdefault("shim", sys.modules[__name__])
    if "--probe" in sys.argv[2:]:
        return probe()
    if "--subinterp" in sys.argv[2:]:
        return subinterp()
    return serve()
