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

import importlib
import importlib.util
import inspect
import itertools
import json
import os
import select
import signal
import struct
import sys
import traceback
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
    """A discovered fixture definition + the location it was declared at."""

    __slots__ = ("name", "scope", "params", "autouse", "func", "location", "deps", "is_yield")

    def __init__(self, name, scope, params, autouse, func, location):
        self.name = name
        self.scope = scope if isinstance(scope, str) else "function"
        self.params = list(params) if params else None
        self.autouse = bool(autouse)
        self.func = func
        self.location = location  # module key ('tests/m.py') for module fixtures, or dir for conftest
        sig = list(inspect.signature(func).parameters)
        self.deps = [p for p in sig if p != "request"]
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

    def add(self, fdef: FixtureDef) -> None:
        self.by_name.setdefault(fdef.name, []).append(fdef)

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
                if _is_fixture(obj):
                    reg.add(_fixture_def(obj, location))
    return reg


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
def _closure(reg: Registry, module_key: str, requested: list[str]) -> list[FixtureDef]:
    """Resolved fixture closure for a test, dependencies-before-dependents (topo). Includes
    requested fixtures, all in-scope autouse fixtures, and their transitive deps."""
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
    for name in requested:
        visit(name)
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


class Engine:
    """Parent-side scope state: wider-than-function fixtures live here, inherited by forked children."""

    def __init__(self, reg: Registry):
        self.reg = reg
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
            args = {dep: self._value(dep, _module_key(node_id)) for dep in d.deps}
            value, gen = _setup_fixture(d, args, None)
            self.active.append(_Active(d, key, value, gen))
            live.add(key)

    def run(self, node_id: str, style: str, deadline_ms: int) -> dict:
        module_key = _module_key(node_id)
        try:
            requested = _test_requested(node_id, style)
        except Exception as exc:  # noqa: BLE001 — import/collection failure for this node
            return {"node_id": node_id, "outcome": "error",
                    "detail": "".join(traceback.format_exception_only(type(exc), exc))}

        closure = _closure(self.reg, module_key, requested)
        parametrized = [d for d in closure if d.params]
        if parametrized:
            axes = [[(d.name, p) for p in d.params] for d in parametrized]
            combos = [dict(c) for c in itertools.product(*axes)]
        else:
            combos = [{}]

        outcomes: list[tuple[str, str]] = []
        for combo in combos:
            self._sync_wider(closure, node_id)
            outcomes.append(self._fork_run(node_id, style, requested, closure, combo, deadline_ms))
        outcome, detail = _aggregate(outcomes)
        return {"node_id": node_id, "outcome": outcome, "detail": detail}

    def _fork_run(self, node_id, style, requested, closure, combo, deadline_ms) -> tuple[str, str]:
        read_fd, write_fd = os.pipe()
        pid = os.fork()
        if pid == 0:  # ---- CHILD: pristine COW copy with all wider fixtures already warm ----
            os.close(read_fd)
            try:
                outcome, detail = self._child_exec(node_id, style, requested, closure, combo)
                os.write(write_fd, json.dumps({"outcome": outcome, "detail": detail[:4000]}).encode())
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
            return "error", "timeout"
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
                return "error", f"child killed by signal {os.WTERMSIG(status)}"
            if os.WIFEXITED(status) and os.WEXITSTATUS(status) != 0:
                return "error", f"child exited {os.WEXITSTATUS(status)}"
            return "error", "no result from child"
        res = json.loads(data.decode())
        return res["outcome"], res.get("detail", "")

    def _child_exec(self, node_id, style, requested, closure, combo) -> tuple[str, str]:
        """In the forked child: set up function-scope fixtures (incl. parametrized + reinit-after-fork
        resources, which thus get a FRESH handle per child), run the body, tear down in reverse."""
        module_key = _module_key(node_id)
        local: dict[str, object] = {}
        gens: list = []

        def value_of(name: str):
            if name in local:
                return local[name]
            return self._value(name, module_key)

        try:
            for d in closure:
                if d.rank != 0:
                    continue  # wider scopes are already live in inherited parent memory
                args = {dep: value_of(dep) for dep in d.deps}
                val, gen = _setup_fixture(d, args, combo.get(d.name))
                local[d.name] = val
                gens.append(gen)
            test_args = {name: value_of(name) for name in requested}
            return _invoke(node_id, style, test_args)
        finally:
            for gen in reversed(gens):
                _teardown(gen)

    def teardown_all(self) -> None:
        while self.active:
            _teardown(self.active.pop().gen)


def _test_requested(node_id: str, style: str) -> list[str]:
    """The fixture names a test requests = its signature parameters (minus self/cls)."""
    module = importlib.import_module(_module_name(_module_key(node_id)))
    if style == "unittest_method":
        return []  # unittest methods drive their own setUp/tearDown; no DI in Phase 3
    if style == "pytest_method":
        cls, method = _class_method(node_id)
        func = getattr(getattr(module, cls), method)
        params = list(inspect.signature(func).parameters)
        return [p for p in params if p not in ("self", "cls")]
    func = getattr(module, node_id.partition("::")[2])
    return list(inspect.signature(func).parameters)


def _invoke(node_id: str, style: str, args: dict) -> tuple[str, str]:
    module = importlib.import_module(_module_name(_module_key(node_id)))
    try:
        if style == "unittest_method":
            cls_name, method = _class_method(node_id)
            result = unittest.TestResult()
            module.__dict__[cls_name](method).run(result)
            if result.errors:
                return "error", result.errors[0][1]
            if result.failures:
                return "failed", result.failures[0][1]
            if result.skipped:
                return "skipped", result.skipped[0][1]
            return "passed", ""
        if style == "pytest_method":
            cls_name, method = _class_method(node_id)
            instance = getattr(module, cls_name)()
            getattr(instance, method)(**args)
            return "passed", ""
        getattr(module, node_id.partition("::")[2])(**args)
        return "passed", ""
    except AssertionError as exc:
        return "failed", "".join(traceback.format_exception_only(type(exc), exc))
    except unittest.SkipTest as exc:
        return "skipped", str(exc)
    except Exception as exc:  # noqa: BLE001 — any test error maps to Outcome::Error
        return "error", "".join(traceback.format_exception_only(type(exc), exc))


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
    sys.path.insert(0, root)
    _preimport(root)
    reg = _discover(root)
    engine = Engine(reg)
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
