"""Parity pass: pytest vs tiderace on every corpus, load-independent by design.

    python -m benchmarks.harness.parity            # every corpus
    python -m benchmarks.harness.parity click flask

Tallies come from `tiderace run --report` (TID-55), not from the terminal: scraping stdout is what
once made a 62-test gap read as one number — 36 changed outcomes plus 32 node ids that never
existed on our side plus 6 extra, two opposite errors partly cancelling. The report is per node, so
`nodediff` can compare sets of ids rather than trusting a tally. Results accumulate in
`parity.json` beside this file; the per-corpus reports stay as `report-<corpus>.json`.
"""
import re
import sys
import time

from .corpora import CORPORA
from .reports import fresh_report, read_json, report_path, write_json
from .runs import pytest_cmd, pytest_env, tiderace_cmd, tiderace_env_for, timed


def counts_pytest(text):
    tail = [l for l in text.strip().splitlines() if re.search(r"\d+ (passed|failed|error)", l)]
    line = tail[-1] if tail else ""
    c = {k: 0 for k in ("passed", "failed", "error", "skipped", "deselected", "xfailed", "xpassed")}
    for n, word in re.findall(r"(\d+) (passed|failed|errors?|skipped|deselected|xfailed|xpassed)", line):
        c["error" if word.startswith("error") else word] += int(n)
    return c, line


def counts_tiderace(report_path, text):
    r = read_json(report_path)
    if r is None:
        return None, text.strip().splitlines()[-1] if text.strip() else ""
    c = {k: r[k] for k in ("passed", "failed", "skipped", "total")}
    c["error"] = r["errored"]
    c["skipped_modules"] = r.get("skipped_modules", 0)
    line = (f"{c['passed']} passed, {c['failed']} failed, {c['error']} error, "
            f"{c['skipped']} skipped ({c['skipped_modules']} module(s) at import), {c['total']} total")
    return c, line


def main(argv):
    only = set(argv)
    out = {}
    for corpus in CORPORA:
        if only and corpus.name not in only:
            continue
        t0 = time.time()
        pr = timed(pytest_cmd(corpus.python, corpus.pytest_target), corpus.cwd, pytest_env(corpus))
        pc, pline = counts_pytest(pr.output)
        rpath = fresh_report("report", corpus.name)
        tr = timed(tiderace_cmd(corpus.tiderace_root, "--report", rpath), corpus.cwd,
                   tiderace_env_for(corpus))
        tc, tline = counts_tiderace(rpath, tr.output)
        # Passed / failed / error must agree exactly. Skips are compared by `nodediff`: pytest
        # counts a module that skips at import once, tiderace once per test in it (TID-55), so the
        # tallies differ by construction on any suite with an `importorskip`.
        same = tc is not None and all(pc[k] == tc[k] for k in ("passed", "failed", "error"))
        out[corpus.name] = {"group": corpus.group, "pytest": pc, "pytest_line": pline, "tiderace": tc,
                            "tiderace_line": tline, "parity": same, "report": rpath}
        print(f"{corpus.name:12s} {'PARITY ' if same else 'DIVERGE'}  pytest[{pline}]\n"
              f"{'':21s}tiderace[{tline}]   ({time.time() - t0:.0f}s)", flush=True)
    path = report_path("parity")
    prev = read_json(path, {})
    prev.update(out)
    write_json(path, prev, indent=2)


if __name__ == "__main__":
    main(sys.argv[1:])
