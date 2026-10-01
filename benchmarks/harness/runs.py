"""Running a tool once, timed; the interleaving every timed pass uses; the two command lines,
spelled once; and the memory of a whole process tree (TID-112).

Each timed pass used to carry its own `perf_counter` + `subprocess.run` wrapper, its own
`for r in range(ROUNDS + 1): order = tools[r % n:] + tools[:r % n]` loop, and its own spelling of
`python -m pytest -q -p no:cacheprovider …` — seventeen of those. One copy here keeps the passes
comparable: they time the same way and invoke the same thing.
"""
from __future__ import annotations

import subprocess
import time
from dataclasses import dataclass
from typing import Iterator, Sequence, TypeVar

from .corpora import TIDERACE, Corpus, clean_env, tiderace_env

T = TypeVar("T")


@dataclass(frozen=True)
class Timing:
    """One run: wall clock, exit status and what it printed."""

    seconds: float
    returncode: int  # -1 when the run outlasted `hung_after`
    output: str  # stdout then stderr; the last 400 bytes when hung


def timed(cmd: Sequence[str], cwd: str, env: dict, timeout: int = 3600,
          hung_after: int | None = None) -> Timing:
    """Run `cmd` to completion and time it. With `hung_after`, a run that outlasts it is recorded
    as hung (`returncode -1`) and the pass goes on: a tier that cannot end a blocked test is itself
    a finding, not a reason to lose the other numbers."""
    t0 = time.perf_counter()
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True,
                           timeout=hung_after or timeout)
    except subprocess.TimeoutExpired as exc:
        if hung_after is None:
            raise
        tail = (exc.stdout or b"") + (exc.stderr or b"")
        tail = tail.decode("utf-8", "replace") if isinstance(tail, bytes) else tail
        return Timing(time.perf_counter() - t0, -1,
                      f"hung: no exit after {hung_after}s\n" + tail[-400:])
    return Timing(time.perf_counter() - t0, p.returncode, p.stdout + p.stderr)


def timed_with_peak(cmd: Sequence[str], cwd: str, env: dict) -> tuple[float, int, int]:
    """Wall clock, exit status, and the peak resident size (MB) of the whole process tree,
    sampled every 200 ms."""
    t0 = time.perf_counter()
    p = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    peak = 0
    while p.poll() is None:
        peak = max(peak, tree_memory_kb(p.pid))
        time.sleep(0.2)
    return time.perf_counter() - t0, p.returncode, peak // 1024


def rounds(tools: Sequence[T], n: int) -> Iterator[tuple[int, list[T]]]:
    """`n` rounds of `tools`, each round rotating which goes first — so a busy stretch of the
    machine is spread across every tool rather than handed to whichever ran during it. The caller
    treats round 0 as the discarded warm-up."""
    for r in range(n):
        k = r % len(tools)
        yield r, list(tools[k:]) + list(tools[:k])


def pytest_cmd(python: str, target: str, *args: str, quiet: bool = True) -> list[str]:
    """`python -m pytest [-q] -p no:cacheprovider <args> <target>` — pytest as the benchmark runs
    it: no cache plugin, so a second run is as cold as the first."""
    return [python, "-m", "pytest", *(["-q"] if quiet else []), "-p", "no:cacheprovider", *args, target]


def tiderace_cmd(root: str, *args: str, binary: str = TIDERACE) -> list[str]:
    """`tiderace run -q <args> <root>`."""
    return [binary, "run", "-q", *args, root]


def pytest_env(corpus: Corpus, xdist: bool = False) -> dict:
    """pytest's environment for `corpus`; with `xdist`, the path that supplies pytest-xdist where
    the corpus's venv has none."""
    base = clean_env()
    return dict(base, PYTHONPATH=corpus.xdist_path) if xdist and corpus.xdist_path else base


def tiderace_env_for(corpus: Corpus, **extra: str) -> dict:
    """tiderace's environment for `corpus`: the shim, the package, and the corpus's interpreter."""
    return dict(tiderace_env(), TIDERACE_PYTHON=corpus.python, **extra)


def tree_memory_kb(root_pid: int) -> int:
    """Memory of `root_pid` and every descendant, summed, in kB — what a fork-based runner and an
    xdist session actually occupy, which a single process's `ru_maxrss` cannot say.

    Proportional set size where the kernel reports it (`/proc/<pid>/smaps_rollup`, Linux): a
    page shared by a forked worker and its parent is charged once, divided among them. Summed
    RSS charges it to every process that maps it — eight workers forked from one image showed
    nine gigabytes of RSS for two of PSS — so RSS is the fallback only."""
    try:
        out = subprocess.run(["ps", "-eo", "pid=,ppid=,rss="], capture_output=True, text=True).stdout
    except OSError:
        return 0
    children: dict = {}
    rss: dict = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) != 3:
            continue
        pid, ppid, kb = int(parts[0]), int(parts[1]), int(parts[2])
        children.setdefault(ppid, []).append(pid)
        rss[pid] = kb
    total, stack = 0, [root_pid]
    while stack:
        pid = stack.pop()
        total += _pss_kb(pid, rss.get(pid, 0))
        stack.extend(children.get(pid, []))
    return total


def _pss_kb(pid: int, rss_fallback: int) -> int:
    try:
        with open(f"/proc/{pid}/smaps_rollup") as fh:
            for line in fh:
                if line.startswith("Pss:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return rss_fallback
