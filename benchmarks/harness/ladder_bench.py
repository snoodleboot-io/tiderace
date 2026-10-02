"""The isolation ladder's cost per test, per tier, on synthetic suites — sync and async.

    python -m benchmarks.harness.ladder_bench --python .tiderace-fx-venv/bin/python

Two suites from `scale_corpus` (1,000 trivial tests each; one all sync, one all `async def` with
async fixtures), each run cold, no daemon, on every tier the platform has: the ladder (the
default — restorable tests in-process with snapshot/restore), `--no-optimistic` (a fork per
test), `--strategy subprocess` (in-process with restore, no fork anywhere), and the ladder on one
worker (the per-test cost without the parallelism). Trivial tests, so the wall clock is the
runner's own cost; divided by the test count it is microseconds per test — the number TID-123's
acceptance asked for and the number a change to the in-process path has to keep (TID-125 doubled
it for async tests and nothing noticed). Median of `--rounds`. Writes `ladder.json`. `--shim-tree`
points at another ref's export (`git archive <ref> engine/py-shim engine/py-tiderace | tar -x`) for
the before of a before/after.
"""
import argparse
import os
import shutil
import statistics
import subprocess
import sys
import tempfile

from .corpora import R, load_average, tiderace_env
from .reports import report_path, write_json
from .runs import tiderace_cmd, timed

TIERS = {
    "ladder": [],
    "fork": ["--no-optimistic"],
    "subprocess": ["--strategy", "subprocess"],
    "ladder, 1 worker": ["--workers", "1"],
}


def generate(share: float, out: str, tests: int) -> str:
    subprocess.run([sys.executable, "-m", "benchmarks.harness.scale_corpus", "--packages", "4", "--modules", "5",
                    "--tests", str(tests // 20), "--async-share", str(share), "--out", out],
                   cwd=R, check=True, capture_output=True)
    return os.path.join(out, "tests")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--python", required=True)
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--tests", type=int, default=1000)
    ap.add_argument("--tiers", nargs="*", default=list(TIERS))
    ap.add_argument("--shim-tree", help="a checkout or `git archive` of another ref: its engine/py-shim and "
                    "engine/py-tiderace are used instead of this tree's (the before of a before/after)")
    a = ap.parse_args()
    env = dict(tiderace_env(), TIDERACE_PYTHON=os.path.abspath(a.python), TIDERACE_NO_DAEMON="1")
    if a.shim_tree:
        tree = os.path.abspath(a.shim_tree)
        env.update(TIDERACE_SHIM=os.path.join(tree, "engine", "py-shim", "shim.py"),
                   PYTHONPATH=os.path.join(tree, "engine", "py-tiderace"))
    out = {}
    with tempfile.TemporaryDirectory(prefix="tiderace-ladder-") as scratch:
        suites = {"sync": generate(0.0, os.path.join(scratch, "sync"), a.tests),
                  "async": generate(1.0, os.path.join(scratch, "async"), a.tests)}
        for kind, root in suites.items():
            out[kind] = {}
            for tier in a.tiers:
                secs = []
                for r in range(a.rounds + 1):  # the first is a discarded warm-up
                    t = timed(tiderace_cmd(root, *TIERS[tier]), scratch, env)
                    tail = [l for l in t.output.splitlines() if " total" in l]
                    if r:
                        secs.append(t.seconds)
                    print(f"{kind:5s} {tier:16s} r{r} {t.seconds:6.2f}s  load {load_average():4.1f}  rc {t.returncode}  "
                          f"{(tail[-1] if tail else '?')[:50]}{'  (warm-up)' if r == 0 else ''}", flush=True)
                med = statistics.median(secs)
                out[kind][tier] = {"secs": secs, "median": round(med, 3), "us_per_test": round(med / a.tests * 1e6)}
            print(f"{kind}: " + "   ".join(f"{tier} {v['median']:.2f}s ({v['us_per_test']} us/test)" for tier, v in out[kind].items()), flush=True)
        shutil.rmtree(scratch, ignore_errors=True)
    write_json(report_path("ladder"), out, indent=2)


if __name__ == "__main__":
    main()
