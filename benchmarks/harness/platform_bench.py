"""Cold timings on the platform this runs on — Windows, macOS, Linux — for the corpora that need
no private snapshot: pytest, pytest -n auto, and tiderace in each execution tier the platform has.

    ROUNDS=3 python benchmarks/harness/platform_bench.py fx_corpus cachetools

(click 8.1.7's own suite does not collect under pytest on Python 3.14 — `filterwarnings = error`
meets a deprecation — so it is not a baseline there; it stays available by name.)

Round-robin interleaved like `timing_rr.py`, the median of the rounds reported, the first round a
discarded warm-up. Writes `platform-<system>.json` beside this file and, under GitHub Actions, a
table into the step summary. Portable: no `/proc`, no `ps`.

Windows has no fork, so tiderace's default tier there is the no-fork subprocess worker, and the
sub-interpreter tier (TID-13) is the parallel option; both are timed, and `--workers 1` gives the
sequential floor each is measured against. On macOS and Linux the default is the fork pool.
"""
import json, os, platform, statistics, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
from corpora import TIDERACE, by_name, clean_env, load, tiderace_env

HERE = os.path.dirname(os.path.abspath(__file__))
ROUNDS = int(os.environ.get("ROUNDS", "3"))
SYSTEM = platform.system().lower()
out_path = os.path.join(HERE, f"platform-{SYSTEM}.json")


# A run that outlasts this is recorded as hung (`rc -1`) and the pass goes on: a tier that
# cannot end a blocked test is itself a finding, not a reason to lose the other numbers.
HUNG_AFTER = int(os.environ.get("HUNG_AFTER", "600"))


def run(cmd, cwd, env):
    t0 = time.perf_counter()
    try:
        p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=HUNG_AFTER)
    except subprocess.TimeoutExpired as exc:
        tail = ((exc.stdout or b"") + (exc.stderr or b""))
        tail = tail.decode("utf-8", "replace") if isinstance(tail, bytes) else tail
        return time.perf_counter() - t0, -1, f"hung: no exit after {HUNG_AFTER}s\n" + tail[-400:]
    return time.perf_counter() - t0, p.returncode, (p.stdout + p.stderr)[-400:]


results = {"system": SYSTEM, "machine": platform.machine(), "python": platform.python_version(),
           "rounds": ROUNDS, "corpora": {}}
for name in sys.argv[1:] or ["fx_corpus"]:
    _, group, cwd, py, target, troot, extra_path = by_name(name)
    base = clean_env()
    xenv = dict(base, PYTHONPATH=extra_path) if extra_path else base
    tenv = dict(tiderace_env(), TIDERACE_PYTHON=py, TIDERACE_NO_DAEMON="1")
    tools = [
        ("pytest", [py, "-m", "pytest", "-q", "-p", "no:cacheprovider", target], base),
        ("pytest -n auto", [py, "-m", "pytest", "-q", "-p", "no:cacheprovider", "-n", "auto", target], xenv),
        ("tiderace", [TIDERACE, "run", "-q", troot], tenv),
        ("tiderace --workers 1", [TIDERACE, "run", "-q", "--workers", "1", troot], tenv),
    ]
    if SYSTEM == "windows":
        tools.append(("tiderace --strategy subinterp",
                      [TIDERACE, "run", "-q", "--strategy", "subinterp", troot], tenv))
    runs = {t[0]: [] for t in tools}
    print(f"== {name} on {SYSTEM} {platform.machine()}, Python {platform.python_version()}", flush=True)
    for r in range(ROUNDS + 1):
        order = tools[r % len(tools):] + tools[:r % len(tools)]
        for tool, cmd, env in order:
            secs, rc, tail = run(cmd, cwd, env)
            if r:
                runs[tool].append({"secs": round(secs, 3), "load": load(), "rc": rc})
            print(f"   r{r} {tool:30s} {secs:8.2f}s  rc {rc}{'  warm-up' if r == 0 else ''}", flush=True)
            if rc not in (0, 1):
                print("      " + tail.strip().replace("\n", "\n      ")[-300:], flush=True)
    med = {t: statistics.median(x["secs"] for x in v) for t, v in runs.items() if v}
    hung = sorted(t for t, v in runs.items() if any(x["rc"] == -1 for x in v))
    failed = sorted(t for t, v in runs.items() if any(x["rc"] not in (0, 1, -1) for x in v))
    print("   medians: " + "  ".join(f"{t} {m:.2f}s" for t, m in med.items())
          + (f"   hung: {', '.join(hung)}" if hung else "")
          + (f"   did not run: {', '.join(failed)}" if failed else ""), flush=True)
    results["corpora"][name] = {"group": group, "runs": runs, "median": med, "hung": hung,
                                "did_not_run": failed}
    json.dump(results, open(out_path, "w"), indent=1)
print(f"wrote {out_path}")

summary = os.environ.get("GITHUB_STEP_SUMMARY")
if summary:
    tools = ["pytest", "pytest -n auto", "tiderace", "tiderace --workers 1", "tiderace --strategy subinterp"]
    present = [t for t in tools if any(t in c["median"] for c in results["corpora"].values())]
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
