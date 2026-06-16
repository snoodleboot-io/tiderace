#!/usr/bin/env python3
"""Wellspring shim — Phase-1 spike (no pytest underneath).

Imports the corpus ONCE into this process (the Wellspring), then forks a pristine
copy-on-write child per test and runs exactly one test in it. Free per-test isolation;
import cost paid once.

Protocol with the Rust orchestrator over stdin(0)/stdout(1): length-prefixed (u32 LE) JSON
frames.
  startup:   shim -> {"ready": true, "pid": int}
  request:   orchestrator -> {"node_id": str, "style": "pytest_func"|"unittest_method",
                              "deadline_ms": int}
  response:  shim -> {"node_id": str, "outcome": "passed|failed|skipped|error", "detail": str}

This shim is intentionally minimal and is expected to be productionized in Phase 2 (that is a
spike, not a stub). It uses length-prefixed JSON here; the bincode-vs-msgpack wire decision is a
Phase-2 follow-up recorded in the spike results.
"""
from __future__ import annotations

import importlib
import json
import os
import select
import signal
import struct
import sys
import traceback
import unittest

_STDIN = 0
_STDOUT = 1


def _read_exactly(fd: int, n: int) -> bytes | None:
    buf = b""
    while len(buf) < n:
        chunk = os.read(fd, n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def _read_frame(fd: int) -> dict | None:
    header = _read_exactly(fd, 4)
    if header is None:
        return None
    (length,) = struct.unpack("<I", header)
    payload = _read_exactly(fd, length)
    if payload is None:
        return None
    return json.loads(payload.decode("utf-8"))


def _write_frame(fd: int, obj: dict) -> None:
    payload = json.dumps(obj).encode("utf-8")
    os.write(fd, struct.pack("<I", len(payload)) + payload)


def _module_name(node_id: str) -> tuple[str, str]:
    """Split 'pkg/mod.py::rest' into ('pkg.mod', 'rest')."""
    path, _, rest = node_id.partition("::")
    if path.endswith(".py"):
        path = path[:-3]
    return path.replace("/", "."), rest


def _run_one(node_id: str, style: str) -> tuple[str, str]:
    """Run a single test in THIS (forked child) process; return (outcome, detail)."""
    mod_name, rest = _module_name(node_id)
    module = importlib.import_module(mod_name)
    try:
        if style == "unittest_method":
            cls_name, _, method = rest.partition("::")
            case = getattr(module, cls_name)(method)
            result = unittest.TestResult()
            case.run(result)  # stdlib drives setUp/test/tearDown — NOT pytest
            if result.errors:
                return "error", result.errors[0][1]
            if result.failures:
                return "failed", result.failures[0][1]
            if result.skipped:
                return "skipped", result.skipped[0][1]
            return "passed", ""
        func = getattr(module, rest)
        func()
        return "passed", ""
    except AssertionError as exc:
        return "failed", "".join(traceback.format_exception_only(type(exc), exc))
    except Exception as exc:  # noqa: BLE001 — any test error maps to Outcome::Error
        return "error", "".join(traceback.format_exception_only(type(exc), exc))


def _exec_forked(req: dict) -> dict:
    node_id = req["node_id"]
    style = req["style"]
    deadline_s = req.get("deadline_ms", 5000) / 1000.0

    read_fd, write_fd = os.pipe()
    pid = os.fork()
    if pid == 0:  # ---- CHILD: pristine COW copy of the warm Wellspring ----
        os.close(read_fd)
        try:
            outcome, detail = _run_one(node_id, style)
            os.write(write_fd, json.dumps({"outcome": outcome, "detail": detail[:4000]}).encode())
        except BaseException:  # noqa: BLE001 — never let the child hang on the way out
            pass
        finally:
            os.close(write_fd)
            os._exit(0)

    # ---- PARENT: time-bounded read of the child's result ----
    os.close(write_fd)
    ready, _, _ = select.select([read_fd], [], [], deadline_s)
    if not ready:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.waitpid(pid, 0)
        os.close(read_fd)
        return {"node_id": node_id, "outcome": "error", "detail": "timeout"}

    data = b""
    while True:
        chunk = os.read(read_fd, 65536)
        if not chunk:
            break
        data += chunk
    os.close(read_fd)
    _, status = os.waitpid(pid, 0)

    if not data:  # child died before reporting (crash / hard exit)
        if os.WIFSIGNALED(status):
            return {"node_id": node_id, "outcome": "error",
                    "detail": f"child killed by signal {os.WTERMSIG(status)}"}
        if os.WIFEXITED(status) and os.WEXITSTATUS(status) != 0:
            return {"node_id": node_id, "outcome": "error",
                    "detail": f"child exited {os.WEXITSTATUS(status)}"}
        return {"node_id": node_id, "outcome": "error", "detail": "no result from child"}

    res = json.loads(data.decode())
    return {"node_id": node_id, "outcome": res["outcome"], "detail": res.get("detail", "")}


def _preimport(corpus_dir: str) -> None:
    """Warm the Wellspring: import numpy (C-extension) and every test module ONCE."""
    import numpy  # noqa: F401 — warmed pre-fork so children inherit it via COW
    for root, _dirs, files in os.walk(corpus_dir):
        for name in files:
            if name.startswith("test_") and name.endswith(".py"):
                rel = os.path.relpath(os.path.join(root, name), corpus_dir)[:-3]
                importlib.import_module(rel.replace(os.sep, "."))


def serve() -> int:
    corpus_dir = sys.argv[1]
    sys.path.insert(0, corpus_dir)
    _preimport(corpus_dir)
    _write_frame(_STDOUT, {"ready": True, "pid": os.getpid()})
    while True:
        req = _read_frame(_STDIN)
        if req is None:
            return 0
        _write_frame(_STDOUT, _exec_forked(req))


if __name__ == "__main__":
    sys.exit(serve())
