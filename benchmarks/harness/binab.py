"""Interleaved A/B of two tiderace binaries on the same corpora.

    ROUNDS=6 python -m benchmarks.harness.binab main=/path/to/tiderace-main new=/path/to/tiderace-new pirn-agents

Same machine, same venv, same snapshot, same worker count — the only difference is which binary
runs. Interleaved and order-rotated, one warm-up round discarded, median reported, load recorded
with every sample. Build each arm with `cargo build --release -p engine-cli` on its branch and copy
the binary aside; the arms must not share a target directory that either build can overwrite.
"""
import os
import statistics
import sys

from .corpora import by_name, load_average
from .reports import report_path, write_json
from .runs import rounds, tiderace_cmd, tiderace_env_for, timed

ROUNDS = int(os.environ.get("ROUNDS", 4))  # the first is a discarded warm-up


def run(corpus, binary):
    t = timed(tiderace_cmd(corpus.tiderace_root, binary=binary), corpus.cwd, tiderace_env_for(corpus))
    tail = [l for l in t.output.splitlines() if " total" in l]
    return t.seconds, load_average(), (tail[-1] if tail else "?")


def main(argv):
    arms = dict(a.split("=", 1) for a in argv if "=" in a)
    corpora = [a for a in argv if "=" not in a]
    if len(arms) != 2 or not corpora:
        sys.exit(__doc__)
    out = {}
    names = list(arms)
    for pkg in corpora:
        corpus = by_name(pkg)
        rows = {a: [] for a in arms}
        for r, order in rounds(names, ROUNDS):  # rotate which arm goes first
            for arm in order:
                wall, ld, summary = run(corpus, arms[arm])
                if r:
                    rows[arm].append({"secs": round(wall, 2), "load": ld})
                print(f"{pkg:12s} r{r} {arm:6s} {wall:6.1f}s  load {ld:5.1f}  {summary}"
                      f"{'  (warm-up, discarded)' if r == 0 else ''}", flush=True)
        med = {a: statistics.median(x["secs"] for x in rows[a]) for a in arms}
        out[pkg] = {"samples": rows, "median": med}
        a, b = names
        print(f"{pkg}: {a} {med[a]:.1f}s   {b} {med[b]:.1f}s   {b} is {med[a] / med[b]:.2f}x {a}", flush=True)
    write_json(report_path("binab"), out, indent=2)


if __name__ == "__main__":
    main(sys.argv[1:])
