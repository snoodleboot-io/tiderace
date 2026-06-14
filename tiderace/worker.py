"""Persistent test worker for the tiderace project.

Driven by a Rust parent over stdin/stdout using newline-delimited JSON.

The whole point of this process is to ``import pytest`` exactly once at
startup and then run many individual tests via repeated ``pytest.main()``
calls inside the same warm interpreter.  Avoiding the per-test interpreter +
import cost is the entire reason this worker exists.

Protocol (newline-delimited JSON, one object per line):

* At startup, after importing pytest, the worker writes EXACTLY one line to
  the real stdout::

      {"ready": true, "pid": <os.getpid()>}

  and flushes.

* The worker then loops, reading request lines from stdin.  Each request::

      {"nodeid": "tests/test_x.py::test_a",
       "invalidate": ["tests/test_x.py", "src/y.py"]}

  ``invalidate`` is a list of file paths whose modules must be dropped from
  ``sys.modules`` BEFORE running, so that pytest re-imports changed code.

* For each request the worker emits EXACTLY one response line to the real
  stdout::

      {"nodeid": "...",
       "status": "passed|failed|skipped|error",
       "duration_ms": <int>,
       "summary": <string or null>}

  and flushes.

* On EOF (stdin closed) the loop breaks and the process exits 0.

Keeping the protocol channel clean is critical: pytest and the tests it runs
print to stdout/stderr, and that output must never pollute the JSON protocol.
We therefore capture the real stdout once at startup, write all protocol JSON
to it, and during each ``pytest.main()`` call we redirect Python-level stdout
and stderr into a throwaway buffer while running pytest with ``--capture=no``
so pytest does not perform its own fd-level capture (which would bypass the
redirect).
"""

import contextlib
import importlib
import io
import json
import os
import sys
import time

# Don't accumulate/trust .pyc across a long-lived session — an editor that writes
# with an unchanged/backdated mtime could otherwise re-load stale bytecode.
sys.dont_write_bytecode = True

# Import pytest ONCE, at startup. This is the warm-process payload.
import pytest


# Capture a reference to the *real* stdout at startup. ALL protocol JSON is
# written here and flushed here. Everything pytest / the tests print is kept
# away from this stream.
PROTO = sys.__stdout__ if sys.__stdout__ is not None else sys.stdout


def _write_proto(obj):
    """Serialize ``obj`` as one JSON line to the protocol stream and flush."""
    PROTO.write(json.dumps(obj))
    PROTO.write("\n")
    PROTO.flush()


class ResultCollector:
    """In-process pytest plugin that records the outcome of a single test.

    Status is derived from report objects rather than by parsing pytest's
    textual output, which is far more robust.
    """

    def __init__(self):
        self.reset()

    def reset(self):
        # Default to "error": if we never see a usable report, something went
        # wrong (e.g. collection failure with no setup/call report).
        self.status = "error"
        self.summary = None
        self._saw_call = False

    @staticmethod
    def _extract_summary(report):
        """Return a short (~200 char) summary string for a failing report."""
        text = None
        longreprtext = getattr(report, "longreprtext", None)
        if longreprtext:
            text = longreprtext
        else:
            longrepr = getattr(report, "longrepr", None)
            if longrepr is not None:
                text = str(longrepr)
        if not text:
            return None
        text = text.strip()
        if not text:
            return None
        return text[:200]

    def pytest_runtest_logreport(self, report):
        when = getattr(report, "when", None)

        if when == "setup":
            if report.failed:
                # Fixture / setup error.
                self.status = "error"
                self.summary = self._extract_summary(report)
            elif report.skipped:
                self.status = "skipped"
                # A skip recorded at setup time carries its reason in longrepr.
                self.summary = self._extract_summary(report)
        elif when == "call":
            self._saw_call = True
            if report.passed:
                self.status = "passed"
                self.summary = None
            elif report.failed:
                self.status = "failed"
                self.summary = self._extract_summary(report)
            elif report.skipped:
                # Skips raised inside the test body surface at call time.
                self.status = "skipped"
                self.summary = self._extract_summary(report)


# Path segments that mark third-party/installed code we must keep warm.
_INSTALLED_MARKERS = ("site-packages", "dist-packages", ".venv", "venv")


def _is_first_party(mod_abs, project_root):
    """True if ``mod_abs`` is project source (under root, not installed deps)."""
    if not mod_abs.startswith(project_root + os.sep):
        return False
    rel = mod_abs[len(project_root) + 1:]
    parts = rel.split(os.sep)
    return not any(marker in parts for marker in _INSTALLED_MARKERS)


def _evict_modules(paths):
    """Evict changed first-party modules from ``sys.modules`` before a re-run.

    Correctness over cleverness (see the stage-B security review): when any file
    changed we drop **every first-party module** — the changed files plus all
    project-local modules that may transitively import them — while keeping
    pytest, the stdlib, and installed dependencies warm (they are the expensive
    imports and rarely change). Re-importing first-party code is cheap, and this
    guarantees a warm re-run never executes stale code (which would be a silent
    false pass). ``importlib.invalidate_caches()`` makes the import system re-stat
    the filesystem so newly-created files are found.
    """
    if not paths:
        return

    project_root = os.path.abspath(os.getcwd())
    targets = set()
    for path in paths:
        try:
            targets.add(os.path.abspath(path))
        except Exception:
            continue

    for name, module in list(sys.modules.items()):
        if module is None:
            continue
        mod_file = getattr(module, "__file__", None)
        if not mod_file:
            continue
        try:
            mod_abs = os.path.abspath(mod_file)
        except Exception:
            continue
        if mod_abs in targets or _is_first_party(mod_abs, project_root):
            sys.modules.pop(name, None)

    importlib.invalidate_caches()


def _run_single(nodeid, collector):
    """Run a single ``nodeid`` via pytest and return the recorded status.

    pytest's terminal output is redirected into a throwaway buffer so it never
    reaches the real stdout/stderr protocol channel.
    """
    collector.reset()

    # --capture=no disables pytest's fd-level capture, so the surrounding
    # redirect_stdout/redirect_stderr (Python-level) actually contains pytest's
    # output instead of being bypassed at the fd layer.
    args = [
        "--capture=no",
        "-p", "no:cacheprovider",
        "-p", "no:cacheprovider",
        nodeid,
    ]

    sink = io.StringIO()
    with contextlib.redirect_stdout(sink), contextlib.redirect_stderr(sink):
        pytest.main(args, plugins=[collector])

    return collector.status, collector.summary


def _handle_request(request, collector):
    """Process a single decoded request dict and return a response dict."""
    nodeid = request.get("nodeid")
    invalidate = request.get("invalidate") or []

    if not isinstance(nodeid, str) or not nodeid:
        return {
            "nodeid": nodeid,
            "status": "error",
            "duration_ms": 0,
            "summary": "missing or invalid 'nodeid'",
        }

    start = time.perf_counter()
    try:
        _evict_modules(invalidate)
        # nodeid is ONLY ever passed as a pytest argument -- never eval/exec'd.
        status, summary = _run_single(nodeid, collector)
    except Exception as exc:  # noqa: BLE001 -- must never crash the loop.
        elapsed = time.perf_counter() - start
        return {
            "nodeid": nodeid,
            "status": "error",
            "duration_ms": int(elapsed * 1000),
            "summary": str(exc),
        }

    elapsed = time.perf_counter() - start
    return {
        "nodeid": nodeid,
        "status": status,
        "duration_ms": int(elapsed * 1000),
        "summary": summary,
    }


def main():
    # Announce readiness exactly once, on the real stdout.
    _write_proto({"ready": True, "pid": os.getpid()})

    collector = ResultCollector()

    # Read request lines until EOF (stdin closed).
    for line in sys.stdin:
        line = line.strip()
        if not line:
            # Ignore blank keep-alive lines without emitting a response.
            continue

        try:
            request = json.loads(line)
        except Exception as exc:  # noqa: BLE001
            # Malformed JSON: emit an error response rather than crashing.
            _write_proto({
                "nodeid": None,
                "status": "error",
                "duration_ms": 0,
                "summary": "invalid JSON request: " + str(exc),
            })
            continue

        if not isinstance(request, dict):
            _write_proto({
                "nodeid": None,
                "status": "error",
                "duration_ms": 0,
                "summary": "request must be a JSON object",
            })
            continue

        try:
            response = _handle_request(request, collector)
        except Exception as exc:  # noqa: BLE001 -- belt-and-suspenders.
            response = {
                "nodeid": request.get("nodeid"),
                "status": "error",
                "duration_ms": 0,
                "summary": str(exc),
            }

        _write_proto(response)

    return 0


if __name__ == "__main__":
    sys.exit(main())
