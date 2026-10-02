"""Generate a synthetic suite of any size, for the question the real corpora cannot answer: how
the runners scale with the number of tests.

    python -m benchmarks.harness.scale_corpus --packages 20 --modules 20 --tests 50 --out /tmp/scale20k

20 packages × 20 modules × 50 tests = 20,000 tests, every one trivial (a few hundred nanoseconds
of work), so what is measured is the runner — collection, dispatch, selection, reporting — not
the tests. Every module has one parametrized test (three cases) and one marked `slow`, so `-k`
and `-m` have something to decide, and a conftest at the root holds a session fixture half the
tests take, so the fixture graph is not empty. Deterministic: the same arguments give the same
bytes. A `pytest.ini` declares the mark, so `--strict-markers` passes.

`--async-share 0.5` makes that share of each module's plain tests `async def` (awaiting one
`asyncio.sleep(0)`), half of them taking an `async def` fixture and handing one call to
`asyncio.to_thread`, so the runner's async path — the loop per test, the async fixture, its
isolation, and the idle worker pool the loop's executor leaves behind (what cost anyio 2.5× in
TID-125) — is exercised (TID-126). Bare `async def`
tests need a plugin under pytest, which the fx venv does not carry: a suite with a non-zero
share is for tiderace alone (`ladder_bench`), not for the pytest comparison.
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
TEST_ASYNC = "async def test_{mod}_{i:03d}():\n    await asyncio.sleep(0)\n    assert {i} + 1 == {j}\n\n\n"
TEST_ASYNC_FIXTURE = ("async def test_{mod}_{i:03d}(async_token):\n    await asyncio.to_thread(len, async_token)\n"
                      "    assert async_token == 'token'\n\n\n")
ASYNC_HEADER = '''\
import asyncio


@pytest.fixture
async def async_token():
    await asyncio.sleep(0)
    return "token"


'''

CONFTEST = '''\
import pytest


@pytest.fixture(scope="session")
def session_counter():
    return 0
'''


def _write(path: str, text: str) -> None:
    with open(path, "w") as fh:
        fh.write(text)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--packages", type=int, default=20)
    ap.add_argument("--modules", type=int, default=20)
    ap.add_argument("--tests", type=int, default=50)
    ap.add_argument("--out", required=True)
    ap.add_argument("--async-share", type=float, default=0.0, help="share of each module's plain tests made async")
    a = ap.parse_args()
    root = os.path.abspath(a.out)
    tests = os.path.join(root, "tests")
    os.makedirs(tests, exist_ok=True)
    _write(os.path.join(root, "pytest.ini"), "[pytest]\nmarkers =\n    slow: slow\n")
    _write(os.path.join(tests, "conftest.py"), CONFTEST)
    _write(os.path.join(tests, "__init__.py"), "")
    total = 0
    for p in range(a.packages):
        pkg = os.path.join(tests, f"pkg_{p:02d}")
        os.makedirs(pkg, exist_ok=True)
        _write(os.path.join(pkg, "__init__.py"), "")
        for m in range(a.modules):
            mod = f"p{p:02d}m{m:02d}"
            body = [MODULE.format(mod=mod)]
            plain = a.tests - 4  # the parametrized three and the marked one count too
            async_from = plain - int(round(plain * a.async_share))
            if a.async_share:
                body.append(ASYNC_HEADER)  # after `import pytest`
            for i in range(plain):
                if i >= async_from:
                    tmpl = TEST_ASYNC_FIXTURE if i % 2 else TEST_ASYNC
                else:
                    tmpl = TEST_FIXTURE if i % 2 else TEST_PLAIN
                body.append(tmpl.format(mod=mod, i=i, j=i + 1))
            _write(os.path.join(pkg, f"test_{mod}.py"), "".join(body))
            total += a.tests
    print(f"{total} tests in {a.packages * a.modules} modules under {tests}")


if __name__ == "__main__":
    main()
