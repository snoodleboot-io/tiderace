"""tiderace.builtins — native equivalents of pytest's always-available fixtures. **No pytest.**

The data-backed #1 adoption gap (ROADMAP-v2 B1): pytest builtins are 77% of click's can't-map list.
These are ordinary tiderace providers (`@provides`, function-scoped, yield-teardown) injected **by
type**, so a migrated test writes `mp: MonkeyPatch` / `p: TmpPath` / `cap: Capsys` instead of pytest's
name-based `monkeypatch` / `tmp_path` / `capsys`. The shim auto-registers `providers()` globally, so
they are available to every test without an import in the test's own conftest.

    from tiderace.builtins import MonkeyPatch, TmpPath, Capsys

    def test_env(mp: MonkeyPatch):
        mp.setenv("API", "x")          # undone automatically at teardown

    def test_writes(p: TmpPath):
        (p / "f.txt").write_text("hi")  # fresh dir, removed at teardown

Adding one is a `@builtin` provider (TID-111): `providers()` and `__all__` are derived from the
registry, and `tiderace migrate` derives its builtin table from the same place.
"""
from __future__ import annotations

import os
import shutil
import tempfile
from typing import Any, Iterator

from ._capture import Capfd, Capsys, CaptureResult
from ._config import NullPluginManager, RunConfig
from ._logging import CapLog
from ._monkeypatch import MonkeyPatch
from ._paths import TmpPath
from ._registry import builtin, providers
from ._runtime import RunContext, context, set_context
from ._warnings import Warnings

# The types a test names. The provider names in `__all__` come from the registry, below.
_TYPES = [
    "MonkeyPatch",
    "TmpPath",
    "Capsys",
    "Capfd",
    "CapLog",
    "CaptureResult",
    "Warnings",
    "RunConfig",
    "NullPluginManager",
    "TmpPathFactory",
    "TmpdirFactory",
    "PytesterRef",
    "TestdirRef",
    "RunContext",
]


@builtin
def monkeypatch() -> Iterator[MonkeyPatch]:
    """Function-scoped record-and-undo patcher; all mutations reversed at teardown."""
    mp = MonkeyPatch()
    yield mp
    mp.undo()


@builtin
def tmp_path() -> Iterator[TmpPath]:
    """Function-scoped fresh temp directory; the whole tree is removed at teardown."""
    raw = tempfile.mkdtemp(prefix="tiderace-")
    path = TmpPath(raw)
    yield path
    shutil.rmtree(raw, ignore_errors=True)


@builtin
def capsys() -> Iterator[Capsys]:
    """Function-scoped sys-level stdout/stderr capture; real streams restored at teardown."""
    with Capsys() as cap:
        yield cap


@builtin
def capfd() -> Iterator[Capfd]:
    """Function-scoped fd-level stdout/stderr capture (catches C-ext writes); restored at teardown."""
    with Capfd() as cap:
        yield cap


@builtin
def caplog() -> Iterator[CapLog]:
    """Function-scoped log capture; the handler is removed and levels restored at teardown."""
    with CapLog() as cap:
        yield cap


@builtin
def recwarn() -> Iterator[Warnings]:
    """Function-scoped warning recorder; the warnings filter is restored at teardown.

    Native form: `w: Warnings`. `recwarn` is pytest's name for the same resource."""
    with Warnings() as rec:
        yield rec


def _legacy_path(path: str | os.PathLike) -> Any:
    """`py.path.local` for `path` when an implementation is importable — pytest's vendored one,
    else the standalone `py` package — otherwise the modern `TmpPath`, which covers the common
    `str()` / `join()`-free usage rather than failing the test outright."""
    try:
        from _pytest._py.path import LocalPath  # pytest vendors py.path
    except Exception:  # noqa: BLE001 — no vendored py.path
        try:
            from py.path import local as LocalPath  # the standalone `py` package
        except Exception:  # noqa: BLE001 — neither: hand back the modern object
            return TmpPath(path)
    return LocalPath(str(path))


@builtin
def tmpdir() -> Iterator[Any]:
    """pytest's legacy `py.path.local` temp directory, for suites that still ask for it.

    `tmp_path` is the modern spelling and the one to migrate to — this exists so a suite written
    before `pathlib` runs unmodified. When no `py.path` implementation is importable the resource
    resolves to a `TmpPath` (see `_legacy_path`).
    """
    raw = tempfile.mkdtemp(prefix="tiderace-")
    yield _legacy_path(raw)
    shutil.rmtree(raw, ignore_errors=True)


class TmpPathFactory:
    """pytest's session-scoped `tmp_path_factory`: `mktemp(basename, numbered=True)` hands out
    directories under one base temp directory per session, `getbasetemp()` is that base."""

    def __init__(self) -> None:
        self._base: TmpPath | None = None

    def getbasetemp(self) -> TmpPath:
        if self._base is None:
            self._base = TmpPath(tempfile.mkdtemp(prefix="tiderace-session-"))
        return self._base

    def mktemp(self, basename: str, numbered: bool = True) -> TmpPath:
        base = self.getbasetemp()
        if not numbered:
            path = TmpPath(os.path.join(str(base), basename))
            os.mkdir(path)
            return path
        n = 0
        while True:  # `basename0`, `basename1`, … — pytest's numbering
            path = TmpPath(os.path.join(str(base), f"{basename}{n}"))
            try:
                os.mkdir(path)
                return path
            except FileExistsError:
                n += 1


@builtin(scope="session")
def tmp_path_factory() -> Iterator[TmpPathFactory]:
    """Session-scoped factory of temp directories (pytest's `tmp_path_factory`); the base tree is
    removed when the session ends. A class-scoped fixture that needs a directory for the whole
    class reaches for this — anyio's file-stream tests do."""
    factory = TmpPathFactory()
    yield factory
    if factory._base is not None:
        shutil.rmtree(str(factory._base), ignore_errors=True)


class TmpdirFactory:
    """The legacy `tmpdir_factory`: `tmp_path_factory` handing out `py.path.local` when one is
    importable, `TmpPath` otherwise (as `tmpdir` does)."""

    def __init__(self, inner: TmpPathFactory) -> None:
        self._inner = inner

    def getbasetemp(self) -> Any:
        return _legacy_path(self._inner.getbasetemp())

    def mktemp(self, basename: str, numbered: bool = True) -> Any:
        return _legacy_path(self._inner.mktemp(basename, numbered))


_TmpdirFactory = TmpdirFactory  # the pre-TID-111 spelling


@builtin(scope="session")
def tmpdir_factory(tmp_path_factory) -> TmpdirFactory:
    """pytest's legacy `tmpdir_factory`, over `tmp_path_factory`."""
    return TmpdirFactory(tmp_path_factory)


@builtin
def pytestconfig() -> RunConfig:
    """Session-wide run configuration: declared options and the project root.

    Native form: `config: RunConfig`. Values come from what the engine already knows — the project's
    `addopts` and any `pytest_addoption` defaults a conftest declared (TID-14)."""
    ctx = context()
    return RunConfig(dict(ctx.options), ctx.rootdir)


# Registered last, as they always were: they need pytest in the suite's interpreter, and they are
# the two builtins defined in their own module (TID-105).
from ._pytester import PytesterRef, TestdirRef, pytester, testdir  # noqa: E402

__all__ = _TYPES + [p.__name__ for p in providers()] + ["builtin", "providers", "context", "set_context"]
