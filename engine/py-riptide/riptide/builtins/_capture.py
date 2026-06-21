"""Output capture builtins — `Capsys` (sys-level) and `Capfd` (fd-level), each injected by type.

Two distinct types so type-DI can request either independently (pytest disambiguates `capsys`/`capfd`
by name; riptide does it by type). Both expose `.readouterr() -> CaptureResult`, draining what was
written since the last read. The builtin providers start capture before the test body and restore the
real streams on teardown."""
from __future__ import annotations

import io
import os
import sys
import tempfile
from typing import NamedTuple


class CaptureResult(NamedTuple):
    """The `(out, err)` pair returned by `.readouterr()` — name-compatible with pytest's."""

    out: str
    err: str


class Capsys:
    """Capture at the Python level by swapping `sys.stdout`/`sys.stderr` for in-memory buffers.

    Catches `print(...)` and anything writing through `sys.stdout`/`sys.stderr`. Does NOT catch writes
    made directly to file descriptors 1/2 by C extensions — use `Capfd` for that."""

    def __init__(self) -> None:
        self._out = io.StringIO()
        self._err = io.StringIO()
        self._saved: tuple | None = None

    def _start(self) -> None:
        self._saved = (sys.stdout, sys.stderr)
        sys.stdout, sys.stderr = self._out, self._err

    def _stop(self) -> None:
        if self._saved is not None:
            sys.stdout, sys.stderr = self._saved
            self._saved = None

    def readouterr(self) -> CaptureResult:
        out, err = self._out.getvalue(), self._err.getvalue()
        self._out.seek(0)
        self._out.truncate()
        self._err.seek(0)
        self._err.truncate()
        return CaptureResult(out, err)


class Capfd:
    """Capture at the file-descriptor level by redirecting fds 1/2 to temp files via `os.dup2`.

    Catches everything `Capsys` does **plus** direct fd writes from C extensions / subprocesses."""

    def __init__(self) -> None:
        self._tmp_out = tempfile.TemporaryFile(mode="w+b")
        self._tmp_err = tempfile.TemporaryFile(mode="w+b")
        self._saved_out: int | None = None
        self._saved_err: int | None = None
        self._read_out = 0  # byte offsets already drained
        self._read_err = 0

    def _start(self) -> None:
        sys.stdout.flush()
        sys.stderr.flush()
        self._saved_out = os.dup(1)
        self._saved_err = os.dup(2)
        os.dup2(self._tmp_out.fileno(), 1)
        os.dup2(self._tmp_err.fileno(), 2)

    def _stop(self) -> None:
        if self._saved_out is not None:
            os.dup2(self._saved_out, 1)
            os.close(self._saved_out)
            self._saved_out = None
        if self._saved_err is not None:
            os.dup2(self._saved_err, 2)
            os.close(self._saved_err)
            self._saved_err = None
        self._tmp_out.close()
        self._tmp_err.close()

    def readouterr(self) -> CaptureResult:
        # fd writes land in the temp files directly; drain whatever is new since the last read.
        out = self._drain(self._tmp_out, "_read_out")
        err = self._drain(self._tmp_err, "_read_err")
        return CaptureResult(out, err)

    def _drain(self, tmp, offset_attr: str) -> str:
        offset = getattr(self, offset_attr)
        tmp.flush()
        tmp.seek(offset)
        data = tmp.read()
        setattr(self, offset_attr, offset + len(data))
        return data.decode(errors="replace")
