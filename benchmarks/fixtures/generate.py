#!/usr/bin/env python3
"""Generate a deterministic reference Python project for tiderace tests & benchmarks.

The generated suite has a real import graph so impact analysis is meaningful:

    src/shared.py          <- imported by EVERY test module (change it => all tests affected)
    src/mod_{i}.py         <- imported by exactly one pytest module (change it => one module affected)
    tests/test_mod_{i}.py  <- pytest-style tests importing src/mod_{i}.py
    tests/test_unit_case.py<- a unittest.TestCase subclass NOT named Test* (exercises unittest support)

Output is fully deterministic (no randomness, no timestamps) so it can be committed and
regenerated identically. Each test does a small fixed computation; pass --work-ms to add a
fixed sleep per test when you want per-test latency to dominate (useful for parallelism benchmarks).

Usage:
    python generate.py [--modules N] [--tests-per-module M] [--work-ms MS] [--out DIR]
"""
import argparse
import shutil
from pathlib import Path

SHARED_SRC = '''\
"""Shared helpers imported by every test module."""


def scale(x: int, factor: int = 2) -> int:
    """Return x multiplied by factor."""
    return x * factor


def clamp(x: int, lo: int, hi: int) -> int:
    """Clamp x into the inclusive range [lo, hi]."""
    if x < lo:
        return lo
    if x > hi:
        return hi
    return x
'''


def module_src(i: int) -> str:
    """Source for src/mod_{i}.py — a small, self-contained unit importing shared."""
    return f'''\
"""Module {i} — arithmetic helpers built on the shared layer."""
from src.shared import scale, clamp


def compute_{i}(n: int) -> int:
    """A deterministic computation unique to module {i}."""
    return scale(n, {i + 2}) + {i}


def bounded_{i}(n: int) -> int:
    """compute_{i} clamped into a fixed window."""
    return clamp(compute_{i}(n), 0, 1000)
'''


def pytest_module(i: int, tests_per_module: int, work_ms: int) -> str:
    """A pytest-style test module importing src/mod_{i}.py."""
    work = (
        f"    time.sleep({work_ms} / 1000.0)\n" if work_ms > 0 else ""
    )
    header = (
        "import time\n" if work_ms > 0 else ""
    ) + f"from src.mod_{i} import compute_{i}, bounded_{i}\n\n\n"
    body = []
    for t in range(tests_per_module):
        body.append(
            f"def test_compute_{i}_{t}():\n"
            f"{work}"
            f"    assert compute_{i}({t}) == ({t} * {i + 2}) + {i}\n"
        )
        body.append(
            f"def test_bounded_{i}_{t}():\n"
            f"{work}"
            f"    assert 0 <= bounded_{i}({t}) <= 1000\n"
        )
    return header + "\n\n".join(body) + "\n"


def unittest_module(work_ms: int) -> str:
    """A unittest.TestCase whose class name does NOT start with 'Test' (W4 coverage)."""
    work = f"        time.sleep({work_ms} / 1000.0)\n" if work_ms > 0 else ""
    head = ("import time\n" if work_ms > 0 else "") + (
        "import unittest\n"
        "from src.shared import scale, clamp\n\n\n"
        "class ArithmeticCase(unittest.TestCase):\n"
    )
    methods = []
    for t in range(4):
        methods.append(
            f"    def test_scale_{t}(self):\n"
            f"{work}"
            f"        self.assertEqual(scale({t}), {t} * 2)\n"
        )
    methods.append(
        "    def test_clamp_bounds(self):\n"
        f"{work}"
        "        self.assertEqual(clamp(50, 0, 10), 10)\n"
        "        self.assertEqual(clamp(-5, 0, 10), 0)\n"
    )
    tail = "\n\nif __name__ == '__main__':\n    unittest.main()\n"
    return head + "\n".join(methods) + tail


def eager_module_src(i: int, kb: int) -> str:
    """One module in the eager package: a little code and a fixed block of module-level data.

    The data is what makes the shape real. A few hundred empty modules cost almost nothing to fork,
    because `fork()`'s price is copying page tables and there are barely any pages. A large-import
    project is expensive because its modules *hold* things — registries, tables, compiled patterns.
    Measured on `pirn-agents`: 1,654 modules for 73 MB, so roughly 45 KB each (TID-18).
    """
    rows = max(1, (kb * 1024) // 64)
    return f'''\
"""Eager module {i} — stands in for a registry entry that the package __init__ pulls in."""

# A fixed table, sized so the package as a whole reaches a realistic resident footprint. Built at
# import time on purpose: that is what puts it in the parent's pages before the first fork.
TABLE_{i} = {{f"key_{{n}}_{i}": "x" * 48 for n in range({rows})}}


def lookup_{i}(n: int) -> str:
    """Read one row, so the table is not dead weight an optimiser could elide."""
    return TABLE_{i}[f"key_{{n % {rows}}}_{i}"]
'''


def eager_init_src(count: int) -> str:
    """The package `__init__` that imports every submodule eagerly.

    This is the pattern the benchmark exists to capture: a plugin registry or a package that
    self-registers its parts, so `import pkg` costs the whole tree rather than one module. It is a
    legitimate design, not a pathology — and it is the worst case for fork-per-test.
    """
    imports = "\n".join(f"from . import eager_{i}" for i in range(count))
    names = ", ".join(f'"eager_{i}"' for i in range(count))
    return f'''\
"""Eagerly import every submodule, the way a self-registering package does."""
{imports}

__all__ = [{names}]
'''


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--modules", type=int, default=6)
    ap.add_argument("--tests-per-module", type=int, default=8)
    ap.add_argument("--work-ms", type=int, default=0,
                    help="fixed sleep per test in ms (0 = pure CPU, the default)")
    ap.add_argument("--eager-modules", type=int, default=0,
                    help="modules in an eagerly-imported package, loaded by conftest so every "
                         "wellspring pays for them before forking (TID-18's large-import shape)")
    ap.add_argument("--eager-kb", type=int, default=45,
                    help="approximate KB of module-level data per eager module "
                         "(45 matches the measured pirn-agents ratio)")
    ap.add_argument("--out", type=Path,
                    default=Path(__file__).parent / "sample_project")
    args = ap.parse_args()

    out: Path = args.out
    if out.exists():
        shutil.rmtree(out)
    src = out / "src"
    tests = out / "tests"
    src.mkdir(parents=True)
    tests.mkdir(parents=True)

    (out / "__init__.py").write_text("")
    (src / "__init__.py").write_text("")
    (tests / "__init__.py").write_text("")
    (src / "shared.py").write_text(SHARED_SRC)
    conftest = "import sys, os\nsys.path.insert(0, os.path.dirname(__file__))\n"
    if args.eager_modules:
        # Imported from conftest so the cost lands in the *parent* — the wellspring pays it once at
        # startup and every fork afterwards copies the resulting page tables. Importing it from the
        # test modules instead would measure something else entirely.
        conftest += "import src.eager  # noqa: F401 — inflate the parent, see TID-18\n"
    (out / "conftest.py").write_text(conftest)
    (out / "pytest.ini").write_text("[pytest]\ntestpaths = tests\n")

    for i in range(args.modules):
        (src / f"mod_{i}.py").write_text(module_src(i))
        (tests / f"test_mod_{i}.py").write_text(
            pytest_module(i, args.tests_per_module, args.work_ms)
        )
    (tests / "test_unit_case.py").write_text(unittest_module(args.work_ms))

    if args.eager_modules:
        eager = src / "eager"
        eager.mkdir()
        for i in range(args.eager_modules):
            (eager / f"eager_{i}.py").write_text(eager_module_src(i, args.eager_kb))
        (eager / "__init__.py").write_text(eager_init_src(args.eager_modules))

    pytest_count = args.modules * args.tests_per_module * 2
    unittest_count = 5
    print(f"Generated {out}")
    print(f"  modules:        {args.modules}")
    print(f"  pytest tests:   {pytest_count}")
    print(f"  unittest tests: {unittest_count}")
    print(f"  total:          {pytest_count + unittest_count}")
    print(f"  work-ms/test:   {args.work_ms}")
    if args.eager_modules:
        print(f"  eager modules:  {args.eager_modules} "
              f"(~{args.eager_kb} KB each, imported by conftest)")


if __name__ == "__main__":
    main()
