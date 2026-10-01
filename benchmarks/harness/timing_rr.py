"""Timed pass, round-robin interleaved: pytest, pytest -n auto, tiderace.

    ROUNDS=4 OUT=timing.json python -m benchmarks.harness.timing_rr pirn-core click

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
import os
import statistics
import sys

from .corpora import CORPORA, load_average
from .reports import HERE, read_json, write_json
from .runs import pytest_cmd, pytest_env, rounds, tiderace_cmd, tiderace_env_for, timed_with_peak

ROUNDS = int(os.environ.get("ROUNDS", "3"))


def main(argv):
    only = set(argv)
    out_path = os.path.join(HERE, os.environ.get("OUT", "timing.json"))
    results = read_json(out_path, {})
    for corpus in CORPORA:
        if only and corpus.name not in only:
            continue
        tools = [
            ("pytest", pytest_cmd(corpus.python, corpus.pytest_target), pytest_env(corpus)),
            ("pytest -n auto", pytest_cmd(corpus.python, corpus.pytest_target, "-n", "auto"),
             pytest_env(corpus, xdist=True)),
            ("tiderace", tiderace_cmd(corpus.tiderace_root), tiderace_env_for(corpus)),
        ]
        runs = {t[0]: [] for t in tools}
        print(f"== {corpus.name}", flush=True)
        for r, order in rounds(tools, ROUNDS + 1):  # rotate which tool goes first
            for tool, cmd, env in order:
                secs, rc, peak_mb = timed_with_peak(cmd, corpus.cwd, env)
                ld = load_average()
                if r:
                    runs[tool].append({"secs": round(secs, 3), "load": ld, "rc": rc, "peak_rss_mb": peak_mb})
                print(f"   r{r} {tool:16s} {secs:8.2f}s  peak {peak_mb:5d} MB  (load {ld:.1f}, rc {rc})"
                      f"{'  warm-up' if r == 0 else ''}", flush=True)
        med = {t: statistics.median(x["secs"] for x in v) for t, v in runs.items() if v}
        peak = {t: max(x["peak_rss_mb"] for x in v) for t, v in runs.items() if v}
        print("   medians: " + "  ".join(f"{t} {m:.1f}s" for t, m in med.items()), flush=True)
        print("   peak memory of the process tree (PSS where available): "
              + "  ".join(f"{t} {m} MB" for t, m in peak.items()), flush=True)
        results[corpus.name] = {"group": corpus.group, "runs": runs, "median": med, "peak_rss_mb": peak}
        write_json(out_path, results, indent=1)
    print(f"wrote {out_path}")


if __name__ == "__main__":
    main(sys.argv[1:])
