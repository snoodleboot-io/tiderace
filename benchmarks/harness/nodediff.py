"""Per-test outcome diff for one corpus: pytest vs tiderace, in that corpus's pinned venv.

    python benchmarks/harness/nodediff.py click

The only sound way to compare two runners is by node id: a tally cannot tell a changed outcome from
a node that never existed on one side. pytest's side comes from `-rA`; ours from `--report`.
"""
import json, os, re, subprocess, sys
from collections import Counter
sys.path.insert(0, os.path.dirname(__file__))
from corpora import TIDERACE, by_name, clean_env, tiderace_env

HERE = os.path.dirname(os.path.abspath(__file__))
name = sys.argv[1]
_, _, cwd, py, target, troot, _ = by_name(name)
pt = subprocess.run([py, "-m", "pytest", "-p", "no:cacheprovider", "-q", "-rA", target],
                    cwd=cwd, capture_output=True, text=True, env=clean_env()).stdout
pmap = {}
for word, node in re.findall(r"^(PASSED|FAILED|ERROR|XFAIL|XPASS|SKIPPED) (\S+)", pt, re.M):
    pmap[node] = word.lower()
rpath = os.path.join(HERE, f"report-{name}.json")
if os.path.exists(rpath):
    os.remove(rpath)
subprocess.run([TIDERACE, "run", "-q", "--report", rpath, troot], cwd=cwd, capture_output=True,
               text=True, env=dict(tiderace_env(), TIDERACE_PYTHON=py))
report = json.load(open(rpath))
tmap = {t["node_id"]: t["outcome"] for t in report["tests"]}
detail = {t["node_id"]: t.get("detail", "").splitlines() for t in report["tests"]}

# tiderace node ids are relative to the run root; pytest's are relative to cwd.
prefix = os.path.relpath(troot, cwd)
norm = lambda n: f"{prefix}/{n}" if prefix != "." else n
ours = {norm(n): o for n, o in tmap.items()}
print(f"{name}: pytest {Counter(pmap.values())}  tiderace {Counter(ours.values())}")
only_ours = sorted(set(ours) - set(pmap))
only_theirs = sorted(set(pmap) - set(ours))
print(f"node ids only tiderace has: {len(only_ours)}   only pytest has: {len(only_theirs)}")
for n in only_ours[:10]:
    print(f"  +{n}")
for n in only_theirs[:10]:
    print(f"  -{n}")
gap = [(n, o) for n, o in ours.items() if o in ("failed", "error") and pmap.get(n) == "passed"]
print(f"tiderace-only failures: {len(gap)}")
causes = Counter()
for n, _ in gap:
    raw = [l.strip() for l in detail[n[len(prefix) + 1:] if prefix != "." else n]]
    last = next((l for l in reversed(raw) if l and not l.startswith("File ")), "?")
    causes[re.sub(r"0x[0-9a-f]+|'[^']*'", "…", last)[:120]] += 1
for c, k in causes.most_common(12):
    print(f"  {k:3d}  {c}")
