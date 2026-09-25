"""The second run: impact analysis and the verdict store, measured (TID-65).

    PIRN_SNAPSHOT=... python benchmarks/harness/second_run.py pirn-core pirn-agents

Every number in the cold benchmark is a full run. This measures what a developer waits on most —
the run after an edit — through `tiderace-daemon`, the front end that does impact analysis:

  cold        `run --all` on a clean tree: the baseline, and it records every test's footprint
  no change   `run` with nothing edited: what is skipped, and how long the skip itself takes
  leaf edit   one line appended to the source module the FEWEST tests depend on
  hub edit    the same, on the module the MOST tests depend on — where impact analysis should
              degrade to nearly a full run, and say so
  failure     the leaf module made to raise at import: the second run MUST report the failures
              (TID-40 was exactly the stale-pass bug this guards against)

Leaf and hub are chosen from the state file the cold run wrote — dependents per source file — so
the choice is the corpus's own, not a guess. Each edit is reverted and the tree re-synced before the
next scenario. pytest is the fixed-cost comparison: it has no warm mode, so its number is the same
full run every time, and that is the point.

Load is recorded with every sample; `quiet_gate.sh` this like any other timed pass.
"""
import json, os, statistics, subprocess, sys, time
from collections import Counter
sys.path.insert(0, os.path.dirname(__file__))
from corpora import R, by_name, clean_env, load, tiderace_env

HERE = os.path.dirname(os.path.abspath(__file__))
DAEMON = os.environ.get("TIDERACE_DAEMON", os.path.join(R, "engine", "target", "release", "tiderace-daemon"))
ROUNDS = int(os.environ.get("ROUNDS", 3))
STATE = ".tiderace-state.json"


def sh(cmd, cwd, env, timeout=3600):
    t0 = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout)
    return time.perf_counter() - t0, p.returncode, p.stdout + p.stderr


def daemon(args, cwd, py):
    secs, rc, out = sh([DAEMON, *args], cwd, dict(tiderace_env(), TIDERACE_PYTHON=py))
    summary = next((l for l in out.splitlines() if " cached, " in l or " total" in l), "")
    return {"secs": round(secs, 3), "rc": rc, "load": load(), "summary": summary.strip()}


def pytest_full(cwd, py, target):
    secs, rc, out = sh([py, "-m", "pytest", "-q", "-p", "no:cacheprovider", target], cwd, clean_env())
    tail = [l for l in out.strip().splitlines() if "passed" in l or "failed" in l]
    return {"secs": round(secs, 3), "rc": rc, "load": load(), "summary": (tail[-1] if tail else "").strip()}


def dependents(state_path):
    """source file -> number of tests whose recorded footprint includes it."""
    state = json.load(open(state_path))
    count = Counter()
    for rec in state.get("tests", {}).values():
        for dep in rec.get("deps", []):
            count[dep] += 1
    return count


def choose_targets(troot, count):
    """The leaf (fewest dependents, at least one) and the hub (most) among the package's own
    source modules — not its tests and not a conftest, which is test-side setup.

    Recorded deps are relative to the run root (`troot`), which for a monorepo package is the
    package directory and for fx_corpus is its `tests/` dir; resolve them there, not from `cwd`."""
    own = {f: n for f, n in count.items()
           if f.endswith(".py") and "/tests/" not in f and not f.startswith("tests/")
           and not os.path.basename(f).startswith("test_") and os.path.basename(f) != "conftest.py"
           and os.path.exists(os.path.join(troot, f))}
    if not own:
        return None, None
    ordered = sorted(own.items(), key=lambda kv: (kv[1], kv[0]))
    return ordered[0], ordered[-1]


def with_edit(troot, rel, text, fn):
    """Append `text` to `rel` (relative to the run root), run `fn`, restore the file byte-for-byte."""
    path = os.path.join(troot, rel)
    original = open(path, "rb").read()
    try:
        with open(path, "ab") as fh:
            fh.write(text.encode())
        return fn()
    finally:
        with open(path, "wb") as fh:
            fh.write(original)


def med(samples, key="secs"):
    return statistics.median(s[key] for s in samples) if samples else None


out = {}
for name in sys.argv[1:]:
    _, group, cwd, py, target, troot, _ = by_name(name)
    state_path = os.path.join(troot, STATE)
    print(f"== {name}", flush=True)
    rec = {"pytest_full": [], "cold_all": [], "no_change": [], "leaf_edit": [], "hub_edit": [],
           "failure_edit": []}

    # pytest, the fixed-cost comparison
    for _ in range(ROUNDS):
        s = pytest_full(cwd, py, target)
        rec["pytest_full"].append(s)
        print(f"   pytest (full)     {s['secs']:8.2f}s  load {s['load']:.1f}  {s['summary']}", flush=True)

    # cold: clean state, full run with coverage → writes every footprint
    for _ in range(ROUNDS):
        if os.path.exists(state_path):
            os.remove(state_path)
        s = daemon(["run", troot, "--all"], cwd, py)
        rec["cold_all"].append(s)
        print(f"   cold run --all    {s['secs']:8.2f}s  load {s['load']:.1f}  {s['summary']}", flush=True)

    # warm, nothing edited
    for _ in range(ROUNDS):
        s = daemon(["run", troot], cwd, py)
        rec["no_change"].append(s)
        print(f"   warm, no change   {s['secs']:8.2f}s  load {s['load']:.1f}  {s['summary']}", flush=True)

    count = dependents(state_path)
    leaf, hub = choose_targets(troot, count)
    rec["targets"] = {"leaf": leaf, "hub": hub, "tests_with_footprints": len(json.load(open(state_path)).get("tests", {}))}
    print(f"   leaf {leaf}   hub {hub}", flush=True)

    if leaf and hub:
        for label, (rel, n) in (("leaf_edit", leaf), ("hub_edit", hub)):
            for _ in range(ROUNDS):
                s = with_edit(troot, rel, "\n# benchmark edit\n", lambda: daemon(["run", troot], cwd, py))
                s["dependents"] = n
                rec[label].append(s)
                print(f"   {label:16s} {s['secs']:8.2f}s  load {s['load']:.1f}  {s['summary']}   ({n} dependents)", flush=True)
                daemon(["run", troot], cwd, py)  # after the revert: re-sync the state, unmeasured

        # the soundness check: the edit that must not be a stale pass
        rel, n = leaf
        s = with_edit(troot, rel, "\nraise RuntimeError('benchmark: injected import failure')\n",
                      lambda: daemon(["run", troot], cwd, py))
        s["dependents"] = n
        rec["failure_edit"].append(s)
        print(f"   failure_edit     {s['secs']:8.2f}s  load {s['load']:.1f}  {s['summary']}   "
              f"({'REPORTED' if s['rc'] != 0 else 'STALE PASS — soundness failure'})", flush=True)
        daemon(["run", troot], cwd, py)

    rec["median"] = {k: med(v) for k, v in rec.items() if isinstance(v, list) and v and "secs" in v[0]}
    out[name] = rec
    json.dump(out, open(os.path.join(HERE, "second_run.json"), "w"), indent=1)
    print("   medians: " + "  ".join(f"{k} {m:.2f}s" for k, m in rec["median"].items() if m is not None), flush=True)
print("=== SECOND RUN DONE ===")
