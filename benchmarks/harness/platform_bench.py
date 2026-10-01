"""Cold timings on the platform this runs on — Windows, macOS, Linux — for the corpora that need
no private snapshot: pytest, pytest -n auto, and tiderace in each execution tier the platform has.

    ROUNDS=3 python -m benchmarks.harness.platform_bench fx_corpus cachetools

(click 8.1.7's own suite does not collect under pytest on Python 3.14 — `filterwarnings = error`
meets a deprecation — so it is not a baseline there; it stays available by name.)

Round-robin interleaved like `timing_rr`, the median of the rounds reported, the first round a
discarded warm-up. Writes `platform-<system>.json` beside this file and, under GitHub Actions, a
table into the step summary. Portable: no `/proc`, no `ps`.

Windows has no fork, so tiderace's default tier there is the no-fork subprocess worker, and the
sub-interpreter tier (TID-13) is the parallel option; both are timed, and `--workers 1` gives the
sequential floor each is measured against. On macOS and Linux the default is the fork pool.
"""
import os
import platform
import statistics
import sys

from .corpora import by_name, load_average
from .reports import report_path, write_json
from .runs import pytest_cmd, pytest_env, rounds, tiderace_cmd, tiderace_env_for, timed

ROUNDS = int(os.environ.get("ROUNDS", "3"))
SYSTEM = platform.system().lower()

# A run that outlasts this is recorded as hung (`rc -1`) and the pass goes on: a tier that
# cannot end a blocked test is itself a finding, not a reason to lose the other numbers.
HUNG_AFTER = int(os.environ.get("HUNG_AFTER", "600"))

TOOLS = ["pytest", "pytest -n auto", "tiderace", "tiderace --workers 1", "tiderace --strategy subinterp"]


def main(argv):
    out_path = report_path("platform", SYSTEM)
    results = {"system": SYSTEM, "machine": platform.machine(), "python": platform.python_version(),
               "rounds": ROUNDS, "corpora": {}}
    for name in argv or ["fx_corpus"]:
        corpus = by_name(name)
        base = pytest_env(corpus)
        xenv = pytest_env(corpus, xdist=True)
        tenv = tiderace_env_for(corpus, TIDERACE_NO_DAEMON="1")
        root = corpus.tiderace_root
        tools = [
            ("pytest", pytest_cmd(corpus.python, corpus.pytest_target), base),
            ("pytest -n auto", pytest_cmd(corpus.python, corpus.pytest_target, "-n", "auto"), xenv),
            ("tiderace", tiderace_cmd(root), tenv),
            ("tiderace --workers 1", tiderace_cmd(root, "--workers", "1"), tenv),
        ]
        if SYSTEM == "windows":
            tools.append(("tiderace --strategy subinterp", tiderace_cmd(root, "--strategy", "subinterp"), tenv))
        runs = {t[0]: [] for t in tools}
        print(f"== {name} on {SYSTEM} {platform.machine()}, Python {platform.python_version()}", flush=True)
        for r, order in rounds(tools, ROUNDS + 1):
            for tool, cmd, env in order:
                t = timed(cmd, corpus.cwd, env, hung_after=HUNG_AFTER)
                if r:
                    runs[tool].append({"secs": round(t.seconds, 3), "load": load_average(), "rc": t.returncode})
                print(f"   r{r} {tool:30s} {t.seconds:8.2f}s  rc {t.returncode}{'  warm-up' if r == 0 else ''}",
                      flush=True)
                if t.returncode not in (0, 1):
                    print("      " + t.output[-400:].strip().replace("\n", "\n      ")[-300:], flush=True)
        med = {t: statistics.median(x["secs"] for x in v) for t, v in runs.items() if v}
        hung = sorted(t for t, v in runs.items() if any(x["rc"] == -1 for x in v))
        failed = sorted(t for t, v in runs.items() if any(x["rc"] not in (0, 1, -1) for x in v))
        print("   medians: " + "  ".join(f"{t} {m:.2f}s" for t, m in med.items())
              + (f"   hung: {', '.join(hung)}" if hung else "")
              + (f"   did not run: {', '.join(failed)}" if failed else ""), flush=True)
        results["corpora"][name] = {"group": corpus.group, "runs": runs, "median": med, "hung": hung,
                                    "did_not_run": failed}
        write_json(out_path, results, indent=1)
    print(f"wrote {out_path}")

    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        present = [t for t in TOOLS if any(t in c["median"] for c in results["corpora"].values())]
        with open(summary, "a") as fh:
            fh.write(f"### {SYSTEM} {platform.machine()}, Python {platform.python_version()} — median of {ROUNDS} rounds, seconds\n\n")
            fh.write("| corpus | " + " | ".join(present) + " |\n|--|" + "--:|" * len(present) + "\n")
            for name, c in results["corpora"].items():
                def cell(t):
                    if t in c["hung"]:
                        return "hung"
                    if t in c["did_not_run"]:
                        return "did not run"
                    return f"{c['median'][t]:.2f}" if t in c["median"] else "—"
                fh.write(f"| {name} | " + " | ".join(cell(t) for t in present) + " |\n")


if __name__ == "__main__":
    main(sys.argv[1:])
