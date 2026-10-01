"""What the builtins know about the run they are part of — the root, the options conftests
declared, the ini values — behind one accessor (TID-111).

`pytestconfig`, `pytester` and `caplog` need the project root, the `pytest_addoption` defaults and
the `addini` declarations the shim recorded. Each used to reach for them with `import shim` and
`getattr(shim, "_ROOT")` — three copies of the same back-door, working only because the shim
aliases itself into `sys.modules["shim"]`. This module is the only one that knows how a runner
hands that context over: the shim calls [`set_context`] once; everything else calls [`context`].

With no runner driving — the package imported directly, a REPL, a unit test of a builtin — the
context is the [`NullContext`]: the working directory is the root and nothing is declared, which
is what the three copies each fell back to.
"""
from __future__ import annotations

import os
import sys
from typing import Any, Mapping, Protocol


class RunContext(Protocol):
    """What a runner exposes to the builtins."""

    @property
    def rootdir(self) -> str:
        """The run root — `config.rootpath`."""

    @property
    def options(self) -> Mapping[str, Any]:
        """Command-line options conftests declared via `pytest_addoption`, as `dest -> default`."""

    def ini(self, name: str) -> Any:
        """`config.getini(name)`: the configured value, else the declared default, else `None`."""


class NullContext:
    """No runner driving."""

    __slots__ = ()

    @property
    def rootdir(self) -> str:
        return os.getcwd()

    @property
    def options(self) -> Mapping[str, Any]:
        return {}

    def ini(self, name: str) -> Any:
        return None


class ModuleContext:
    """The context a running shim exposes: its module's `_ROOT`, `_CLI_OPTIONS` and `_ini_value`,
    read live — the shim sets them after it starts, so they are looked up on each use, never
    copied."""

    __slots__ = ("_module",)

    def __init__(self, module) -> None:
        self._module = module

    @property
    def rootdir(self) -> str:
        return getattr(self._module, "_ROOT", "") or os.getcwd()

    @property
    def options(self) -> Mapping[str, Any]:
        return dict(getattr(self._module, "_CLI_OPTIONS", {}) or {})

    def ini(self, name: str) -> Any:
        reader = getattr(self._module, "_ini_value", None)
        return reader(name) if reader is not None else None


_NULL = NullContext()
_CONTEXT: RunContext | None = None


def set_context(ctx: RunContext | None) -> None:
    """Install the runner's context (`None` clears it). The shim calls this when it registers the
    builtins — in every interpreter it runs in, since a sub-interpreter imports its own copy."""
    global _CONTEXT
    _CONTEXT = ctx


def context() -> RunContext:
    """The current run's context: what the runner installed, else — for a shim too old to install
    one, found by the alias it still sets — its module, else the null context."""
    if _CONTEXT is not None:
        return _CONTEXT
    legacy = sys.modules.get("shim")
    if legacy is not None and hasattr(legacy, "_ini_value"):
        return ModuleContext(legacy)
    return _NULL
