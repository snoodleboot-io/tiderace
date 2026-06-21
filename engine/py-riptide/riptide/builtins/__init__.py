"""riptide.builtins — native equivalents of pytest's always-available fixtures. **No pytest.**

The data-backed #1 adoption gap (ROADMAP-v2 B1): pytest builtins are 77% of click's can't-map list.
These are ordinary riptide providers (`@provides`, function-scoped, yield-teardown) injected **by
type**, so a migrated test writes `mp: MonkeyPatch` / `p: TmpPath` / `cap: Capsys` instead of pytest's
name-based `monkeypatch` / `tmp_path` / `capsys`. The shim auto-registers `providers()` globally, so
they are available to every test without an import in the test's own conftest.

    from riptide.builtins import MonkeyPatch, TmpPath, Capsys

    def test_env(mp: MonkeyPatch):
        mp.setenv("API", "x")          # undone automatically at teardown

    def test_writes(p: TmpPath):
        (p / "f.txt").write_text("hi")  # fresh dir, removed at teardown
"""
from __future__ import annotations

import shutil
import tempfile
from typing import Iterator

import riptide

from ._capture import Capfd, Capsys, CaptureResult
from ._monkeypatch import MonkeyPatch
from ._paths import TmpPath

__all__ = [
    "MonkeyPatch",
    "TmpPath",
    "Capsys",
    "Capfd",
    "CaptureResult",
    "monkeypatch",
    "tmp_path",
    "capsys",
    "capfd",
    "providers",
]


@riptide.provides
def monkeypatch() -> Iterator[MonkeyPatch]:
    """Function-scoped record-and-undo patcher; all mutations reversed at teardown."""
    mp = MonkeyPatch()
    yield mp
    mp.undo()


@riptide.provides
def tmp_path() -> Iterator[TmpPath]:
    """Function-scoped fresh temp directory; the whole tree is removed at teardown."""
    raw = tempfile.mkdtemp(prefix="riptide-")
    path = TmpPath(raw)
    yield path
    shutil.rmtree(raw, ignore_errors=True)


@riptide.provides
def capsys() -> Iterator[Capsys]:
    """Function-scoped sys-level stdout/stderr capture; real streams restored at teardown."""
    cap = Capsys()
    cap._start()
    yield cap
    cap._stop()


@riptide.provides
def capfd() -> Iterator[Capfd]:
    """Function-scoped fd-level stdout/stderr capture (catches C-ext writes); restored at teardown."""
    cap = Capfd()
    cap._start()
    yield cap
    cap._stop()


def providers() -> list:
    """The builtin provider callables, for the shim to register globally (always-available)."""
    return [monkeypatch, tmp_path, capsys, capfd]
