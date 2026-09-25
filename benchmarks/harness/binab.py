"""Interleaved A/B of two tiderace binaries on the same corpora.

    ROUNDS=6 python benchmarks/harness/binab.py main=/path/to/tiderace-main new=/path/to/tiderace-new pirn-agents

Same machine, same venv, same snapshot, same worker count — the only difference is which binary
runs. Interleaved and order-rotated, one warm-up round discarded, median reported, load recorded
with every sample. Build each arm with `cargo build --release -p engine-cli` on its branch and copy
the binary aside; the arms must not share a target directory that either build can overwrite.
"""
import json, os, statistics, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
from corpora import by_name, load, tiderace_env

HERE = os.path.dirname(os.path.abspath(__file__))
ROUNDS = int(os.environ.get("ROUNDS", 4))  # the first is a discarded warm-up
arms = dict(a.split("=", 1) for a in sys.argv[1:] if "=" in a)
corpora = [a for a in sys.argv[1:] if "=" not in a]
if len(arms) != 2 or not corpora:
    sys.exit(__doc__)


def run(pkg, binary):
    _, _, cwd, py, _, troot, _ = by_name(pkg)
    t0 = time.time()
    p = subprocess.run([binary, "run", "-q", troot], cwd=cwd, capture_output=True, text=True,
                       env=dict(tiderace_env(), TIDERACE_PYTHON=py), timeout=3600)
    tail = [l for l in (p.stdout + p.stderr).splitlines() if " total" in l]
    return time.time() - t0, load(), (tail[-1] if tail else "?")


out = {}
names = list(arms)
for pkg in corpora:
    rows = {a: [] for a in arms}
    for r in range(ROUNDS):
        for arm in (names if r % 2 == 0 else names[::-1]):  # rotate which arm goes first
            wall, ld, summary = run(pkg, arms[arm])
            if r:
                rows[arm].append({"secs": round(wall, 2), "load": ld})
            print(f"{pkg:12s} r{r} {arm:6s} {wall:6.1f}s  load {ld:5.1f}  {summary}"
                  f"{'  (warm-up, discarded)' if r == 0 else ''}", flush=True)
    med = {a: statistics.median(x["secs"] for x in rows[a]) for a in arms}
    out[pkg] = {"samples": rows, "median": med}
    a, b = names
    print(f"{pkg}: {a} {med[a]:.1f}s   {b} {med[b]:.1f}s   {b} is {med[a] / med[b]:.2f}x {a}", flush=True)
json.dump(out, open(os.path.join(HERE, "binab.json"), "w"), indent=2)
