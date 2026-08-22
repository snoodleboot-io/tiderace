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
import textwrap
import time
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


def _module_name(module_key: str) -> str:
    """Importable dotted module name for a module key ('tests/m.py' -> 'tests.m').

    Rooted the way pytest roots it: walk up while the directory is a package
    (has `__init__.py`), and import relative to the first directory that is not.
    That directory is also put on `sys.path`, because the dotted name is only
    resolvable from there.

    Naming relative to the run root instead is wrong whenever a test package is
    named like a stdlib module. `<root>/types/test_x.py` yields `types.test_x`,
    and `types` resolves to the stdlib module — "No module named
    'types.test_x'; 'types' is not a package" — so every test under such a
    directory errors, but only when the run root sits above it. Running that
    directory directly renames the module and the errors vanish, which makes the
    bug look like a batch-size effect rather than a naming one.
    """
    path = module_key[:-3] if module_key.endswith(".py") else module_key
    base = os.path.abspath(_ROOT) if _ROOT else os.getcwd()
    absolute = os.path.join(base, path.replace("/", os.sep))
    directory, stem = os.path.split(absolute)
    parts = [stem]
    while os.path.exists(os.path.join(directory, "__init__.py")):
        directory, package = os.path.split(directory)
        parts.insert(0, package)
    if directory not in sys.path:
        sys.path.insert(0, directory)
    return ".".join(parts)


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


# Command-line options declared by conftests via `pytest_addoption`, as `dest -> default` (TID-14).
# Only defaults live here: tiderace has no way to *pass* a custom flag yet (that is TID-17), so a
# declared option always reads as its default — which is exactly what an opt-in guard like
# `if not request.config.getoption("--real"): pytest.skip(...)` needs to resolve correctly.
_CLI_OPTIONS: dict[str, object] = {}

_NOTSET = object()


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

    def getgroup(self, *_a, **_kw):
        return self  # groups expose the same `addoption`, so the recorder can be its own group

    def addini(self, *_a, **_kw) -> None:
        pass  # ini declarations carry no option value; nothing to record


def _collect_addoption(module) -> None:
    """Run a conftest's `pytest_addoption` hook against the recorder, if it has one."""
    hook = getattr(module, "pytest_addoption", None)
    if hook is None:
        return
    try:
        hook(_OptionRecorder(_CLI_OPTIONS))
    except Exception as exc:  # noqa: BLE001 — a hook we can't model must not abort discovery
        print(f"tiderace: pytest_addoption in {getattr(module, '__file__', '?')} "
              f"could not be recorded: {exc!r}", file=sys.stderr, flush=True)


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
        return None  # ini values are not modelled yet; `None` reads as "unset" at every call site


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
        marks = getattr(owner, "pytestmark", None)
        if marks:
            out.extend(marks)
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
    print(f"tiderace: pytest_collection_modifyitems in "
          f"{getattr(module, '__file__', '?')} failed: {exc!r} — its skips will not be applied",
          file=sys.stderr, flush=True)


class _TestRequest:
    """The `request` a TEST function sees — pytest's `FixtureRequest`, minus the fixture plumbing.

    Distinct from `_Request` (what a *parametrized fixture* sees, which is only `.param`). A test
    asking for `request` overwhelmingly wants `request.config.getoption(...)` to decide whether to
    run, so `config` is the part that has to be real; the identity attributes are cheap and come
    along for free."""

    __slots__ = ("config", "node", "function", "cls", "instance", "param", "fixturenames")

    def __init__(self, node_id: str, func, instance=None):
        self.config = _Config()
        self.node = node_id
        self.function = func
        self.instance = instance
        self.cls = type(instance) if instance is not None else None
        self.param = None  # only a parametrized *fixture* has one; a test's request never does
        self.fixturenames = [p for p in inspect.signature(func).parameters
                             if p not in ("self", "cls")]


def _with_request(func, args: dict, node_id: str, instance=None) -> dict:
    """Add a `request` argument when the test asks for one (TID-14).

    `_bind_by_type` deliberately skips the name `request`, so it never resolves as a provider and
    the test was simply called without it — a `TypeError` about a missing positional argument. It
    is injected here instead of registered as a provider because it needs the node context that
    only the call site has."""
    if "request" in args or "request" not in inspect.signature(func).parameters:
        return args
    return {**args, "request": _TestRequest(node_id, func, instance)}


def _is_fixture(obj) -> bool:
    return hasattr(obj, "_fixture_function_marker") and hasattr(obj, "_fixture_function")


def _is_native_provider(obj) -> bool:
    """A tiderace-native provider (ADR-E012) — carries the tiderace-owned marker, not pytest's."""
    return hasattr(obj, "__tiderace_provider__")


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
        best_spec = None
        for d in self.by_name.get(name, ()):
            if d.location.endswith(".py"):  # module fixture: visible only in its own module
                if d.location != module_key:
                    continue
                spec = 10_000  # most specific
            elif _is_ancestor_dir(d.location, test_dir):
                spec = _location_depth(d.location)  # deeper dir = more specific; above root = negative
            else:
                continue
            if best_spec is None or spec > best_spec:
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


# Files that mark a project root, in pytest's rootdir sense. The nearest ancestor holding one bounds
# how far up `conftest.py` collection reaches (pytest's confcutdir defaults to rootdir).
_ROOTDIR_MARKERS = ("pyproject.toml", "setup.cfg", "tox.ini", "setup.py")

# Ancestor conftests, memoised per run root. They must be *executed once*: a conftest's whole job is
# side effects (env defaults, warning filters, sys.path surgery), and running it twice would apply
# them twice. `serve()` warms this before `_preimport`; `_discover` then reads it back.
_ANCESTOR_CONFTESTS: dict[str, list] = {}


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


def _discover(root: str) -> Registry:
    reg = Registry()
    native: list[tuple] = []  # (provider obj, location) — resolved in a second pass (see below)
    conftests: list = []  # every conftest module, for the collection hooks (TID-20)
    test_modules: list = []  # (module, rel path) — the items those hooks inspect
    # Ancestor conftests first: their fixtures are the widest in the tree, and `serve()` has already
    # executed them ahead of `_preimport` so their side effects precede every test-module import.
    for module, location in _load_ancestor_conftests(root):
        conftests.append(module)
        for obj in vars(module).values():
            if _is_native_provider(obj):
                native.append((obj, location))
            elif _is_fixture(obj):
                reg.add(_fixture_def(obj, location))
    for current, _dirs, files in sorted(os.walk(root)):
        rel_dir = os.path.relpath(current, root)
        rel_dir = "" if rel_dir == "." else rel_dir.replace(os.sep, "/")
        for name in sorted(files):
            if not name.endswith(".py"):
                continue
            path = os.path.join(current, name)
            if name == "conftest.py":
                module, location = _import_conftest(path, rel_dir), rel_dir
                if module is not None:
                    _collect_addoption(module)
                    conftests.append(module)
            elif name.startswith("test_") or name.endswith("_test.py"):
                rel = os.path.relpath(path, root)[:-3].replace(os.sep, ".")
                try:
                    module = importlib.import_module(rel)
                except Exception:  # noqa: BLE001 — a bad module surfaces per-test, not at discovery
                    continue
                location = os.path.relpath(path, root).replace(os.sep, "/")
                test_modules.append((module, location))
            else:
                continue
            if module is None:
                continue
            for obj in vars(module).values():
                if _is_native_provider(obj):  # native-first (ADR-E012); pytest is compat fallback
                    native.append((obj, location))
                elif _is_fixture(obj):
                    reg.add(_fixture_def(obj, location))

    # After every conftest is loaded and every test module imported — the hooks need both, and the
    # marks they inspect only exist once the decorators have run.
    _run_collection_hooks(conftests, test_modules)

    # Native providers wire by type, so provider→provider deps need the FULL type set first: build the
    # type index, then build the defs (a two-pass the name-DI pytest path doesn't need).
    type_index: dict = {}
    for obj, _loc in native:
        spec = obj.__tiderace_provider__
        type_index.setdefault(spec.provides, []).append(spec.name)
    for obj, location in native:
        reg.add(_native_fixture_def(obj, location, type_index))
    _register_builtins(reg)
    return reg


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
        print(f"tiderace: builtin providers unavailable ({exc!r}) — monkeypatch/tmp_path/capsys/"
              f"capfd/caplog will not resolve. Install `tiderace` into this interpreter, or put "
              f"engine/py-tiderace on PYTHONPATH.", file=sys.stderr, flush=True)
        return
    for obj in builtins_pkg.providers():
        reg.add(_native_fixture_def(obj, "", {}))


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
    except Exception as exc:  # noqa: BLE001 — a broken conftest costs its fixtures, not the run
        # Say so. A conftest that fails to import takes its fixtures and its side effects with it, and
        # the tests below it then fail for reasons that name something else entirely — the exact
        # confusion TID-19 was filed about.
        print(f"tiderace: could not import {path}: {exc!r}", file=sys.stderr, flush=True)
        return None


# --------------------------------------------------------------------------- closure
def _closure(reg: Registry, module_key: str, requested: dict, extra: list | None = None) -> list[FixtureDef]:
    """Resolved fixture closure for a test, dependencies-before-dependents (topo). Includes
    requested fixtures (the provider names of `requested`'s param→provider bindings), `extra` provider
    names (e.g. `@tiderace.uses` — set up but not injected), all in-scope autouse fixtures, and their
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
_MISSING = object()
_OPAQUE = object()
# Purity tri-state: a reason string (impure), `None` (measured pure), or `_UNKNOWN_PURITY` (not measured
# — the test forked, ran async, or was trusted-pure so we skipped the snapshot). Only a *measured pure*
# verdict is recordable for the bare-no-fork fast path (ADR-E014 / TID-1).
_UNKNOWN_PURITY = object()

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
            out[k] = copy.deepcopy(v)
        except Exception:  # noqa: BLE001
            out[k] = _OPAQUE
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
    and evicting them would only make the next test pay to import them again."""
    replaced = []
    for name, module in before.items():
        if sys.modules.get(name) is not module:
            sys.modules[name] = module
            replaced.append(name)
    return replaced


def _restorable(module) -> bool:
    """Whether a module's shared state is fully snapshot/restorable (no opaque mutable globals). A test
    in a non-restorable module can't use the no-fork restore path — it must fork for isolation."""
    return _OPAQUE not in _snapshot_shared(module).values()


def _test_is_async(node_id: str, style: str) -> bool:
    """Whether the test body is `async def` (unittest methods are never async-driven here)."""
    if style == "unittest_method":
        return False
    module = importlib.import_module(_module_name(_module_key(node_id)))
    if style == "class_method":
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
        if style == "class_method":
            cls_name, method = _class_method(node_id)
            instance = getattr(module, cls_name)()
            bound = getattr(instance, method)
            result = bound(**_with_request(bound, args, node_id, instance))
        else:
            func = getattr(module, node_id.partition("::")[2])
            result = func(**_with_request(func, args, node_id))
        if inspect.iscoroutine(result):
            await result
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

            mon.use_tool_id(tid, "tiderace")
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
                 coverage: bool = False, purity_guard: bool = False, restore: bool = False):
        self.reg = reg
        self.no_fork = no_fork  # no-COW fallback path (SubprocessWorker / Windows / --no-fork)
        self.root = root  # corpus root, for coverage path relativization
        self.coverage = coverage  # ADR-E006: capture per-test executed-source footprint
        self.purity_guard = purity_guard  # detect shared-state mutation per test (→ pure-test batching)
        self.restore = restore  # snapshot/restore shared state around no-fork tests (isolation w/o fork)
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

    def run(self, node_id: str, style: str, deadline_ms: int, force_no_fork: bool = False,
            trusted_pure: bool = False) -> dict:
        # `force_no_fork`: run THIS test in-process (no fork) — the pure-test fast path (~90× cheaper).
        # The caller asserts it's pure (purity guard); the guard re-checks and flags any escapee.
        module_key = _module_key(node_id)
        if style in ("inherited_methods", "unresolved_class"):
            return self._run_inherited(node_id, deadline_ms, force_no_fork, trusted_pure,
                                       own_too=style == "unresolved_class")
        try:
            requested = self._requested(node_id, style)
            marks = self._marks(node_id, style)
        except Exception as exc:  # noqa: BLE001 — import/collection failure for this node
            return {"node_id": node_id, "outcome": "error",
                    "detail": "".join(traceback.format_exception_only(type(exc), exc))}

        # Native marks first, then anything a `@pytest.mark.skip` or a collection hook decided
        # (TID-20). Both short-circuit BEFORE any fixture setup — a test skipped for a missing
        # backend must not pay to build one.
        skip_reason = _skip_decision(marks) or _MARKER_SKIPS.get(node_id)
        if skip_reason is not None:
            return {"node_id": node_id, "outcome": "skipped", "detail": skip_reason}

        # Split requested params: fixtures (resolved by the graph) vs. bare params filled positionally
        # by @tiderace.cases. Without this, a parametrized test's params look like missing fixtures.
        fixture_requested = {p: t for p, t in requested.items() if self.reg.is_provider(t)}
        case_params = [p for p in requested if p not in fixture_requested]
        # `@tiderace.cases` yields positional variants; `@pytest.mark.parametrize`
        # yields name→value maps (argnames need not follow the signature order).
        raw_cases = self._cases(node_id, style)
        case_kwargs_list = [
            c if isinstance(c, dict) else dict(zip(case_params, c.values))
            for c, _ in raw_cases
        ] or [{}]
        # Author-supplied ids, aligned with `case_kwargs_list`; `None` ⇒ generate one.
        case_ids = [cid for _, cid in raw_cases] or [None]

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
                must_fork = not _restorable(importlib.import_module(_module_name(module_key)))
            except Exception:  # noqa: BLE001 — can't import/inspect ⇒ be safe, fork
                must_fork = True
            if must_fork:
                force_no_fork = False

        uses = self._uses(node_id, style)  # @tiderace.uses: set up by type, not injected (B2)
        closure = _closure(self.reg, module_key, fixture_requested, uses)
        parametrized = [d for d in closure if d.params]
        if parametrized:
            axes = [[(d.name, p) for p in d.params] for d in parametrized]
            combos = [dict(c) for c in itertools.product(*axes)]
        else:
            combos = [{}]

        outcomes: list[tuple[str, str]] = []
        coverage: dict[str, set] = {}  # union of touched lines across all variants of this node
        impurity = None  # first impurity reason across variants (any impure ⇒ the node is impure)
        node_pure = None  # tri-state across variants: None (unmeasured), True (all measured pure), False
        # Per-variant results, reported alongside the aggregate (TID-25). Each case already gets its
        # own `_fork_run`, so collapsing them to one outcome discarded results that had already been
        # paid for — the tally lost the passes, and a node with several failures kept one detail.
        parametrized_node = bool(combos != [{}] or case_kwargs_list != [{}])
        variants: list[dict] = []
        seen_ids: dict[str, int] = {}
        variant_index = 0
        for combo in combos:
            self._sync_wider(closure, node_id)
            for case_pos, case_kwargs in enumerate(case_kwargs_list):
                started = time.perf_counter()
                oc, detail, cov, purity = self._fork_run(
                    node_id, style, fixture_requested, closure, combo, deadline_ms, case_kwargs,
                    force_no_fork, trusted_pure, must_fork)
                if parametrized_node:
                    variant = {
                        "node_id": _variant_id(
                            node_id, combo, case_kwargs, variant_index, seen_ids,
                            case_ids[case_pos],
                        ),
                        "outcome": oc,
                        "detail": detail,
                        "duration_ms": int((time.perf_counter() - started) * 1000),
                    }
                    if cov:
                        variant["coverage"] = {p: sorted(l) for p, l in cov.items()}
                    if purity is None:
                        variant["pure"] = True
                    elif purity is not _UNKNOWN_PURITY:
                        variant["pure"] = False
                    variants.append(variant)
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
        resp = {"node_id": node_id, "outcome": outcome, "detail": detail}
        # Additive and omitted for an unparametrized node, so its frame stays byte-identical.
        if variants:
            resp["variants"] = variants
        if coverage:  # additive field (Phase-3 CONTRACT §6); omitted when capture is off/empty
            resp["coverage"] = {path: sorted(lines) for path, lines in coverage.items()}
        if node_pure is not None:  # additive: purity was measured (guard or restore) — record the verdict
            resp["pure"] = node_pure
            if impurity is not None:
                resp["impurity"] = impurity
        return resp

    def _run_inherited(self, node_id: str, deadline_ms: int, force_no_fork: bool,
                       trusted_pure: bool, own_too: bool = False) -> dict:
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
            module = importlib.import_module(_module_name(module_key))
            cls = getattr(module, cls_name)
        except Exception as exc:  # noqa: BLE001 — a class we can't resolve contributes nothing
            return {"node_id": node_id, "outcome": "error", "expanded": True, "variants": [],
                    "detail": "".join(traceback.format_exception_only(type(exc), exc))}

        # pytest's rule: a `Test*` class, or any `unittest.TestCase` subclass whatever its name.
        # `PackOverridesBuiltinTests` is the second kind, which is why the name scan missed it.
        if own_too and not (
            cls.__name__.startswith("Test") or issubclass(cls, unittest.TestCase)
        ):
            return {"node_id": node_id, "outcome": "passed", "expanded": True, "variants": []}

        own = set() if own_too else set(vars(cls))
        inherited = sorted(
            name for name in dir(cls)
            if name.startswith("test") and name not in own and callable(getattr(cls, name, None))
        )
        # `expanded` says "these variants are the whole answer", so an empty list means this class
        # contributes nothing — distinct from a node that simply isn't parametrized.
        if not inherited:
            return {"node_id": node_id, "outcome": "passed", "expanded": True, "variants": []}

        style = "unittest_method" if issubclass(cls, unittest.TestCase) else "class_method"
        variants = []
        for name in inherited:
            child = f"{module_key}::{cls_name}::{name}"
            started = time.perf_counter()
            res = self.run(child, style, deadline_ms, force_no_fork, trusted_pure)
            # A parametrized inherited method expands again; splice its cases in rather than nesting.
            if res.get("variants"):
                variants.extend(res["variants"])
                continue
            variant = {
                "node_id": child,
                "outcome": res["outcome"],
                "detail": res.get("detail", ""),
                "duration_ms": int((time.perf_counter() - started) * 1000),
            }
            if res.get("coverage"):
                variant["coverage"] = res["coverage"]
            if "pure" in res:
                variant["pure"] = res["pure"]
            variants.append(variant)
        worst = _aggregate([(v["outcome"], v.get("detail", "")) for v in variants])
        return {"node_id": node_id, "outcome": worst[0], "detail": worst[1],
                "expanded": True, "variants": variants}

    def _fork_run(self, node_id, style, requested, closure, combo, deadline_ms, case_kwargs=None,
                  force_no_fork=False, trusted_pure=False, must_fork=False) -> tuple:
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
            try:
                return self._child_exec(node_id, style, requested, closure, combo, case_kwargs,
                                        in_process=True, trusted_pure=trusted_pure)
            except BaseException as exc:  # noqa: BLE001 — any in-process test error → Outcome::Error
                return "error", "".join(traceback.format_exception_only(type(exc), exc)), {}, _UNKNOWN_PURITY

        read_fd, write_fd = os.pipe()
        pid = os.fork()
        if pid == 0:  # ---- CHILD: pristine COW copy with all wider fixtures already warm ----
            os.close(read_fd)
            try:
                outcome, detail, coverage, purity = self._child_exec(
                    node_id, style, requested, closure, combo, case_kwargs)
                payload = {"outcome": outcome, "detail": detail[:4000]}
                if coverage:
                    payload["coverage"] = coverage
                # Carry the purity tri-state across the pipe: pure=True/False when measured (guard on),
                # omitted when unknown (the default forked path measures nothing).
                if purity is None:
                    payload["pure"] = True
                elif purity is not _UNKNOWN_PURITY:
                    payload["pure"] = False
                    payload["impurity"] = purity
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
        ready, _, _ = select.select([read_fd], [], [], deadline_ms / 1000.0)
        if not ready:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
            os.close(read_fd)
            return "error", "timeout", {}, _UNKNOWN_PURITY
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
        # Reconstruct the purity tri-state from the pipe: pure omitted ⇒ unknown; True ⇒ measured pure;
        # False ⇒ impure (with reason).
        if "pure" not in res:
            purity = _UNKNOWN_PURITY
        elif res["pure"]:
            purity = None
        else:
            purity = res.get("impurity") or "impure"
        return res["outcome"], res.get("detail", ""), res.get("coverage", {}), purity

    def _child_exec(self, node_id, style, requested, closure, combo, case_kwargs=None,
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
                return outcome, detail, cov.stop(), _UNKNOWN_PURITY  # async purity not measured
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
            # Purity guard / restore: snapshot shared state right before the body, compare right after.
            # When running in-process (no fork) with `restore`, undo any mutation so the next test is
            # isolated WITHOUT a fork — the snapshot/restore fast path for impure tests too.
            # `trusted_pure` (TID-1): a recorded-pure, unchanged test skips the snapshot entirely and runs
            # BARE no-fork (~90×) — no measurement, no restore. Otherwise snapshot to measure/restore.
            need_snap = (self.purity_guard or (self.restore and in_process)) and not trusted_pure
            mod = importlib.import_module(_module_name(module_key)) if need_snap else None
            before = _snapshot_shared(mod) if mod is not None else None
            env_before = dict(os.environ) if mod is not None else None
            # Tracked independently of the per-module snapshot: `sys.modules` is interpreter-global,
            # and a test can swap a library module without touching a single global of its own
            # (TID-27). Cheap enough to do unconditionally on the in-process path.
            modules_before = dict(sys.modules) if (self.restore and in_process) else None
            outcome, detail = _invoke(node_id, style, test_args)
            purity = _purity_verdict(mod, before, env_before) if mod is not None else _UNKNOWN_PURITY
            if self.restore and in_process and mod is not None and purity is not None:
                _restore_shared(mod, before, env_before)  # undo the mutation → next test isolated
            if modules_before is not None:
                replaced = _restore_modules(modules_before)
                if replaced:
                    # Impure whatever the globals said: `_purity_verdict` cannot see this, and a test
                    # recorded pure would later take the BARE no-fork tier, which skips the snapshot
                    # entirely and would leave the swap in place for good (TID-1).
                    shown = ", ".join(sorted(replaced)[:3])
                    more = f" (+{len(replaced) - 3} more)" if len(replaced) > 3 else ""
                    purity = f"replaced modules in sys.modules: {shown}{more}"
            return outcome, detail, cov.stop(), purity
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
        if style == "class_method":
            cls, method = _class_method(node_id)
            func = getattr(getattr(module, cls), method)
        else:
            func = getattr(module, node_id.partition("::")[2])
        return self.reg.bind_params(func)

    def _marks(self, node_id: str, style: str) -> list:
        """The native marks (`__tiderace_marks__`) on a test, read by attribute — the tiderace-owned
        analogue of pytest's marker read. unittest methods carry none."""
        if style == "unittest_method":
            return []
        module = importlib.import_module(_module_name(_module_key(node_id)))
        if style == "class_method":
            cls, method = _class_method(node_id)
            func = getattr(getattr(module, cls), method)
        else:
            func = getattr(module, node_id.partition("::")[2])
        return list(getattr(func, "__tiderace_marks__", ()))

    def _uses(self, node_id: str, style: str) -> list:
        """Provider names a test depends on via `@tiderace.uses(Type, ...)` — resolved by type, set up
        in the closure but never passed as args (the native `usefixtures`). unittest carries none."""
        if style == "unittest_method":
            return []
        module = importlib.import_module(_module_name(_module_key(node_id)))
        if style == "class_method":
            cls, method = _class_method(node_id)
            func = getattr(getattr(module, cls), method)
        else:
            func = getattr(module, node_id.partition("::")[2])
        names = []
        for t in getattr(func, "__tiderace_uses__", ()):
            provs = self.reg.by_type.get(t, [])
            if len(provs) == 1:  # unambiguous; ambiguity is the author's to disambiguate
                names.append(provs[0])
        return names

    def _cases(self, node_id: str, style: str) -> list:
        """The variants of a test: native `@tiderace.cases`, else `@pytest.mark.parametrize`.

        unittest has neither — pytest cannot parametrize a `TestCase` method
        either, so the early return matches the oracle.
        """
        if style == "unittest_method":
            return []
        module = importlib.import_module(_module_name(_module_key(node_id)))
        if style == "class_method":
            cls, method = _class_method(node_id)
            func = getattr(getattr(module, cls), method)
        else:
            func = getattr(module, node_id.partition("::")[2])
        native = list(getattr(func, "__tiderace_cases__", ()))
        if native:
            return [(c, None) for c in native]  # native cases carry no author-supplied id
        return _parametrize_cases(func)

    def teardown_all(self) -> None:
        while self.active:
            _teardown(self.active.pop().gen)


def _parametrize_cases(func) -> list[dict]:
    """Expand `@pytest.mark.parametrize` on ``func`` into one kwargs dict per case.

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
    """
    marks = [m for m in getattr(func, "pytestmark", ()) if getattr(m, "name", "") == "parametrize"]
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
            if hasattr(entry, "values") and hasattr(entry, "marks"):
                raw = tuple(entry.values)
                explicit = getattr(entry, "id", None)
            elif len(names) == 1:
                raw = (entry,)
            else:
                raw = tuple(entry)
            if explicit is None and ids_kw is not None:
                explicit = _explicit_id(ids_kw, raw, position)
            axis.append((dict(zip(names, raw)), None if explicit is None else str(explicit)))
        axes.append(axis)
    cases: list[tuple] = []
    for combo in itertools.product(*axes):
        merged: dict = {}
        ids = []
        for piece, piece_id in combo:
            merged.update(piece)
            ids.append(piece_id)
        case_id = "-".join(ids) if ids and all(i is not None for i in ids) else None
        cases.append((merged, case_id))
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
        if style == "class_method":
            cls_name, method = _class_method(node_id)
            instance = getattr(module, cls_name)()
            bound = getattr(instance, method)
            _maybe_await(bound(**_with_request(bound, args, node_id, instance)))
            return "passed", ""
        func = getattr(module, node_id.partition("::")[2])
        _maybe_await(func(**_with_request(func, args, node_id)))
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


def _invoke_unittest(module, node_id: str) -> tuple[str, str]:
    """Run one `unittest.TestCase` method with fuller fidelity (Phase 4): honor `setUpClass`/
    `tearDownClass` (which `TestCase.run()` alone does NOT call), and map `@expectedFailure` /
    unexpected-success / `subTest` to the right node outcome.

    Class setup/teardown run per test here (correctness over the once-per-class optimization — the
    fork model would re-run them per child anyway; a class-scope mapping is a later refinement)."""
    cls_name, method = _class_method(node_id)
    cls = module.__dict__[cls_name]
    result = _SkipAwareResult()
    ran_setup = False
    try:
        cls.setUpClass()
        ran_setup = True
        cls(method).run(result)
    except _SKIP_EXCEPTIONS as exc:  # setUpClass may skip the whole class
        return "skipped", str(exc)
    finally:
        if ran_setup:
            try:
                cls.tearDownClass()
            except Exception:  # noqa: BLE001 — teardown error must not mask the test outcome
                pass

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
    if value is None or isinstance(value, (bool, int, float)):
        return str(value)
    name = getattr(value, "__name__", None)  # classes and functions id by name in pytest
    if isinstance(name, str):
        return name
    return f"{argname}{index}"


def _variant_id(node_id: str, combo: dict, case_kwargs: dict, index: int, seen: dict,
                explicit: str | None = None) -> str:
    """`node_id[params]` for one variant, unique within the node (TID-25).

    Parametrized-fixture values come first, then the test's own `parametrize` values, each in
    declaration order. Duplicate ids (two cases whose values print alike) get an index suffix, as
    pytest does — an id that collides is worse than an ugly one, because it cannot select."""
    parts = [_id_part(v, k, index) for k, v in combo.items()]
    if explicit is not None:
        parts.append(explicit)  # the author named this case; theirs wins over anything generated
    else:
        parts += [_id_part(v, k, index) for k, v in case_kwargs.items()]
    if not parts:
        return node_id
    base = f"{node_id}[{'-'.join(parts)}]"
    n = seen.get(base, 0)
    seen[base] = n + 1
    return base if n == 0 else f"{base}{n}"


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


def probe() -> int:
    """`--probe` mode: classify each requested module as sub-interpreter-safe (ADR-E015 detection).
    Same framed pipe as `serve`: reads `{"module": "<rel/path.py>"}` frames, replies
    `{"module", "safe": true|false|null, "reason"?}`. No tests run — this only decides eligibility."""
    root = sys.argv[1]
    global _ROOT
    _ROOT = root
    sys.path.insert(0, root)
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
import shim as _shim
_eng = _shim.Engine(_shim._discover(_root), root=_root, no_fork=True, restore=True)
try:
    while True:
        _task = _in_q.get()
        if _task is None:
            break
        try:
            _r = _eng.run(_task["node_id"], _task["style"], _task.get("deadline_ms", 5000),
                          force_no_fork=True)
            _out_q.put({"node_id": _task["node_id"], "outcome": _r["outcome"],
                        "detail": _r.get("detail", "")})
        except BaseException as _exc:  # noqa: BLE001 — never drop a task's response
            _out_q.put({"node_id": _task["node_id"], "outcome": "error", "detail": repr(_exc)})
finally:
    _eng.teardown_all()
"""


def subinterp() -> int:
    """`--subinterp` mode (ADR-E015 Phase 2): run a batch of *safe* tests across a pool of isolated
    sub-interpreters, parallel via per-interpreter GILs (PEP 684). Batch protocol: read one
    `{"batch": [{node_id, style, deadline_ms}, …]}` frame, reply one `{"results": [{node_id, outcome,
    detail}, …]}` frame (input order). The caller only routes sub-interpreter-safe modules here."""
    import threading

    from concurrent import interpreters  # 3.14+; the caller probes first, so this is expected present

    root = sys.argv[1]
    global _ROOT
    _ROOT = root
    sys.path.insert(0, root)
    paths = list(sys.path)
    workers = max(1, int(os.environ.get("TIDERACE_SUBINTERP_WORKERS") or (os.cpu_count() or 4)))

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
            for _ in range(len(batch)):
                r = out_q.get()
                collected[r["node_id"]] = r
            _write_frame(_STDOUT, {"results": [collected[t["node_id"]] for t in batch]})
    finally:
        for _ in pool:
            in_q.put(None)  # stop each worker
        for t in threads:
            t.join(timeout=5)


def serve() -> int:
    root = sys.argv[1]
    global _ROOT
    _ROOT = root
    no_fork = "--no-fork" in sys.argv[2:]
    coverage = "--coverage" in sys.argv[2:] or os.environ.get("TIDERACE_COVERAGE") == "1"
    purity = "--purity" in sys.argv[2:] or os.environ.get("TIDERACE_PURITY") == "1"
    restore = "--restore" in sys.argv[2:] or os.environ.get("TIDERACE_RESTORE") == "1"
    sys.path.insert(0, root)
    # Ancestor conftests before `_preimport` (TID-19): a root conftest exists to set things up that
    # must already be true when test modules import — env defaults, warning filters, `sys.path`. pytest
    # loads conftests first for the same reason. `_discover` reads the memoised result back.
    _load_ancestor_conftests(root)
    _preimport(root)
    reg = _discover(root)
    engine = Engine(reg, no_fork=no_fork, root=root, coverage=coverage, purity_guard=purity,
                    restore=restore)
    _write_frame(_STDOUT, {"ready": True, "pid": os.getpid()})
    try:
        while True:
            req = _read_frame(_STDIN)
            if req is None:
                return 0
            _write_frame(
                _STDOUT,
                engine.run(req["node_id"], req["style"], req.get("deadline_ms", 5000),
                           req.get("force_no_fork", False), req.get("trusted_pure", False)),
            )
    finally:
        engine.teardown_all()


if __name__ == "__main__":
    if "--probe" in sys.argv[2:]:
        sys.exit(probe())
    if "--subinterp" in sys.argv[2:]:
        sys.exit(subinterp())
    sys.exit(serve())
