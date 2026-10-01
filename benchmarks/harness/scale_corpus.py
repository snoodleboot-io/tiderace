"""Generate a synthetic suite of any size, for the question the real corpora cannot answer: how
the runners scale with the number of tests.

    python benchmarks/harness/scale_corpus.py --packages 20 --modules 20 --tests 50 --out /tmp/scale20k

20 packages × 20 modules × 50 tests = 20,000 tests, every one trivial (a few hundred nanoseconds
of work), so what is measured is the runner — collection, dispatch, selection, reporting — not
the tests. Every module has one parametrized test (three cases) and one marked `slow`, so `-k`
and `-m` have something to decide, and a conftest at the root holds a session fixture half the
tests take, so the fixture graph is not empty. Deterministic: the same arguments give the same
bytes. A `pytest.ini` declares the mark, so `--strict-markers` passes.
"""
import argparse, os

MODULE = '''\
import pytest


@pytest.mark.parametrize("n", [1, 2, 3])
def test_{mod}_cases(n):
    assert n > 0


@pytest.mark.slow
def test_{mod}_marked():
    assert True


'''

TEST_PLAIN = "def test_{mod}_{i:03d}():\n    assert {i} + 1 == {j}\n\n\n"
TEST_FIXTURE = "def test_{mod}_{i:03d}(session_counter):\n    assert session_counter >= 0\n\n\n"

CONFTEST = '''\
import pytest


@pytest.fixture(scope="session")
def session_counter():
    return 0
'''


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--packages", type=int, default=20)
    ap.add_argument("--modules", type=int, default=20)
    ap.add_argument("--tests", type=int, default=50)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    root = os.path.abspath(a.out)
    tests = os.path.join(root, "tests")
    os.makedirs(tests, exist_ok=True)
    open(os.path.join(root, "pytest.ini"), "w").write("[pytest]\nmarkers =\n    slow: slow\n")
    open(os.path.join(tests, "conftest.py"), "w").write(CONFTEST)
    open(os.path.join(tests, "__init__.py"), "w").write("")
    total = 0
    for p in range(a.packages):
        pkg = os.path.join(tests, f"pkg_{p:02d}")
        os.makedirs(pkg, exist_ok=True)
        open(os.path.join(pkg, "__init__.py"), "w").write("")
        for m in range(a.modules):
            mod = f"p{p:02d}m{m:02d}"
            body = [MODULE.format(mod=mod)]
            for i in range(a.tests - 4):  # the parametrized three and the marked one count too
                tmpl = TEST_FIXTURE if i % 2 else TEST_PLAIN
                body.append(tmpl.format(mod=mod, i=i, j=i + 1))
            open(os.path.join(pkg, f"test_{mod}.py"), "w").write("".join(body))
            total += a.tests
    print(f"{total} tests in {a.packages * a.modules} modules under {tests}")


if __name__ == "__main__":
    main()
