"""What a node id names (TID-121): its module, class and function, resolved once.

A node id is `tests/m.py::C::t` — a module key, then the `::` chain. Eleven functions in the shim
used to open with the same four lines (`importlib.import_module(_module_name(_module_key(…)))`,
then `getattr` down the chain, by style); `Engine.run` now resolves a [`Target`] once and hands it
down, and the module-name rule — pytest's rootdir walk, memoised — lives here with it.
"""
from __future__ import annotations

import functools
import importlib
import inspect
import os
import sys
import unittest
from dataclasses import dataclass
from types import ModuleType
from typing import Any

_RUN_ROOT = ""  # the run root (argv[1]); set by the mode that starts the shim, before any import


def set_run_root(root: str) -> None:
    global _RUN_ROOT
    _RUN_ROOT = root


def module_key(node_id: str) -> str:
    """The module path of a node id: 'tests/m.py::C::t' -> 'tests/m.py'."""
    return node_id.partition("::")[0]


def class_method(node_id: str) -> tuple[str, str]:
    """('C', 't') for 'm.py::C::t'."""
    rest = node_id.partition("::")[2]
    cls, _, method = rest.partition("::")
    return cls, method


def module_name(key: str) -> str:
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
    base = os.path.abspath(_RUN_ROOT) if _RUN_ROOT else os.getcwd()
    directory, name = _module_name_walk(key, base)
    if directory not in sys.path:
        sys.path.insert(0, directory)
    return name


@functools.lru_cache(maxsize=None)
def _module_name_walk(key: str, base: str) -> tuple[str, str]:
    """The walk behind `module_name`, memoised: `(import directory, dotted name)`.

    Every node asks for its module's name four or five times — the fixture check, the requested
    params, the marks, the class chain — and each walk is a `stat` per directory level. On a
    5,600-node suite that was 19,000 `stat`s and 61% of the cost of deselecting a node, which is
    what a `-k` run does to every node it does not select (TID-91). The answer depends only on
    which `__init__.py` files exist, which does not change within a run."""
    path = key[:-3] if key.endswith(".py") else key
    absolute = os.path.join(base, path.replace("/", os.sep))
    directory, stem = os.path.split(absolute)
    parts = [stem]
    while os.path.exists(os.path.join(directory, "__init__.py")):
        directory, package = os.path.split(directory)
        parts.insert(0, package)
    return directory, ".".join(parts)


def import_module(key: str) -> ModuleType:
    """The module a module key names, imported (or already imported) under its pytest name —
    the one place the shim imports a test module by key."""
    return importlib.import_module(module_name(key))


@dataclass(frozen=True)
class Target:
    """The live objects a node id names. `cls` is the class of a method node, `func` the function
    or the unbound method; either is `None` only when resolved leniently and absent."""

    node_id: str
    style: str
    module_key: str
    module: ModuleType
    cls: type | None
    func: Any

    @property
    def name(self) -> str:
        """The bare function or method name."""
        return self.node_id.rpartition("::")[2]

    @property
    def owners(self) -> tuple:
        """The chain pytest reads marks from, widest first: module, class, function — minus what
        is absent."""
        return tuple(o for o in (self.module, self.cls, self.func) if o is not None)

    @property
    def is_unittest(self) -> bool:
        """Whether this node's class is really a `unittest.TestCase`, whatever the collector
        decided. The source scan reads base classes as *text*, so `class TestThing(_MyBase)` looks
        like a pytest class even when `_MyBase` derives from `IsolatedAsyncioTestCase`; running it
        as a pytest class never runs `setUp` / `asyncSetUp` (TID-51). The shim holds the live
        class and can simply ask."""
        if self.style != "class_method":
            return self.style == "unittest_method"
        return isinstance(self.cls, type) and issubclass(self.cls, unittest.TestCase)

    @property
    def is_async(self) -> bool:
        """Whether the test body is `async def` **and** tiderace is the thing that must await it.
        A `unittest` class drives its own coroutines — `IsolatedAsyncioTestCase.run()` builds the
        loop and calls `asyncSetUp` around the body — so those are never async-driven from here,
        however the collector labelled them."""
        if self.is_unittest:
            return False
        return inspect.iscoroutinefunction(self.func)


def resolve_target(node_id: str, style: str, *, lenient: bool = False) -> Target:
    """Import the node's module and walk its `::` chain. Strict by default — a missing class or
    function raises, as a test that cannot be found should; `lenient` leaves it `None`, for the
    readers that answer "nothing" rather than fail (marks, class chains)."""
    key = module_key(node_id)
    module = import_module(key)
    cls = func = None
    if style in ("class_method", "unittest_method"):
        cls_name, method = class_method(node_id)
        cls = getattr(module, cls_name, None) if lenient else getattr(module, cls_name)
        if cls is not None:
            func = getattr(cls, method, None) if lenient else getattr(cls, method)
    else:
        name = node_id.partition("::")[2]
        func = getattr(module, name, None) if lenient else getattr(module, name)
    return Target(node_id, style, key, module, cls, func)
