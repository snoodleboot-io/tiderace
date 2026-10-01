"""Timed pass, round-robin interleaved: pytest, pytest -n auto, tiderace.

    ROUNDS=4 OUT=timing.json python benchmarks/harness/timing_rr.py pirn-core click

This machine is shared with other work, and its load never settles for long enough to time seven
corpora back to back. Running all of one tool's repeats and then the next tool's would hand
whichever tool happened to run during a busy stretch a worse number. Interleaving instead — one
repeat of each tool per round, rotating which goes first — spreads any drift across all three, and
the median of the rounds is what gets reported. Every measurement records the one-minute load
average it ran under, so the spread is visible rather than assumed. One warm-up round is discarded
(import caches, page cache, tiderace's own state file).

Gate it: a wall-clock comparison at load 20 on 8 cores measures the contention, not the runners.
`quiet_gate.sh` waits for the machine before starting a pass.
"""
import json, os, statistics, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
from corpora import CORPORA, TIDERACE, clean_env, load, tiderace_env

HERE = os.path.dirname(os.path.abspath(__file__))
ROUNDS = int(os.environ.get("ROUNDS", "3"))
only = set(sys.argv[1:])
out_path = os.path.join(HERE, os.environ.get("OUT", "timing.json"))
results = json.load(open(out_path)) if os.path.exists(out_path) else {}


def tree_rss_kb(root_pid: int) -> int:
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
        total += pss_kb(pid, rss.get(pid, 0))
        stack.extend(children.get(pid, []))
    return total


def pss_kb(pid: int, rss_fallback: int) -> int:
    try:
        with open(f"/proc/{pid}/smaps_rollup") as fh:
            for line in fh:
                if line.startswith("Pss:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return rss_fallback


def run(cmd, cwd, env):
    """Wall clock and the peak resident size of the whole process tree, sampled every 200 ms."""
    t0 = time.perf_counter()
    p = subprocess.Popen(cmd, cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    peak = 0
    while p.poll() is None:
        peak = max(peak, tree_rss_kb(p.pid))
        time.sleep(0.2)
    return time.perf_counter() - t0, p.returncode, peak // 1024


for name, group, cwd, py, target, troot, extra_path in CORPORA:
    if only and name not in only:
        continue
    base = clean_env()
    xenv = dict(base, PYTHONPATH=extra_path) if extra_path else base
    tools = [
        ("pytest", [py, "-m", "pytest", "-q", "-p", "no:cacheprovider", target], base),
        ("pytest -n auto", [py, "-m", "pytest", "-q", "-p", "no:cacheprovider", "-n", "auto", target], xenv),
        ("tiderace", [TIDERACE, "run", "-q", troot], dict(tiderace_env(), TIDERACE_PYTHON=py)),
    ]
    runs = {t[0]: [] for t in tools}
    print(f"== {name}", flush=True)
    for r in range(ROUNDS + 1):
        order = tools[r % len(tools):] + tools[:r % len(tools)]  # rotate which tool goes first
        for tool, cmd, env in order:
            secs, rc, peak_mb = run(cmd, cwd, env)
            ld = load()
            if r:
                runs[tool].append({"secs": round(secs, 3), "load": ld, "rc": rc, "peak_rss_mb": peak_mb})
            print(f"   r{r} {tool:16s} {secs:8.2f}s  peak {peak_mb:5d} MB  (load {ld:.1f}, rc {rc})"
                  f"{'  warm-up' if r == 0 else ''}", flush=True)
    med = {t: statistics.median(x["secs"] for x in v) for t, v in runs.items() if v}
    peak = {t: max(x["peak_rss_mb"] for x in v) for t, v in runs.items() if v}
    print(f"   medians: " + "  ".join(f"{t} {m:.1f}s" for t, m in med.items()), flush=True)
    print(f"   peak memory of the process tree (PSS where available): " + "  ".join(f"{t} {m} MB" for t, m in peak.items()), flush=True)
    results[name] = {"group": group, "runs": runs, "median": med, "peak_rss_mb": peak}
    json.dump(results, open(out_path, "w"), indent=1)
print(f"wrote {out_path}")
