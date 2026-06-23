#!/usr/bin/env python3
"""Wellspring shim — the only Python the engine ships (no pytest *runner* underneath).

Imports the project ONCE (this process is the Wellspring), then drives a native, fork-based
fixture-execution engine (Phase 3, ADR-E003): wider-than-function fixtures (session/package/
module/class) are set up **once in this parent** as tests stream by, and a pristine copy-on-write
child is forked **per test** to set up function-scope fixtures and run the body. Wider-scope setup
cost is paid 1x and inherited by every child via COW; per-test isolation is free.

Protocol with the Rust orchestrator over stdin(0)/stdout(1): length-prefixed (u32 LE) JSON frames
(Phase 2 CONTRACT §3, unchanged).
  startup:   shim -> {"ready": true, "pid": int}
  request:   orchestrator -> {"node_id": str, "style": "pytest_func"|"pytest_method"|
                              "unittest_method", "deadline_ms": int}
  response:  shim -> {"node_id": str, "outcome": "passed|failed|skipped|error", "detail": str}

The fixture **definitions** are authored with `@pytest.fixture` (the corpus is also pytest's
differential oracle), so the engine reads pytest's fixture *marker* metadata — scope / params /
autouse — via `FixtureFunctionDefinition`. It does NOT use pytest's collection or runner: closure
resolution, nearest-override, scope layering, fork-from-warm, parametrization fan-out and yield
teardown are all implemented here. A future native `@riptide.fixture` decorator would replace only
the marker read (ADR-E001).
"""
from __future__ import annotations

import ast
import asyncio
import difflib
import importlib
import importlib.util
import inspect
import itertools
import json
import linecache
import os
import select
import signal
import struct
import sys
import traceback
import typing
import unittest

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


# --------------------------------------------------------------------------- node ids
def _module_key(node_id: str) -> str:
    """The module path of a node id: 'tests/m.py::C::t' -> 'tests/m.py'."""
    return node_id.partition("::")[0]


def _module_name(module_key: str) -> str:
    """Importable dotted module name for a module key ('tests/m.py' -> 'tests.m')."""
    path = module_key[:-3] if module_key.endswith(".py") else module_key
    return path.replace("/", ".").replace(os.sep, ".")


def _class_method(node_id: str) -> tuple[str, str]:
    """('C', 't') for 'm.py::C::t'."""
    rest = node_id.partition("::")[2]
    cls, _, method = rest.partition("::")
    return cls, method


def _test_dir(module_key: str) -> str:
    return os.path.dirname(module_key)


def _is_ancestor_dir(loc: str, test_dir: str) -> bool:
    """True if directory `loc` is `test_dir` or an ancestor of it (''=root, ancestor of all)."""
    if loc == "":
        return True
    return test_dir == loc or test_dir.startswith(loc + "/")


# --------------------------------------------------------------------------- fixture model
class FixtureDef:
    """A discovered fixture definition + the location it was declared at.

    `bindings` maps each of the function's parameter *names* to the *provider name* that satisfies it.
    For pytest-authored fixtures the two are identical (name-DI); for riptide-native providers they may
    differ (the param is wired by **type**, ADR-E012), so callers must build kwargs from `bindings`,
    not from raw parameter names. `deps` (provider names — the registry keys the closure walks) is
    derived from the bindings."""

    __slots__ = (
        "name", "scope", "params", "autouse", "func", "location", "deps", "is_yield",
        "bindings", "provides_type",
    )

    def __init__(self, name, scope, params, autouse, func, location, bindings=None, provides_type=None):
        self.name = name
        self.scope = scope if isinstance(scope, str) else "function"
        self.params = list(params) if params else None
        self.autouse = bool(autouse)
        self.func = func
        self.location = location  # module key ('tests/m.py') for module fixtures, or dir for conftest
        self.provides_type = provides_type  # native: the type this provider is injected by (else None)
        if bindings is None:
            sig = list(inspect.signature(func).parameters)
            bindings = {p: p for p in sig if p != "request"}  # pytest/name-DI: identity
        self.bindings = bindings  # param_name -> provider_name
        self.deps = list(bindings.values())
        self.is_yield = inspect.isgeneratorfunction(func)

    @property
    def rank(self) -> int:
        return _SCOPE_RANK.get(self.scope, 0)

    @property
    def wants_request(self) -> bool:
        return "request" in inspect.signature(self.func).parameters


class _Request:
    """The minimal `request` object a parametrized fixture sees (just `.param`)."""

    __slots__ = ("param",)

    def __init__(self, param):
        self.param = param


def _is_fixture(obj) -> bool:
    return hasattr(obj, "_fixture_function_marker") and hasattr(obj, "_fixture_function")


def _is_native_provider(obj) -> bool:
    """A riptide-native provider (ADR-E012) — carries the riptide-owned marker, not pytest's."""
    return hasattr(obj, "__riptide_provider__")


def _safe_type_hints(func) -> dict:
    try:
        return typing.get_type_hints(func, include_extras=True)
    except Exception:  # noqa: BLE001 — an unresolved annotation ⇒ treat as untyped (name fallback)
        return {}


def _provider_for_type(annotation, type_index: dict):
    """The single provider name registered for `annotation`'s type, or None (0 or >1 ⇒ name fallback).
    `Annotated[T, "name"]` disambiguates. Strict ambiguity errors are the `riptide` package's job at
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
    spec = obj.__riptide_provider__
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


def _fixture_def(obj, location: str) -> FixtureDef:
    marker = obj._fixture_function_marker
    return FixtureDef(
        name=getattr(obj, "name", None) or getattr(marker, "name", None) or obj._fixture_function.__name__,
        scope=getattr(marker, "scope", "function"),
        params=getattr(marker, "params", None),
        autouse=getattr(marker, "autouse", False),
        func=obj._fixture_function,
        location=location,
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

    def resolve(self, name: str, module_key: str) -> FixtureDef | None:
        """Nearest-override: among defs of `name` visible to `module_key`, pick the most specific
        (a same-file module def beats a conftest; a deeper conftest beats a shallower one)."""
        test_dir = _test_dir(module_key)
        best: FixtureDef | None = None
        best_spec = -1
        for d in self.by_name.get(name, ()):
            if d.location.endswith(".py"):  # module fixture: visible only in its own module
                if d.location != module_key:
                    continue
                spec = 10_000  # most specific
            elif _is_ancestor_dir(d.location, test_dir):
                spec = len(d.location.split("/")) if d.location else 0  # deeper dir = more specific
            else:
                continue
            if spec > best_spec:
                best, best_spec = d, spec
        return best

    def autouse_for(self, module_key: str) -> list[FixtureDef]:
        """Every autouse fixture visible to `module_key`, widest scope first."""
        test_dir = _test_dir(module_key)
        out = []
        for defs in self.by_name.values():
            for d in defs:
                if not d.autouse:
                    continue
                visible = d.location == module_key if d.location.endswith(".py") else _is_ancestor_dir(
                    d.location, test_dir
                )
                if visible:
                    out.append(d)
        out.sort(key=lambda d: -d.rank)
        return out


def _discover(root: str) -> Registry:
    reg = Registry()
    native: list[tuple] = []  # (provider obj, location) — resolved in a second pass (see below)
    for current, _dirs, files in sorted(os.walk(root)):
        rel_dir = os.path.relpath(current, root)
        rel_dir = "" if rel_dir == "." else rel_dir.replace(os.sep, "/")
        for name in sorted(files):
            if not name.endswith(".py"):
                continue
            path = os.path.join(current, name)
            if name == "conftest.py":
                module, location = _import_conftest(path, rel_dir), rel_dir
            elif name.startswith("test_") or name.endswith("_test.py"):
                rel = os.path.relpath(path, root)[:-3].replace(os.sep, ".")
                try:
                    module = importlib.import_module(rel)
                except Exception:  # noqa: BLE001 — a bad module surfaces per-test, not at discovery
                    continue
                location = os.path.relpath(path, root).replace(os.sep, "/")
            else:
                continue
            if module is None:
                continue
            for obj in vars(module).values():
                if _is_native_provider(obj):  # native-first (ADR-E012); pytest is compat fallback
                    native.append((obj, location))
                elif _is_fixture(obj):
                    reg.add(_fixture_def(obj, location))

    # Native providers wire by type, so provider→provider deps need the FULL type set first: build the
    # type index, then build the defs (a two-pass the name-DI pytest path doesn't need).
    type_index: dict = {}
    for obj, _loc in native:
        spec = obj.__riptide_provider__
        type_index.setdefault(spec.provides, []).append(spec.name)
    for obj, location in native:
        reg.add(_native_fixture_def(obj, location, type_index))
    _register_builtins(reg)
    return reg


def _register_builtins(reg: Registry) -> None:
    """Register riptide's always-available builtin resources (ROADMAP-v2 B1: monkeypatch/tmp_path/
    capsys/capfd) at the root location (""), so every test can request them — by type (the migrated
    form, `mp: MonkeyPatch`) or by name (the pytest form, `monkeypatch`), with no per-tree import."""
    try:
        import riptide.builtins as builtins_pkg
    except Exception:  # noqa: BLE001 — riptide not importable ⇒ no builtins (pure-pytest fallback)
        return
    for obj in builtins_pkg.providers():
        reg.add(_native_fixture_def(obj, "", {}))


def _import_conftest(path: str, rel_dir: str):
    mod_name = "_fx_conftest_" + (rel_dir.replace("/", "_") or "root")
    try:
        spec = importlib.util.spec_from_file_location(mod_name, path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[mod_name] = module
        spec.loader.exec_module(module)
        return module
    except Exception:  # noqa: BLE001
        return None


# --------------------------------------------------------------------------- closure
def _closure(reg: Registry, module_key: str, requested: dict, extra: list | None = None) -> list[FixtureDef]:
    """Resolved fixture closure for a test, dependencies-before-dependents (topo). Includes
    requested fixtures (the provider names of `requested`'s param→provider bindings), `extra` provider
    names (e.g. `@riptide.uses` — set up but not injected), all in-scope autouse fixtures, and their
    transitive deps."""
    ordered: list[FixtureDef] = []
    seen: set[str] = set()
    visiting: set[str] = set()

    def visit(name: str) -> None:
        if name in seen or name in visiting:
            return
        d = reg.resolve(name, module_key)
        if d is None:
            return  # unknown name (e.g. a non-fixture arg) — the body call will surface it
        visiting.add(name)
        for dep in d.deps:
            visit(dep)
        visiting.discard(name)
        if name not in seen:
            seen.add(name)
            ordered.append(d)

    for d in reg.autouse_for(module_key):
        visit(d.name)
    for provider_name in requested.values():
        visit(provider_name)
    for provider_name in extra or ():
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
    """Run a fixture body up to its first yield (or to completion). Returns (value, gen_or_none)."""
    call_args = dict(args)
    if fdef.wants_request:
        call_args["request"] = _Request(param)
    if fdef.is_yield:
        gen = fdef.func(**call_args)
        return next(gen), gen
    return fdef.func(**call_args), None


def _teardown(gen) -> None:
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
    if fdef.wants_request:
        call_args["request"] = _Request(param)
    if inspect.isasyncgenfunction(fdef.func):
        ag = fdef.func(**call_args)
        return await ag.__anext__(), ("agen", ag)
    if inspect.iscoroutinefunction(fdef.func):
        return await fdef.func(**call_args), None
    if fdef.is_yield:  # a sync yield-fixture used alongside async ones
        gen = fdef.func(**call_args)
        return next(gen), ("gen", gen)
    return fdef.func(**call_args), None


async def _teardown_async(handle) -> None:
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


def _test_is_async(node_id: str, style: str) -> bool:
    """Whether the test body is `async def` (unittest methods are never async-driven here)."""
    if style == "unittest_method":
        return False
    module = importlib.import_module(_module_name(_module_key(node_id)))
    if style == "pytest_method":
        cls, method = _class_method(node_id)
        func = getattr(getattr(module, cls), method, None)
    else:
        func = getattr(module, node_id.partition("::")[2], None)
    return inspect.iscoroutinefunction(func)


async def _invoke_async(node_id: str, style: str, args: dict) -> tuple[str, str]:
    """The async sibling of `_invoke`: call the test, `await` it if it's a coroutine, and map the same
    outcomes (incl. lazy RichDiff on `AssertionError`). Runs inside the per-test event loop, so it must
    `await` directly — never `asyncio.run` (which can't nest)."""
    module = importlib.import_module(_module_name(_module_key(node_id)))
    try:
        if style == "pytest_method":
            cls_name, method = _class_method(node_id)
            result = getattr(getattr(module, cls_name)(), method)(**args)
        else:
            result = getattr(module, node_id.partition("::")[2])(**args)
        if inspect.iscoroutine(result):
            await result
        return "passed", ""
    except AssertionError as exc:
        plain = "".join(traceback.format_exception_only(type(exc), exc))
        rich = _introspect_assertion(exc)
        return "failed", (rich + plain) if rich else plain
    except unittest.SkipTest as exc:
        return "skipped", str(exc)
    except Exception as exc:  # noqa: BLE001 — any test error maps to Outcome::Error
        return "error", "".join(traceback.format_exception_only(type(exc), exc))


class _Coverage:
    """Per-test executed-source capture inside the fork child (ADR-E006, design 11). Uses PEP 669
    `sys.monitoring` LINE events on CPython 3.12+ (disabling each location once seen, so overhead is
    low enough to leave on), falling back to `sys.settrace` on ≤3.11. Records `{rel_source_path:
    set(line)}` for `.py` files under `root` — the test's dependency footprint the DepGraph/cache key
    consume. A no-op when disabled, so the default path is byte-identical to before."""

    _TOOL_ID = 5  # sys.monitoring tool slot (0..5 available); 5 avoids coverage.py/profiler clashes

    def __init__(self, root: str | None, enabled: bool):
        self.enabled = enabled and root is not None
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

            mon.use_tool_id(tid, "riptide")
            mon.register_callback(tid, events.LINE, on_line)
            mon.set_events(tid, events.LINE)
        else:  # ≤3.11 fallback
            def tracer(frame, event, arg):
                if event == "line":
                    fn = frame.f_code.co_filename
                    if self._want(fn):
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
            mon.register_callback(tid, mon.events.LINE, None)
            mon.free_tool_id(tid)
        else:
            sys.settrace(self._prev_trace)
        return self._report()

    def _report(self) -> dict:
        return {os.path.relpath(p, self.root): sorted(lines) for p, lines in self.touched.items()}


class Engine:
    """Parent-side scope state: wider-than-function fixtures live here, inherited by forked children."""

    def __init__(self, reg: Registry, no_fork: bool = False, root: str | None = None,
                 coverage: bool = False):
        self.reg = reg
        self.no_fork = no_fork  # no-COW fallback path (SubprocessWorker / Windows / --no-fork)
        self.root = root  # corpus root, for coverage path relativization
        self.coverage = coverage  # ADR-E006: capture per-test executed-source footprint
        self.active: list[_Active] = []  # in setup order (widest → narrowest)

    def _value(self, name: str, module_key: str):
        # The most-recently set-up active instance of `name` is the one in scope for this test.
        for a in reversed(self.active):
            if a.fdef.name == name:
                return a.value
        raise KeyError(name)

    def _sync_wider(self, closure: list[FixtureDef], node_id: str) -> None:
        """Tear down active wider fixtures whose scope-instance no longer matches this test, then set
        up any missing wider fixtures the test needs (each exactly once per scope-instance)."""
        # Teardown stale from the narrow end (active is ordered widest → narrowest).
        while self.active:
            top = self.active[-1]
            if top.key == _instance_key(top.fdef, node_id):
                break
            _teardown(top.gen)
            self.active.pop()
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

    def run(self, node_id: str, style: str, deadline_ms: int) -> dict:
        module_key = _module_key(node_id)
        try:
            requested = self._requested(node_id, style)
            marks = self._marks(node_id, style)
        except Exception as exc:  # noqa: BLE001 — import/collection failure for this node
            return {"node_id": node_id, "outcome": "error",
                    "detail": "".join(traceback.format_exception_only(type(exc), exc))}

        skip_reason = _skip_decision(marks)
        if skip_reason is not None:  # short-circuit BEFORE any fixture setup
            return {"node_id": node_id, "outcome": "skipped", "detail": skip_reason}

        # Split requested params: fixtures (resolved by the graph) vs. bare params filled positionally
        # by @riptide.cases. Without this, a parametrized test's params look like missing fixtures.
        fixture_requested = {p: t for p, t in requested.items() if self.reg.is_provider(t)}
        case_params = [p for p in requested if p not in fixture_requested]
        case_kwargs_list = [dict(zip(case_params, c.values)) for c in self._cases(node_id, style)] or [{}]

        uses = self._uses(node_id, style)  # @riptide.uses: set up by type, not injected (B2)
        closure = _closure(self.reg, module_key, fixture_requested, uses)
        parametrized = [d for d in closure if d.params]
        if parametrized:
            axes = [[(d.name, p) for p in d.params] for d in parametrized]
            combos = [dict(c) for c in itertools.product(*axes)]
        else:
            combos = [{}]

        outcomes: list[tuple[str, str]] = []
        coverage: dict[str, set] = {}  # union of touched lines across all variants of this node
        for combo in combos:
            self._sync_wider(closure, node_id)
            for case_kwargs in case_kwargs_list:
                oc, detail, cov = self._fork_run(
                    node_id, style, fixture_requested, closure, combo, deadline_ms, case_kwargs)
                outcomes.append((oc, detail))
                for path, lines in cov.items():
                    coverage.setdefault(path, set()).update(lines)
        outcome, detail = _aggregate(outcomes)
        outcome, detail = _apply_xfail(marks, outcome, detail)
        resp = {"node_id": node_id, "outcome": outcome, "detail": detail}
        if coverage:  # additive field (Phase-3 CONTRACT §6); omitted when capture is off/empty
            resp["coverage"] = {path: sorted(lines) for path, lines in coverage.items()}
        return resp

    def _fork_run(self, node_id, style, requested, closure, combo, deadline_ms, case_kwargs=None) -> tuple:
        """Run one (combo, case) variant; returns `(outcome, detail, coverage)` (coverage `{}` unless
        enabled). The child streams its coverage map back through the result pipe alongside the outcome."""
        case_kwargs = case_kwargs or {}
        if self.no_fork:
            # No-COW fallback: run the test in THIS process (no isolation, but the same fixture
            # engine → result-identical outcomes; §8 boundary 3). Function fixtures are set up and
            # torn down per test in-process; wider scopes still live once in the parent.
            try:
                return self._child_exec(node_id, style, requested, closure, combo, case_kwargs)
            except BaseException as exc:  # noqa: BLE001 — any in-process test error → Outcome::Error
                return "error", "".join(traceback.format_exception_only(type(exc), exc)), {}

        read_fd, write_fd = os.pipe()
        pid = os.fork()
        if pid == 0:  # ---- CHILD: pristine COW copy with all wider fixtures already warm ----
            os.close(read_fd)
            try:
                outcome, detail, coverage = self._child_exec(
                    node_id, style, requested, closure, combo, case_kwargs)
                payload = {"outcome": outcome, "detail": detail[:4000]}
                if coverage:
                    payload["coverage"] = coverage
                os.write(write_fd, json.dumps(payload).encode())
            except BaseException:  # noqa: BLE001 — never hang the child on the way out
                pass
            finally:
                os.close(write_fd)
                os._exit(0)

        os.close(write_fd)
        ready, _, _ = select.select([read_fd], [], [], deadline_ms / 1000.0)
        if not ready:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
            os.close(read_fd)
            return "error", "timeout", {}
        data = b""
        while True:
            chunk = os.read(read_fd, 65536)
            if not chunk:
                break
            data += chunk
        os.close(read_fd)
        _, status = os.waitpid(pid, 0)
        if not data:
            if os.WIFSIGNALED(status):
                return "error", f"child killed by signal {os.WTERMSIG(status)}", {}
            if os.WIFEXITED(status) and os.WEXITSTATUS(status) != 0:
                return "error", f"child exited {os.WEXITSTATUS(status)}", {}
            return "error", "no result from child", {}
        res = json.loads(data.decode())
        return res["outcome"], res.get("detail", ""), res.get("coverage", {})

    def _child_exec(self, node_id, style, requested, closure, combo, case_kwargs=None) -> tuple:
        """In the forked child: set up function-scope fixtures (incl. parametrized + reinit-after-fork
        resources, which thus get a FRESH handle per child), run the body, tear down in reverse.
        `case_kwargs` are the @riptide.cases values bound to the test's bare params. Returns
        `(outcome, detail, coverage)` where coverage is `{rel_path: [lines]}` (empty unless enabled)."""
        module_key = _module_key(node_id)
        local: dict[str, object] = {}
        gens: list = []

        def value_of(name: str):
            if name in local:
                return local[name]
            return self._value(name, module_key)

        cov = _Coverage(self.root, self.coverage)
        cov.start()  # capture the per-test footprint: fixture setup + body, this test only (ADR-E006)
        # B5: async test body or any function-scope async provider ⇒ run setup+body+teardown on ONE
        # event loop (objects created on a loop must be awaited on the same loop). Sync path untouched.
        if _test_is_async(node_id, style) or any(
            _is_async_fixture(d.func) for d in closure if d.rank == 0
        ):
            try:
                outcome, detail = asyncio.run(
                    self._child_exec_async(node_id, style, requested, closure, combo, case_kwargs)
                )
                return outcome, detail, cov.stop()
            finally:
                cov.stop()
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
            outcome, detail = _invoke(node_id, style, test_args)
            return outcome, detail, cov.stop()
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

    def _requested(self, node_id: str, style: str) -> dict:
        """The resources a test requests, as `param_name -> provider_name` bindings. Native params
        resolve by **type** (ADR-E012); untyped params fall back to name (the pytest path), so a
        pytest-authored test with `(db, cache)` args binds identically to before."""
        module = importlib.import_module(_module_name(_module_key(node_id)))
        if style == "unittest_method":
            return {}  # unittest methods drive their own setUp/tearDown; no DI in Phase 3
        if style == "pytest_method":
            cls, method = _class_method(node_id)
            func = getattr(getattr(module, cls), method)
        else:
            func = getattr(module, node_id.partition("::")[2])
        return self.reg.bind_params(func)

    def _marks(self, node_id: str, style: str) -> list:
        """The native marks (`__riptide_marks__`) on a test, read by attribute — the riptide-owned
        analogue of pytest's marker read. unittest methods carry none."""
        if style == "unittest_method":
            return []
        module = importlib.import_module(_module_name(_module_key(node_id)))
        if style == "pytest_method":
            cls, method = _class_method(node_id)
            func = getattr(getattr(module, cls), method)
        else:
            func = getattr(module, node_id.partition("::")[2])
        return list(getattr(func, "__riptide_marks__", ()))

    def _uses(self, node_id: str, style: str) -> list:
        """Provider names a test depends on via `@riptide.uses(Type, ...)` — resolved by type, set up
        in the closure but never passed as args (the native `usefixtures`). unittest carries none."""
        if style == "unittest_method":
            return []
        module = importlib.import_module(_module_name(_module_key(node_id)))
        if style == "pytest_method":
            cls, method = _class_method(node_id)
            func = getattr(getattr(module, cls), method)
        else:
            func = getattr(module, node_id.partition("::")[2])
        names = []
        for t in getattr(func, "__riptide_uses__", ()):
            provs = self.reg.by_type.get(t, [])
            if len(provs) == 1:  # unambiguous; ambiguity is the author's to disambiguate
                names.append(provs[0])
        return names

    def _cases(self, node_id: str, style: str) -> list:
        """The native `@riptide.cases` variants on a test, read by attribute. unittest has none."""
        if style == "unittest_method":
            return []
        module = importlib.import_module(_module_name(_module_key(node_id)))
        if style == "pytest_method":
            cls, method = _class_method(node_id)
            func = getattr(getattr(module, cls), method)
        else:
            func = getattr(module, node_id.partition("::")[2])
        return list(getattr(func, "__riptide_cases__", ()))

    def teardown_all(self) -> None:
        while self.active:
            _teardown(self.active.pop().gen)


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
    module = importlib.import_module(_module_name(_module_key(node_id)))
    try:
        if style == "unittest_method":
            return _invoke_unittest(module, node_id)
        if style == "pytest_method":
            cls_name, method = _class_method(node_id)
            instance = getattr(module, cls_name)()
            _maybe_await(getattr(instance, method)(**args))
            return "passed", ""
        _maybe_await(getattr(module, node_id.partition("::")[2])(**args))
        return "passed", ""
    except AssertionError as exc:
        plain = "".join(traceback.format_exception_only(type(exc), exc))
        rich = _introspect_assertion(exc)  # lazy: only a FAILED assert pays this (ADR-E009)
        return "failed", (rich + plain) if rich else plain
    except unittest.SkipTest as exc:
        return "skipped", str(exc)
    except Exception as exc:  # noqa: BLE001 — any test error maps to Outcome::Error
        return "error", "".join(traceback.format_exception_only(type(exc), exc))


def _maybe_await(result):
    """Drive an `async def test_*` to completion (Phase 4). A sync test returns a plain value (passed
    straight through); a coroutine is run on a fresh event loop per test — isolation is free since each
    test is its own fork child. Async *providers* are deferred to Track B (B5)."""
    if inspect.iscoroutine(result):
        asyncio.run(result)


def _invoke_unittest(module, node_id: str) -> tuple[str, str]:
    """Run one `unittest.TestCase` method with fuller fidelity (Phase 4): honor `setUpClass`/
    `tearDownClass` (which `TestCase.run()` alone does NOT call), and map `@expectedFailure` /
    unexpected-success / `subTest` to the right node outcome.

    Class setup/teardown run per test here (correctness over the once-per-class optimization — the
    fork model would re-run them per child anyway; a class-scope mapping is a later refinement)."""
    cls_name, method = _class_method(node_id)
    cls = module.__dict__[cls_name]
    result = unittest.TestResult()
    ran_setup = False
    try:
        cls.setUpClass()
        ran_setup = True
        cls(method).run(result)
    except unittest.SkipTest as exc:  # setUpClass may skip the whole class
        return "skipped", str(exc)
    finally:
        if ran_setup:
            try:
                cls.tearDownClass()
            except Exception:  # noqa: BLE001 — teardown error must not mask the test outcome
                pass

    if result.errors:
        return "error", result.errors[0][1]
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
        "assertion failed (riptide rich diff):",
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


def _aggregate(outcomes: list[tuple[str, str]]) -> tuple[str, str]:
    """Collapse parametrization variants into one node outcome (worst wins)."""
    order = {"error": 3, "failed": 2, "skipped": 1, "passed": 0}
    worst = max(outcomes, key=lambda o: order.get(o[0], 0))
    return worst


# --------------------------------------------------------------------------- serve loop
def _preimport(root: str) -> None:
    for current, _dirs, files in os.walk(root):
        for name in files:
            if name.endswith(".py") and (name.startswith("test_") or name.endswith("_test.py")):
                rel = os.path.relpath(os.path.join(current, name), root)[:-3]
                try:
                    importlib.import_module(rel.replace(os.sep, "."))
                except Exception:  # noqa: BLE001
                    pass


def serve() -> int:
    root = sys.argv[1]
    no_fork = "--no-fork" in sys.argv[2:]
    coverage = "--coverage" in sys.argv[2:] or os.environ.get("RIPTIDE_COVERAGE") == "1"
    sys.path.insert(0, root)
    _preimport(root)
    reg = _discover(root)
    engine = Engine(reg, no_fork=no_fork, root=root, coverage=coverage)
    _write_frame(_STDOUT, {"ready": True, "pid": os.getpid()})
    try:
        while True:
            req = _read_frame(_STDIN)
            if req is None:
                return 0
            _write_frame(
                _STDOUT,
                engine.run(req["node_id"], req["style"], req.get("deadline_ms", 5000)),
            )
    finally:
        engine.teardown_all()


if __name__ == "__main__":
    sys.exit(serve())
