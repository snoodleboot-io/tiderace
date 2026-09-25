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


def run(cmd, cwd, env):
    t0 = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=7200)
    return time.perf_counter() - t0, p.returncode


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
            secs, rc = run(cmd, cwd, env)
            ld = load()
            if r:
                runs[tool].append({"secs": round(secs, 3), "load": ld, "rc": rc})
            print(f"   r{r} {tool:16s} {secs:8.2f}s  (load {ld:.1f}, rc {rc})"
                  f"{'  warm-up' if r == 0 else ''}", flush=True)
    med = {t: statistics.median(x["secs"] for x in v) for t, v in runs.items() if v}
    print(f"   medians: " + "  ".join(f"{t} {m:.1f}s" for t, m in med.items()), flush=True)
    results[name] = {"group": group, "runs": runs, "median": med}
    json.dump(results, open(out_path, "w"), indent=1)
print(f"wrote {out_path}")
