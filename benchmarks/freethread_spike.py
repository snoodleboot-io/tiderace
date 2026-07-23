"""TID-3 spike — free-threading (PEP 703 / CPython 3.14t) as a no-fork parallel tier.

The premise: a free-threaded interpreter runs pure tests on **threads** — no fork, one import, parallel
across cores. The question is whether that is (a) actually parallel and (b) sound, and how it relates to
the sub-interpreter tier (TID-2) which solves the same "parallel no-fork" problem a different way.

Run under BOTH interpreters and compare:

    # GIL-enabled 3.14 (the ordinary build)
    .tiderace-fx-venv/bin/python benchmarks/freethread_spike.py

    # free-threaded 3.14t
    $(uv python find cpython-3.14.4+freethreaded)  benchmarks/freethread_spike.py

Deterministic except for wall-clock timings (labelled). No third-party deps for the core experiments;
the numpy experiment self-skips if numpy is absent.
"""

from __future__ import annotations

import concurrent.futures
import sys
import sysconfig
import threading
import time


def _timer() -> "callable":
    # perf_counter is monotonic; fine for wall-clock deltas even though Date.now-style calls are banned
    # in the workflow sandbox — this is a plain Python script, not a workflow.
    return time.perf_counter


def gil_status() -> tuple[bool, bool]:
    """(-> built_free_threaded, gil_enabled_at_runtime)."""
    built_ft = sysconfig.get_config_var("Py_GIL_DISABLED") == 1
    # sys._is_gil_enabled exists on 3.13+; on a free-threaded build it can still be re-enabled if an
    # extension without the flag is imported. This spike imports none in experiments 1–3.
    runtime_gil = getattr(sys, "_is_gil_enabled", lambda: True)()
    return built_ft, runtime_gil


# --- CPU-bound "test body": pure integer work, no I/O, no shared mutation. Sized to ~40ms each. ---
def cpu_body(seed: int) -> int:
    total = 0
    for i in range(1, 400_000):
        total = (total + seed * i) % 1_000_003
    return total


def run_sequential(n: int) -> tuple[float, list[int]]:
    clock = _timer()
    t0 = clock()
    out = [cpu_body(i) for i in range(n)]
    return clock() - t0, out


def run_threaded(n: int, workers: int) -> tuple[float, list[int]]:
    clock = _timer()
    t0 = clock()
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as ex:
        out = list(ex.map(cpu_body, range(n)))
    return clock() - t0, out


def experiment_parallelism() -> float:
    """Does threading actually use multiple cores? Returns the speedup."""
    import os

    workers = min(8, (os.cpu_count() or 2))
    n = workers * 3  # a few tasks per worker

    # warm up the JIT/allocator so the first timing isn't penalised
    run_sequential(2)

    seq_t, seq_out = run_sequential(n)
    par_t, par_out = run_threaded(n, workers)

    print(f"  workers={workers}  tasks={n}")
    print(f"  sequential: {seq_t*1000:7.1f} ms")
    print(f"  threaded:   {par_t*1000:7.1f} ms")
    speedup = seq_t / par_t if par_t else 0.0
    print(f"  speedup:    {speedup:5.2f}x")
    # correctness: threading must not change results
    assert seq_out == par_out, "threaded results diverged from sequential — corruption"
    print("  results identical to sequential: OK")
    return speedup


# --- Module-level shared state, the thing the no-fork ladder snapshots/restores. ---
_SHARED = {"counter": 0}


def experiment_pure_reads_are_safe(workers: int = 8) -> bool:
    """Genuinely-pure tests (read shared state, never mutate) are correct under concurrency."""
    base = {"a": 1, "b": 2, "c": 3}
    _SHARED_RO = dict(base)

    def pure_test(_: int) -> int:
        # reads only; returns a function of the shared immutable snapshot
        return _SHARED_RO["a"] + _SHARED_RO["b"] + _SHARED_RO["c"]

    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as ex:
        results = list(ex.map(pure_test, range(workers * 50)))
    ok = all(r == 6 for r in results)
    print(f"  {len(results)} concurrent pure reads all correct: {'OK' if ok else 'FAIL'}")
    return ok


def experiment_restore_races(workers: int = 8) -> bool:
    """The race the issue warns about: snapshot/restore is NOT thread-safe on a shared module dict.

    The no-fork ladder isolates an *impure* test by snapshotting module globals, running the test, and
    restoring. On threads the module dict is shared across all workers, so two threads' mutate→restore
    windows interleave and clobber each other. This demonstrates *why* free-threading can only run
    genuinely-pure tests: the restore rung of the ladder cannot be used.

    Returns True if a race was observed (the expected, cautionary result).
    """
    _SHARED["counter"] = 0
    observed_bad = threading.Event()

    def impure_test_with_restore(_: int) -> None:
        snapshot = _SHARED["counter"]          # snapshot
        _SHARED["counter"] = snapshot + 1       # mutate (the "test")
        # a tiny window where another thread can observe/overwrite our mutation
        if _SHARED["counter"] != snapshot + 1:
            observed_bad.set()
        _SHARED["counter"] = snapshot           # restore

    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as ex:
        list(ex.map(impure_test_with_restore, range(workers * 500)))

    # After perfectly-isolated runs the counter must be back to 0. A race leaves it corrupted, or a
    # thread saw its own mutation overwritten mid-flight.
    final = _SHARED["counter"]
    raced = observed_bad.is_set() or final != 0
    print(f"  restore-on-shared-dict raced: {raced}  (final counter={final}, expected 0)")
    return raced


def experiment_numpy(workers: int = 4) -> str:
    """The differentiator vs sub-interpreters: free-threading CAN load numpy (sub-interps can't)."""
    try:
        import numpy as np
    except ImportError:
        print("  numpy not installed — skipping (install into this interpreter to measure)")
        return "skipped"

    def numpy_test(seed: int) -> float:
        rng = np.arange(seed + 1, seed + 10_001, dtype=np.float64)
        return float((rng * rng).sum())

    seq = [numpy_test(i) for i in range(workers * 4)]
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as ex:
        par = list(ex.map(numpy_test, range(workers * 4)))
    ok = seq == par
    print(f"  numpy {np.__version__} imported and ran across {workers} threads: {'OK' if ok else 'FAIL'}")
    return "ok" if ok else "fail"


def main() -> int:
    built_ft, runtime_gil = gil_status()
    print("=" * 72)
    print(f"interpreter: {sys.executable}")
    print(f"version: {sys.version.split()[0]}   built free-threaded: {built_ft}   "
          f"GIL enabled at runtime: {runtime_gil}")
    print("=" * 72)

    print("\n[1] PARALLELISM — do pure CPU-bound tests scale across cores?")
    speedup = experiment_parallelism()

    print("\n[2] SOUNDNESS — pure (read-only) tests under concurrency")
    pure_ok = experiment_pure_reads_are_safe()

    print("\n[3] SOUNDNESS — the restore race (why impure-on-threads is unsafe)")
    raced = experiment_restore_races()

    print("\n[4] NUMPY — the sub-interpreter differentiator")
    numpy_result = experiment_numpy()

    print("\n" + "=" * 72)
    print("VERDICT INPUTS")
    print(f"  parallel speedup:            {speedup:.2f}x  "
          f"({'real parallelism' if speedup > 1.5 else 'GIL-bound / no gain'})")
    print(f"  pure tests correct:          {pure_ok}")
    print(f"  restore races on threads:    {raced}  "
          f"(=> only genuinely-pure tests are thread-safe; the restore rung is unavailable)")
    print(f"  numpy across threads:        {numpy_result}  "
          f"(sub-interpreters CANNOT load numpy; free-threading can)")
    print("=" * 72)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
