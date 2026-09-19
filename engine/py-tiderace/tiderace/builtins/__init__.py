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
from ._config import RunConfig
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
    "monkeypatch",
    "tmp_path",
    "capsys",
    "capfd",
    "caplog",
    "recwarn",
    "tmpdir",
    "pytestconfig",
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


def providers() -> list:
    """The builtin provider callables, for the shim to register globally (always-available)."""
    return [monkeypatch, tmp_path, capsys, capfd, caplog, recwarn, tmpdir, pytestconfig]
