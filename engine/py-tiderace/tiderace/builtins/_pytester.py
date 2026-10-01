"""`pytester` and `testdir` — pytest's own fixtures for testing pytest plugins, provided here so a
plugin author's suite runs under tiderace (TID-105).

A test that takes `pytester` writes a throwaway pytest project (`makepyfile`, `makeconftest`) and
runs real pytest in it — in-process with `runpytest`, or as a child with `runpytest_subprocess` —
and asserts on the result. anyio's `test_pytest_plugin.py` is eight such tests, and every suite of
a pytest plugin has them. pytest's `Pytester` class does all of that itself; what it needs from
the runner is small: a `request` with a node name, a config that answers `--runpytest`, a place to
register finalizers, a temp-directory factory and a monkeypatch — the last two being builtins
already. The `request` is a small adapter, the config is `RunConfig` with `runpytest` answered,
and `Pytester` is pytest's own, so `runpytest`, `inline_run`, `parseconfig` and the `RunResult`
assertions behave exactly as under pytest. Its `_finalize` restores `sys.modules` and `sys.path`
after the inner session, which is what keeps one worker's later tests unaffected.

`testdir` is the legacy spelling: pytest's `Testdir`, wrapping the same `Pytester`, returning
`py.path.local` objects where the modern one returns `pathlib.Path`.

Both need pytest importable in the suite's interpreter, which a suite that asks for them has.
"""
from __future__ import annotations

from typing import Any, Iterator

import tiderace

from ._config import RunConfig


class PytesterRef:
    """The provided type of `pytester` for tiderace's type-DI. A test names the fixture, and
    pytest's `Pytester` is what it receives; this stands in only so the provider can declare a
    type without importing pytest at module import time."""

    __slots__ = ()


class TestdirRef:
    """As `PytesterRef`, for `testdir`."""

    __slots__ = ()


class _Node:
    __slots__ = ("name", "nodeid")

    def __init__(self, name: str) -> None:
        self.name = name
        self.nodeid = name


class _PytesterRequest:
    """What `Pytester` reads from the requesting test: `.function`, whose `__name__` names the
    directory and the default `makepyfile` module (so `test_x.py` is collected by the inner
    session — a module named after anything else is not), `.node.name` as the fallback, a config
    that answers `getoption("--runpytest")`, and `addfinalizer`, whose callbacks the provider runs
    at teardown, newest first, as pytest does."""

    __slots__ = ("function", "node", "config", "_finalizers")

    def __init__(self, name: str, function, options: dict) -> None:
        self.function = function
        self.node = _Node(name)
        self.config = RunConfig({**options, "runpytest": "inprocess"}, _rootdir())
        self._finalizers: list = []

    def addfinalizer(self, fn) -> None:
        self._finalizers.append(fn)

    def _finalize(self) -> None:
        while self._finalizers:
            self._finalizers.pop()()


def _rootdir() -> str:
    import os

    try:
        import shim

        return getattr(shim, "_ROOT", "") or os.getcwd()
    except Exception:  # noqa: BLE001
        return os.getcwd()


def _declared_options() -> dict:
    try:
        import shim

        return dict(getattr(shim, "_CLI_OPTIONS", {}) or {})
    except Exception:  # noqa: BLE001
        return {}


@tiderace.provides(type=PytesterRef)
def pytester(request, tmp_path_factory, monkeypatch) -> Iterator[Any]:
    """pytest's `pytester`: a `Pytester` over a fresh directory, torn down with its finalizers.
    `request` is the shim's, carrying the node under test, whose name and function name the
    `Pytester` the test receives is built around."""
    try:
        from _pytest.pytester import Pytester
    except ImportError as exc:  # pragma: no cover — a suite asking for pytester has pytest
        raise RuntimeError("the `pytester` fixture needs pytest installed in this interpreter") from exc
    node = getattr(request, "node", None)
    name = getattr(node, "name", None) or "pytester"
    function = getattr(node, "function", None)
    request = _PytesterRequest(name, function, _declared_options())
    instance = Pytester(request, tmp_path_factory, monkeypatch, _ispytest=True)
    try:
        yield instance
    finally:
        request._finalize()


@tiderace.provides(type=TestdirRef)
def testdir(pytester) -> Any:
    """pytest's legacy `testdir`, over the same `Pytester`."""
    from _pytest.legacypath import Testdir

    return Testdir(pytester, _ispytest=True)
