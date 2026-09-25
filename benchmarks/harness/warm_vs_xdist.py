"""Warm-ordered tiderace vs pytest-xdist on one corpus (TID-52's second acceptance criterion).

    PIRN_SNAPSHOT=... ROUNDS=4 python benchmarks/harness/warm_vs_xdist.py pirn-agents

The first tiderace run records every test's duration (TID-62); every run after it orders the work
units by those durations. So the comparison is: xdist as it always runs, against tiderace on its
second run and later — the run a developer actually gets. One priming run, then interleaved rounds
rotating which goes first, one warm-up round discarded, medians, load recorded. The header of each
tiderace run is kept, so the record shows `learned=N durations` was in effect.
"""
import json, os, statistics, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
from corpora import TIDERACE, by_name, clean_env, load, tiderace_env

HERE = os.path.dirname(os.path.abspath(__file__))
ROUNDS = int(os.environ.get("ROUNDS", 4))
name = sys.argv[1]
_, _, cwd, py, target, troot, extra = by_name(name)
xenv = dict(clean_env(), PYTHONPATH=extra) if extra else clean_env()
tenv = dict(tiderace_env(), TIDERACE_PYTHON=py)


def run(cmd, env):
    t0 = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=3600)
    out = p.stdout + p.stderr
    header = next((l for l in out.splitlines() if l.startswith("tiderace: strategy")), "")
    tail = next((l for l in reversed(out.strip().splitlines()) if " total" in l or "passed" in l), "")
    return round(time.perf_counter() - t0, 2), load(), header, tail


report = os.path.join(HERE, f"report-{name}-warm.json")
state = os.path.join(troot, ".tiderace-state.json")
if os.path.exists(state):
    os.remove(state)
print(f"== {name}: priming run (records durations)", flush=True)
secs, ld, header, tail = run([TIDERACE, "run", "-q", troot], tenv)
print(f"   prime  {secs:6.1f}s  load {ld:.1f}  {header.replace('tiderace: ', '')}", flush=True)

arms = {
    "pytest -n auto": [py, "-m", "pytest", "-q", "-p", "no:cacheprovider", "-n", "auto", target],
    "tiderace (warm)": [TIDERACE, "run", "-q", "--report", report, troot],
}
rows = {a: [] for a in arms}
names = list(arms)
for r in range(ROUNDS):
    for arm in (names if r % 2 == 0 else names[::-1]):
        secs, ld, header, tail = run(arms[arm], xenv if arm.startswith("pytest") else tenv)
        if r:
            rows[arm].append({"secs": secs, "load": ld, "header": header})
        learned = header.split("learned=")[-1] if "learned=" in header else ""
        print(f"   r{r} {arm:16s} {secs:6.1f}s  load {ld:5.1f}  {learned:26s} {tail[:60]}"
              f"{'  (warm-up, discarded)' if r == 0 else ''}", flush=True)
med = {a: statistics.median(x["secs"] for x in rows[a]) for a in arms}
print(f"{name}: xdist {med['pytest -n auto']:.1f}s   tiderace warm {med['tiderace (warm)']:.1f}s   "
      f"tiderace is {med['pytest -n auto'] / med['tiderace (warm)']:.2f}x xdist", flush=True)
json.dump({"corpus": name, "samples": rows, "median": med},
          open(os.path.join(HERE, f"warm_vs_xdist-{name}.json"), "w"), indent=2)
print("=== WARM VS XDIST DONE ===")
