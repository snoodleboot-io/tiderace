"""Parity pass: pytest vs tiderace on every corpus, load-independent by design.

    python benchmarks/harness/parity.py            # every corpus
    python benchmarks/harness/parity.py click flask

Tallies come from `tiderace run --report` (TID-55), not from the terminal: scraping stdout is what
once made a 62-test gap read as one number — 36 changed outcomes plus 32 node ids that never
existed on our side plus 6 extra, two opposite errors partly cancelling. The report is per node, so
`nodediff.py` can compare sets of ids rather than trusting a tally. Results accumulate in
`parity.json` beside this file; the per-corpus reports stay as `report-<corpus>.json`.
"""
import json, os, re, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
from corpora import CORPORA, TIDERACE, clean_env, tiderace_env

HERE = os.path.dirname(os.path.abspath(__file__))


def counts_pytest(text):
    tail = [l for l in text.strip().splitlines() if re.search(r"\d+ (passed|failed|error)", l)]
    line = tail[-1] if tail else ""
    c = {k: 0 for k in ("passed", "failed", "error", "skipped", "deselected", "xfailed", "xpassed")}
    for n, word in re.findall(r"(\d+) (passed|failed|errors?|skipped|deselected|xfailed|xpassed)", line):
        c["error" if word.startswith("error") else word] += int(n)
    return c, line


def counts_tiderace(report_path, text):
    if not os.path.exists(report_path):
        return None, text.strip().splitlines()[-1] if text.strip() else ""
    r = json.load(open(report_path))
    c = {k: r[k] for k in ("passed", "failed", "skipped", "total")}
    c["error"] = r["errored"]
    c["skipped_modules"] = r.get("skipped_modules", 0)
    line = (f"{c['passed']} passed, {c['failed']} failed, {c['error']} error, "
            f"{c['skipped']} skipped ({c['skipped_modules']} module(s) at import), {c['total']} total")
    return c, line


only = set(sys.argv[1:])
out = {}
for name, group, cwd, py, target, troot, _ in CORPORA:
    if only and name not in only:
        continue
    t0 = time.time()
    pr = subprocess.run([py, "-m", "pytest", "-q", "-p", "no:cacheprovider", target], cwd=cwd,
                        capture_output=True, text=True, env=clean_env(), timeout=3600)
    pc, pline = counts_pytest(pr.stdout + pr.stderr)
    rpath = os.path.join(HERE, f"report-{name}.json")
    if os.path.exists(rpath):
        os.remove(rpath)  # a stale report from a previous pass must never be read as this one's
    tr = subprocess.run([TIDERACE, "run", "-q", "--report", rpath, troot], cwd=cwd,
                        capture_output=True, text=True, env=dict(tiderace_env(), TIDERACE_PYTHON=py),
                        timeout=3600)
    tc, tline = counts_tiderace(rpath, tr.stdout + tr.stderr)
    # Passed / failed / error must agree exactly. Skips are compared by `nodediff.py`: pytest counts
    # a module that skips at import once, tiderace once per test in it (TID-55), so the tallies
    # differ by construction on any suite with an `importorskip`.
    same = tc is not None and all(pc[k] == tc[k] for k in ("passed", "failed", "error"))
    out[name] = {"group": group, "pytest": pc, "pytest_line": pline, "tiderace": tc,
                 "tiderace_line": tline, "parity": same, "report": rpath}
    print(f"{name:12s} {'PARITY ' if same else 'DIVERGE'}  pytest[{pline}]\n{'':21s}tiderace[{tline}]"
          f"   ({time.time() - t0:.0f}s)", flush=True)
path = os.path.join(HERE, "parity.json")
prev = json.load(open(path)) if os.path.exists(path) else {}
prev.update(out)
json.dump(prev, open(path, "w"), indent=2)
