"""Interleaved A/B of the shim at several git refs, one binary.

    ROUNDS=4 python -m benchmarks.harness.shimab base=origin/main cand=HEAD anyio pirn-core fx_corpus

Each arm's `engine/py-shim` and `engine/py-tiderace` are exported from the ref with `git archive`
into a scratch directory and handed to the same `tiderace` binary through `TIDERACE_SHIM` and
`PYTHONPATH`; the only difference between arms is the Python the workers run. Interleaved and
order-rotated, one warm-up round discarded, median reported, load recorded with every sample.

This is the gate a shim change runs before it merges: against its merge base, on a suite that
exercises what changed. A change that reads clean on the Rust suites and parity can still double
a run — TID-124 step 4 did, on anyio, and nothing timed it (TID-125). Writes `shimab.json`.
"""
import os
import statistics
import subprocess
import sys
import tempfile

from .corpora import R, by_name, load_average
from .reports import report_path, write_json
from .runs import rounds, tiderace_cmd, tiderace_env_for, timed

ROUNDS = int(os.environ.get("ROUNDS", 4))  # the first is a discarded warm-up
THRESHOLD = float(os.environ.get("THRESHOLD", 10))  # percent slower than the first arm that counts as a finding


def export(ref: str, into: str) -> str:
    """`engine/py-shim` and `engine/py-tiderace` at `ref`, exported under `into/<ref>`."""
    dest = os.path.join(into, ref.replace("/", "_"))
    os.makedirs(dest, exist_ok=True)
    archive = subprocess.run(["git", "archive", ref, "engine/py-shim", "engine/py-tiderace"],
                             cwd=R, capture_output=True, check=True).stdout
    subprocess.run(["tar", "-x", "-C", dest], input=archive, check=True)
    return dest


def run(corpus, tree: str):
    env = tiderace_env_for(corpus, TIDERACE_SHIM=os.path.join(tree, "engine", "py-shim", "shim.py"),
                           PYTHONPATH=os.path.join(tree, "engine", "py-tiderace"))
    t = timed(tiderace_cmd(corpus.tiderace_root), corpus.cwd, env)
    tail = [l for l in t.output.splitlines() if " total" in l]
    return t.seconds, load_average(), t.returncode, (tail[-1] if tail else "?")


def main(argv):
    arms = dict(a.split("=", 1) for a in argv if "=" in a)
    corpora = [a for a in argv if "=" not in a]
    if len(arms) < 2 or not corpora:
        sys.exit(__doc__)
    names = list(arms)
    out = {}
    findings = []
    with tempfile.TemporaryDirectory(prefix="tiderace-shimab-") as scratch:
        trees = {name: export(ref, scratch) for name, ref in arms.items()}
        for pkg in corpora:
            corpus = by_name(pkg)
            rows = {a: [] for a in arms}
            for r, order in rounds(names, ROUNDS):
                for arm in order:
                    wall, ld, rc, summary = run(corpus, trees[arm])
                    if r:
                        rows[arm].append({"secs": round(wall, 2), "load": ld, "rc": rc})
                    print(f"{pkg:12s} r{r} {arm:8s} {wall:7.2f}s  load {ld:5.1f}  rc {rc}  {summary[:60]}"
                          f"{'  (warm-up, discarded)' if r == 0 else ''}", flush=True)
            med = {a: statistics.median(x["secs"] for x in rows[a]) for a in arms}
            out[pkg] = {"arms": arms, "samples": rows, "median": med}
            base = names[0]
            line = f"{pkg}: " + "   ".join(f"{a} {med[a]:.2f}s" for a in names)
            for a in names[1:]:
                pct = (med[a] - med[base]) / med[base] * 100
                line += f"   {a} vs {base} {pct:+.0f}%"
                if pct > THRESHOLD:
                    findings.append(f"{pkg}: {a} is {pct:.0f}% slower than {base} ({med[a]:.2f}s vs {med[base]:.2f}s)")
            print(line, flush=True)
    write_json(report_path("shimab"), out, indent=2)
    if findings:
        print("\nSLOWER THAN THE BASE ARM (threshold {:.0f}%):".format(THRESHOLD))
        for f in findings:
            print("  " + f)
        sys.exit(1)
    print(f"\nno arm is more than {THRESHOLD:.0f}% slower than {names[0]}")


if __name__ == "__main__":
    main(sys.argv[1:])
