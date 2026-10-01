"""Selection parity for one corpus: what `-k` / `-m` select under tiderace vs under pytest.

    python -m benchmarks.harness.selection_diff click -k context "utils and not echo" tests
    python -m benchmarks.harness.selection_diff pirn-core -m "not slow" "needs_postgres or needs_kafka"
    python -m benchmarks.harness.selection_diff anyio            # the built-in expressions

A runner that agrees with pytest on outcomes but not on *what an expression selects* is still a
different runner, and a tally cannot tell the two apart: `-k unit` selecting 0 tests and 4,557
tests both report "0 failed". So each expression is compared as a set of node ids — tiderace's
`--report` against pytest's `--collect-only` in the corpus's own venv — and the ids only one side
has are printed. This is how TID-100 (directory names on the node's chain) was found and closed.

Module-level skips (a `pytest.importorskip` that skips a whole file) are reported by tiderace as
one skipped node per test and never collected by pytest, so they are left out of tiderace's side
by their `skip_origin`; the module count is printed instead.
"""
import sys

from .corpora import by_name
from .reports import fresh_report, read_json
from .runs import pytest_cmd, pytest_env, tiderace_cmd, tiderace_env_for, timed

# Expressions that exercise every kind of name `-k` matches, and `-m` on the marks the corpora
# declare. A corpus not listed gets the generic set.
BUILTIN = {
    "click": ("-k", ["context", "utils and not echo", "not shell", "tests"]),
    "anyio": ("-k", ["socket", "asyncio and not trio", "streams", "tls and asyncio"]),
    "pirn-core": ("-k", ["unit", "end_to_end", "connectors", "not unit"]),
    "pirn-agents": ("-m", ["anyio", "not anyio"]),
}
GENERIC = ("-k", ["test", "not test_"])


def pytest_selects(corpus, flag, expr):
    out = timed(pytest_cmd(corpus.python, corpus.pytest_target, "-p", "no:randomly", "--collect-only",
                           "-q", flag, expr, quiet=False), corpus.cwd, pytest_env(corpus)).output
    return {l.strip() for l in out.splitlines() if "::" in l}


def tiderace_selects(corpus, flag, expr):
    report = fresh_report("report", "selection")
    timed(tiderace_cmd(corpus.tiderace_root, "--report", report, flag, expr), corpus.cwd,
          tiderace_env_for(corpus, TIDERACE_NO_DAEMON="1"))
    tests = read_json(report, {"tests": []})["tests"]
    prefix = corpus.prefix
    norm = (lambda n: f"{prefix}/{n}") if prefix != "." else (lambda n: n)
    ids = {norm(t["node_id"]) for t in tests if not t.get("skip_origin")}
    modules = {t["skip_origin"] for t in tests if t.get("skip_origin")}
    return ids, len(modules)


def main(argv):
    if not argv:
        print(__doc__)
        return 2
    name = argv[0]
    corpus = by_name(name)
    if len(argv) >= 3 and argv[1] in ("-k", "-m"):
        flag, exprs = argv[1], argv[2:]
    else:
        flag, exprs = BUILTIN.get(name, GENERIC)
    worst = 0
    for expr in exprs:
        want = pytest_selects(corpus, flag, expr)
        got, skipped_modules = tiderace_selects(corpus, flag, expr)
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
