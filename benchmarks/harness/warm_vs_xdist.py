"""Warm-ordered tiderace vs pytest-xdist on one corpus (TID-52's second acceptance criterion).

    PIRN_SNAPSHOT=... ROUNDS=4 python -m benchmarks.harness.warm_vs_xdist pirn-agents

The first tiderace run records every test's duration (TID-62); every run after it orders the work
units by those durations. So the comparison is: xdist as it always runs, against tiderace on its
second run and later — the run a developer actually gets. One priming run, then interleaved rounds
rotating which goes first, one warm-up round discarded, medians, load recorded. The header of each
tiderace run is kept, so the record shows `learned=N durations` was in effect.
"""
import os
import statistics
import sys

from .corpora import by_name, load_average
from .reports import report_path, write_json
from .runs import pytest_cmd, pytest_env, rounds, tiderace_cmd, tiderace_env_for, timed

ROUNDS = int(os.environ.get("ROUNDS", 4))


def run(cmd, cwd, env):
    t = timed(cmd, cwd, env)
    header = next((l for l in t.output.splitlines() if l.startswith("tiderace: strategy")), "")
    tail = next((l for l in reversed(t.output.strip().splitlines()) if " total" in l or "passed" in l), "")
    return round(t.seconds, 2), load_average(), header, tail


def main(name):
    corpus = by_name(name)
    xenv = pytest_env(corpus, xdist=True)
    tenv = tiderace_env_for(corpus)
    report = report_path("report", f"{name}-warm")
    state = os.path.join(corpus.tiderace_root, ".tiderace-state.json")
    if os.path.exists(state):
        os.remove(state)
    print(f"== {name}: priming run (records durations)", flush=True)
    secs, ld, header, tail = run(tiderace_cmd(corpus.tiderace_root), corpus.cwd, tenv)
    print(f"   prime  {secs:6.1f}s  load {ld:.1f}  {header.replace('tiderace: ', '')}", flush=True)

    arms = {
        "pytest -n auto": pytest_cmd(corpus.python, corpus.pytest_target, "-n", "auto"),
        "tiderace (warm)": tiderace_cmd(corpus.tiderace_root, "--report", report),
    }
    rows = {a: [] for a in arms}
    for r, order in rounds(list(arms), ROUNDS):
        for arm in order:
            secs, ld, header, tail = run(arms[arm], corpus.cwd, xenv if arm.startswith("pytest") else tenv)
            if r:
                rows[arm].append({"secs": secs, "load": ld, "header": header})
            learned = header.split("learned=")[-1] if "learned=" in header else ""
            print(f"   r{r} {arm:16s} {secs:6.1f}s  load {ld:5.1f}  {learned:26s} {tail[:60]}"
                  f"{'  (warm-up, discarded)' if r == 0 else ''}", flush=True)
    med = {a: statistics.median(x["secs"] for x in rows[a]) for a in arms}
    print(f"{name}: xdist {med['pytest -n auto']:.1f}s   tiderace warm {med['tiderace (warm)']:.1f}s   "
          f"tiderace is {med['pytest -n auto'] / med['tiderace (warm)']:.2f}x xdist", flush=True)
    write_json(report_path("warm_vs_xdist", name), {"corpus": name, "samples": rows, "median": med}, indent=2)
    print("=== WARM VS XDIST DONE ===")


if __name__ == "__main__":
    main(sys.argv[1])
