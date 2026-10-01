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
"""
from __future__ import annotations

import os
import shutil
import tempfile
from typing import Any, Iterator

import tiderace

from ._capture import Capfd, Capsys, CaptureResult
from ._config import NullPluginManager, RunConfig
from ._pytester import pytester, testdir
from ._logging import CapLog
from ._monkeypatch import MonkeyPatch
from ._paths import TmpPath
from ._warnings import Warnings

__all__ = [
    "MonkeyPatch",
    "TmpPath",
    "Capsys",
    "Capfd",
    "CapLog",
    "CaptureResult",
    "Warnings",
    "RunConfig",
    "NullPluginManager",
    "monkeypatch",
    "tmp_path",
    "capsys",
    "capfd",
    "caplog",
    "recwarn",
    "tmpdir",
    "tmp_path_factory",
    "tmpdir_factory",
    "TmpPathFactory",
    "pytestconfig",
    "pytester",
    "testdir",
    "providers",
]


@tiderace.provides
def monkeypatch() -> Iterator[MonkeyPatch]:
    """Function-scoped record-and-undo patcher; all mutations reversed at teardown."""
    mp = MonkeyPatch()
    yield mp
    mp.undo()


@tiderace.provides
def tmp_path() -> Iterator[TmpPath]:
    """Function-scoped fresh temp directory; the whole tree is removed at teardown."""
    raw = tempfile.mkdtemp(prefix="tiderace-")
    path = TmpPath(raw)
    yield path
    shutil.rmtree(raw, ignore_errors=True)


@tiderace.provides
def capsys() -> Iterator[Capsys]:
    """Function-scoped sys-level stdout/stderr capture; real streams restored at teardown."""
    cap = Capsys()
    cap._start()
    yield cap
    cap._stop()


@tiderace.provides
def capfd() -> Iterator[Capfd]:
    """Function-scoped fd-level stdout/stderr capture (catches C-ext writes); restored at teardown."""
    cap = Capfd()
    cap._start()
    yield cap
    cap._stop()


@tiderace.provides
def caplog() -> Iterator[CapLog]:
    """Function-scoped log capture; the handler is removed and levels restored at teardown."""
    cap = CapLog()
    cap._start()
    yield cap
    cap._stop()


@tiderace.provides
def recwarn() -> Iterator[Warnings]:
    """Function-scoped warning recorder; the warnings filter is restored at teardown.

    Native form: `w: Warnings`. `recwarn` is pytest's name for the same resource."""
    rec = Warnings()
    rec._start()
    yield rec
    rec._stop()


@tiderace.provides
def tmpdir() -> Iterator[Any]:
    """pytest's legacy `py.path.local` temp directory, for suites that still ask for it.

    `tmp_path` is the modern spelling and the one to migrate to — this exists so a suite written
    before `pathlib` runs unmodified. When no `py.path` implementation is importable the resource
    resolves to a `TmpPath`, which covers the common `str()` / `join()`-free usage rather than
    failing the test outright.
    """
    raw = tempfile.mkdtemp(prefix="tiderace-")
    try:
        from _pytest._py.path import LocalPath  # pytest vendors py.path
        value: Any = LocalPath(raw)
    except Exception:  # noqa: BLE001 — no vendored py.path
        try:
            from py.path import local as LocalPath  # the standalone `py` package

            value = LocalPath(raw)
        except Exception:  # noqa: BLE001 — neither: hand back the modern object
            value = TmpPath(raw)
    yield value
    shutil.rmtree(raw, ignore_errors=True)


@tiderace.provides
def pytestconfig() -> RunConfig:
    """Session-wide run configuration: declared options and the project root.

    Native form: `config: RunConfig`. Values come from what the engine already knows — the project's
    `addopts` and any `pytest_addoption` defaults a conftest declared (TID-14)."""
    return RunConfig(_declared_options(), _rootdir())


def _declared_options() -> dict:
    """Option defaults the shim collected from conftest `pytest_addoption` hooks, if it is driving."""
    try:
        import shim  # the engine's own module, present only when the shim is running this
    except Exception:  # noqa: BLE001 — imported directly (tests of this package); no options known
        return {}
    return dict(getattr(shim, "_CLI_OPTIONS", {}) or {})


def _rootdir() -> str:
    try:
        import shim

        return getattr(shim, "_ROOT", "") or os.getcwd()
    except Exception:  # noqa: BLE001
        return os.getcwd()


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


@tiderace.provides(scope="session")
def tmp_path_factory() -> Iterator[TmpPathFactory]:
    """Session-scoped factory of temp directories (pytest's `tmp_path_factory`); the base tree is
    removed when the session ends. A class-scoped fixture that needs a directory for the whole
    class reaches for this — anyio's file-stream tests do."""
    factory = TmpPathFactory()
    yield factory
    if factory._base is not None:
        shutil.rmtree(str(factory._base), ignore_errors=True)


class _TmpdirFactory:
    """The legacy `tmpdir_factory`: `tmp_path_factory` handing out `py.path.local` when one is
    importable, `TmpPath` otherwise (as `tmpdir` does)."""

    def __init__(self, inner: TmpPathFactory) -> None:
        self._inner = inner

    @staticmethod
    def _legacy(path: TmpPath) -> Any:
        try:
            from _pytest._py.path import LocalPath

            return LocalPath(str(path))
        except Exception:  # noqa: BLE001 — no vendored py.path
            try:
                from py.path import local as LocalPath

                return LocalPath(str(path))
            except Exception:  # noqa: BLE001
                return path

    def getbasetemp(self) -> Any:
        return self._legacy(self._inner.getbasetemp())

    def mktemp(self, basename: str, numbered: bool = True) -> Any:
        return self._legacy(self._inner.mktemp(basename, numbered))


@tiderace.provides(scope="session")
def tmpdir_factory(tmp_path_factory) -> _TmpdirFactory:
    """pytest's legacy `tmpdir_factory`, over `tmp_path_factory`."""
    return _TmpdirFactory(tmp_path_factory)


def providers() -> list:
    """The builtin provider callables, for the shim to register globally (always-available)."""
    return [monkeypatch, tmp_path, capsys, capfd, caplog, recwarn, tmpdir, tmp_path_factory,
            tmpdir_factory, pytestconfig, pytester, testdir]
