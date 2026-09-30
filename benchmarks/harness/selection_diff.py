"""Selection parity for one corpus: what `-k` / `-m` select under tiderace vs under pytest.

    python benchmarks/harness/selection_diff.py click -k context "utils and not echo" tests
    python benchmarks/harness/selection_diff.py pirn-core -m "not slow" "needs_postgres or needs_kafka"
    python benchmarks/harness/selection_diff.py anyio            # the built-in expressions

A runner that agrees with pytest on outcomes but not on *what an expression selects* is still a
different runner, and a tally cannot tell the two apart: `-k unit` selecting 0 tests and 4,557
tests both report "0 failed". So each expression is compared as a set of node ids — tiderace's
`--report` against pytest's `--collect-only` in the corpus's own venv — and the ids only one side
has are printed. This is how TID-100 (directory names on the node's chain) was found and closed.

Module-level skips (a `pytest.importorskip` that skips a whole file) are reported by tiderace as
one skipped node per test and never collected by pytest, so they are left out of tiderace's side
by their `skip_origin`; the module count is printed instead.
"""
import json, os, subprocess, sys
sys.path.insert(0, os.path.dirname(__file__))
from corpora import TIDERACE, by_name, clean_env, tiderace_env

HERE = os.path.dirname(os.path.abspath(__file__))

# Expressions that exercise every kind of name `-k` matches, and `-m` on the marks the corpora
# declare. A corpus not listed gets the generic set.
BUILTIN = {
    "click": ("-k", ["context", "utils and not echo", "not shell", "tests"]),
    "anyio": ("-k", ["socket", "asyncio and not trio", "streams", "tls and asyncio"]),
    "pirn-core": ("-k", ["unit", "end_to_end", "connectors", "not unit"]),
    "pirn-agents": ("-m", ["anyio", "not anyio"]),
}
GENERIC = ("-k", ["test", "not test_"])


def pytest_selects(py, cwd, target, flag, expr):
    p = subprocess.run([py, "-m", "pytest", "-p", "no:cacheprovider", "-p", "no:randomly",
                        "--collect-only", "-q", flag, expr, target],
                       cwd=cwd, capture_output=True, text=True, env=clean_env())
    return {l.strip() for l in p.stdout.splitlines() if "::" in l}


def tiderace_selects(py, cwd, troot, prefix, flag, expr):
    report = os.path.join(HERE, "report-selection.json")
    if os.path.exists(report):
        os.remove(report)
    subprocess.run([TIDERACE, "run", "-q", "--report", report, flag, expr, troot], cwd=cwd,
                   capture_output=True, text=True,
                   env=dict(tiderace_env(), TIDERACE_PYTHON=py, TIDERACE_NO_DAEMON="1"))
    tests = json.load(open(report))["tests"] if os.path.exists(report) else []
    norm = (lambda n: f"{prefix}/{n}") if prefix != "." else (lambda n: n)
    ids = {norm(t["node_id"]) for t in tests if not t.get("skip_origin")}
    modules = {t["skip_origin"] for t in tests if t.get("skip_origin")}
    return ids, len(modules)


def main(argv):
    if not argv:
        print(__doc__)
        return 2
    name = argv[0]
    _, _, cwd, py, target, troot, _ = by_name(name)
    prefix = os.path.relpath(troot, cwd)
    if len(argv) >= 3 and argv[1] in ("-k", "-m"):
        flag, exprs = argv[1], argv[2:]
    else:
        flag, exprs = BUILTIN.get(name, GENERIC)
    worst = 0
    for expr in exprs:
        want = pytest_selects(py, cwd, target, flag, expr)
        got, skipped_modules = tiderace_selects(py, cwd, troot, prefix, flag, expr)
        only_pytest, only_ours = sorted(want - got), sorted(got - want)
        note = f"  (+{skipped_modules} modules skipped at import)" if skipped_modules else ""
        print(f"{name} {flag} {expr!r}: pytest {len(want)}  tiderace {len(got)}{note}  "
              f"only-pytest {len(only_pytest)}  only-tiderace {len(only_ours)}", flush=True)
        for n in only_pytest[:5]:
            print(f"  - {n}")
        for n in only_ours[:5]:
            print(f"  + {n}")
        worst = max(worst, len(only_pytest) + len(only_ours))
    return 1 if worst else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
