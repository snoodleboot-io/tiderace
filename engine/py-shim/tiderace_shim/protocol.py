"""The shim's one transport (TID-122): length-prefixed JSON frames, the request loop, and the
children it forks.

The shim talks to the engine, and to its own children, over frames of `u32 LE length + JSON`.
The loop that reads a request, dispatches it and writes the reply used to be written five times,
the frame reader four, and a forked child answered in two wire formats — raw JSON until EOF for
the fork tier and the clean room's grandchild, frames for the module child — each with its own
fork / pipe / select / kill / reap sequence. This module is the only place that reads or writes a
frame, waits on a descriptor, forks, or reaps.

* [`Transport`] — a read fd and a write fd; [`Transport.serve`] is *the* request loop,
  [`Transport.stdio`] the engine's pipe with the stdout→stderr redirect (TID-103) done once.
* [`spawn`] — `os.fork()`, once: the child runs a callable and `_exit`s, the parent gets the pid.
* [`run_child`] — fork a child that computes one reply and writes one frame; the parent waits up
  to a deadline, kills on overrun, reaps, and reports exactly what came back ([`ChildResult`]).
* [`reap`] / [`end_child`] — how a child's exit is collected, with or without a kill.
"""
from __future__ import annotations

import json
import os
import select
import signal
import struct
import time
from dataclasses import dataclass
from typing import Any, Callable

# A child that ran the test but could not serialise or write its result frame exits with this, so
# the parent reports a documented code rather than a bare "exited N" (TID-15).
EXIT_UNREPORTABLE = 199


# --------------------------------------------------------------------------- frames
def read_exactly(fd: int, n: int, deadline_at: float | None = None) -> tuple[bytes | None, bool]:
    """`n` bytes from `fd`: `(bytes, False)`; `(None, False)` on EOF; with a monotonic
    `deadline_at`, `(None, True)` when it passes first. The deadline covers the WHOLE read, not
    just the first byte (TID-31): a child that wrote part of its frame and then hung used to
    satisfy a `select` on the first byte and block the parent in `os.read` for good."""
    buf = b""
    while len(buf) < n:
        if deadline_at is not None:
            remaining = deadline_at - time.monotonic()
            if remaining <= 0 or not select.select([fd], [], [], remaining)[0]:
                return None, True
        chunk = os.read(fd, n - len(buf))
        if not chunk:
            return None, False
        buf += chunk
    return buf, False


def read_frame(fd: int) -> dict | None:
    """One frame, or `None` at EOF."""
    header, _ = read_exactly(fd, 4)
    if header is None:
        return None
    (length,) = struct.unpack("<I", header)
    payload, _ = read_exactly(fd, length)
    if payload is None:
        return None
    return json.loads(payload.decode("utf-8"))


def read_frame_by(fd: int, deadline_at: float) -> tuple[bytes | None, bool]:
    """One frame's payload by `deadline_at`; `(None, True)` on timeout, `(None, False)` on EOF."""
    header, timed_out = read_exactly(fd, 4, deadline_at)
    if header is None:
        return None, timed_out
    (length,) = struct.unpack("<I", header)
    return read_exactly(fd, length, deadline_at)


def write_frame(fd: int, obj: Any) -> None:
    payload = json.dumps(obj).encode("utf-8")
    os.write(fd, struct.pack("<I", len(payload)) + payload)


# --------------------------------------------------------------------------- the loop
class Transport:
    """A frame channel over a read fd and a write fd (the same one, for a socket)."""

    __slots__ = ("read_fd", "write_fd")

    def __init__(self, read_fd: int, write_fd: int) -> None:
        self.read_fd, self.write_fd = read_fd, write_fd

    @classmethod
    def stdio(cls, *, redirect_stdout: bool = True) -> "Transport":
        """The engine's pipe: stdin in, stdout out. The protocol owns a private duplicate of fd 1,
        and fd 1 itself — what `print()`, a C extension and a subprocess's inherited stdout reach
        — is pointed at stderr (TID-103). The frames are length-prefixed, so one stray byte on
        the stream desynchronises it for good: in the warm image (TID-84) the parent's stdout is
        the daemon's control pipe, shared by inheritance with every forked worker, and the first
        test that printed put its bytes in front of the next spawn's acknowledgement — read as a
        frame length and waited on forever. click's suite hung the daemon on its second run,
        deterministically. A one-shot worker's stdout is the engine's result stream, with the
        same exposure; its stray output now reaches the engine's stderr instead."""
        if not redirect_stdout:
            return cls(0, 1)
        out = os.dup(1)
        os.dup2(2, 1)
        return cls(0, out)

    @classmethod
    def over(cls, sock) -> "Transport":
        """A connected socket, both ways."""
        return cls(sock.fileno(), sock.fileno())

    def send(self, obj: Any) -> None:
        write_frame(self.write_fd, obj)

    def recv(self) -> dict | None:
        return read_frame(self.read_fd)

    def ready(self, **fields: Any) -> None:
        """The start-up frame: `{"ready": true, "pid": …}`."""
        self.send({"ready": True, "pid": os.getpid(), **fields})

    def serve(self, handler: Callable[[dict], Any]) -> None:
        """The one request loop: a frame in, `handler(request)` out, until EOF. A handler that
        returns `None` has answered (or declined) on its own."""
        while True:
            request = self.recv()
            if request is None:
                return
            reply = handler(request)
            if reply is not None:
                self.send(reply)

    def request(self, obj: Any, deadline_at: float | None = None) -> tuple[bytes | None, bool]:
        """Send `obj` and wait for the reply's payload — by `deadline_at` when given — as
        `read_frame_by` reports it."""
        self.send(obj)
        if deadline_at is None:
            header, _ = read_exactly(self.read_fd, 4)
            if header is None:
                return None, False
            (length,) = struct.unpack("<I", header)
            return read_exactly(self.read_fd, length)
        return read_frame_by(self.read_fd, deadline_at)


# --------------------------------------------------------------------------- children
def spawn(child: Callable[[], int | None]) -> int:
    """`os.fork()`, the one call: the child runs `child()` and `_exit`s with what it returns (0 by
    default) — never unwinding past the fork point — and the parent gets the pid."""
    pid = os.fork()
    if pid == 0:
        code = 0
        try:
            code = child() or 0
        finally:
            os._exit(code)
    return pid


def exit_text(status: int) -> str:
    """How a reaped process ended, for a diagnostic."""
    if os.WIFSIGNALED(status):
        return f"killed by signal {os.WTERMSIG(status)}"
    code = os.WEXITSTATUS(status) if os.WIFEXITED(status) else None
    if code == EXIT_UNREPORTABLE:
        return "it ran the test but could not serialise its result frame"
    return f"exited {code}"


def exit_code(status: int) -> int | None:
    """The exit code of a reaped process, or `None` when a signal ended it."""
    return os.WEXITSTATUS(status) if os.WIFEXITED(status) else None


def reap(pid: int, *, kill: bool = False) -> int:
    """Wait for `pid` — after a `SIGKILL` when asked — and return its wait status."""
    if kill:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    _, status = os.waitpid(pid, 0)
    return status


def end_child(pid: int, grace_s: float) -> int:
    """Give a child `grace_s` to exit on its own (its request pipe has been closed), then kill it;
    return its wait status."""
    deadline_at = time.monotonic() + grace_s
    while time.monotonic() < deadline_at:
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            return status
        time.sleep(0.01)
    return reap(pid, kill=True)


@dataclass(frozen=True)
class ChildResult:
    """What came back from a [`run_child`]: the reply when there was one; `timed_out` when the
    deadline passed first (the child was killed); `status` the wait status (`None` only when the
    pipe failed before the child could be reaped); `received` the bytes read before the end, so a
    child that wrote half a frame is told apart from one that wrote nothing; `error` the decode
    failure of a corrupt frame."""

    reply: dict | None
    timed_out: bool
    status: int | None
    received: int
    error: str | None

    @property
    def exit_text(self) -> str:
        return exit_text(self.status) if self.status is not None else "no exit status"

    @property
    def exit_code(self) -> int | None:
        return exit_code(self.status) if self.status is not None else None

    @property
    def signaled(self) -> bool:
        return self.status is not None and os.WIFSIGNALED(self.status)


def run_child(body: Callable[[], Any], deadline_s: float) -> ChildResult:
    """Fork a child that runs `body()` and writes its return value as ONE frame, then wait for it:
    up to `deadline_s`, killing on overrun. The fork tier, the clean room's grandchild and any
    one-shot child speak this, so there is one child wire format and one place that waits on it.

    `body` is the child's whole job and its own guard: whatever it raises is not caught here (a
    child that dies unreported is a reported fault in the parent). A reply the child cannot
    serialise or write exits with `EXIT_UNREPORTABLE`."""
    read_fd, write_fd = os.pipe()

    def child() -> int:
        os.close(read_fd)
        reply = body()
        try:
            write_frame(write_fd, reply)
        except BaseException:  # noqa: BLE001 — an unserialisable reply, a closed pipe
            try:
                os.close(write_fd)
            except BaseException:  # noqa: BLE001
                pass
            return EXIT_UNREPORTABLE
        os.close(write_fd)
        return 0

    pid = spawn(child)
    os.close(write_fd)
    deadline_at = time.monotonic() + deadline_s
    received = 0
    header, timed_out = read_exactly(read_fd, 4, deadline_at)
    payload = None
    if header is not None:
        received = 4
        (length,) = struct.unpack("<I", header)
        payload, timed_out = read_exactly(read_fd, length, deadline_at)
        if payload is not None:
            received += len(payload)
    os.close(read_fd)
    if timed_out:
        return ChildResult(None, True, reap(pid, kill=True), received, None)
    status = reap(pid)
    if payload is None:
        return ChildResult(None, False, status, received, None)
    try:
        return ChildResult(json.loads(payload.decode("utf-8")), False, status, received, None)
    except (ValueError, UnicodeDecodeError) as exc:
        return ChildResult(None, False, status, received, str(exc))
