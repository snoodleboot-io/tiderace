"""What the builtins know about the run they are part of — the root, the options conftests
declared, the ini values — behind one accessor (TID-111).

`pytestconfig`, `pytester` and `caplog` need the project root, the `pytest_addoption` defaults and
the `addini` declarations the shim recorded. Each used to reach for them with `import shim` and
`getattr(shim, "_ROOT")` — three copies of the same back-door, working only because the shim once
aliased itself under that name (gone with TID-124). This module is the only one that knows how a
runner hands that context over: the shim calls [`set_context`] once; everything else calls
[`context`].

With no runner driving — the package imported directly, a REPL, a unit test of a builtin — the
context is the [`NullContext`]: the working directory is the root and nothing is declared, which
is what the three copies each fell back to.
"""
from __future__ import annotations

import os
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


_NULL = NullContext()
_CONTEXT: RunContext | None = None


def set_context(ctx: RunContext | None) -> None:
    """Install the runner's context (`None` clears it). The shim calls this when it registers the
    builtins — in every interpreter it runs in, since a sub-interpreter imports its own copy."""
    global _CONTEXT
    _CONTEXT = ctx


def context() -> RunContext:
    """The current run's context: what the runner installed, else the null context."""
    return _CONTEXT if _CONTEXT is not None else _NULL
