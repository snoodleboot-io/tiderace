"""The package's shape (TID-124, step 5): every module imports on its own, the import graph is a
DAG in the stated direction, no module outgrows what one reader can hold, and the things the
redesign retired stay retired."""
from __future__ import annotations

import ast
import os
import re
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
PKG = os.path.join(os.path.dirname(HERE), "tiderace_shim")
MODULES = sorted(f[:-3] for f in os.listdir(PKG) if f.endswith(".py") and not f.startswith("__"))

# Each module's level; an import must point at a lower level. `modes` is the top: nothing imports it.
LEVEL = {
    "modes": 6,
    "engine": 5,
    "plan": 4, "tiers": 4, "discovery": 4,
    "invoke": 3, "isolation": 3, "footprint": 3, "fixtures": 3, "selection": 3,
    "pytest_compat": 2,
    "config": 1, "nodes": 1, "results": 1, "protocol": 1,
    "safe": 0, "log": 0,
}


def imports_of(module: str) -> set[str]:
    tree = ast.parse(open(os.path.join(PKG, module + ".py"), encoding="utf-8").read())
    out = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom) and node.level == 1 and node.module:
            out.add(node.module.split(".")[0])
    return out


def test_every_module_is_accounted_for():
    assert set(MODULES) == set(LEVEL), set(MODULES) ^ set(LEVEL)


@pytest.mark.parametrize("module", MODULES)
def test_each_module_imports_on_its_own(module):
    env = dict(os.environ, PYTHONPATH=os.path.dirname(PKG))
    proc = subprocess.run([sys.executable, "-c", f"import tiderace_shim.{module}"], env=env,
                          capture_output=True, text=True, timeout=60)
    assert proc.returncode == 0, proc.stderr[-2000:]


def test_the_import_graph_points_downward():
    """`modes → engine → {plan, tiers, discovery} → {invoke, isolation, footprint, fixtures, selection}
    → pytest_compat → {config, nodes, results, protocol} → {safe, log}`; a type-only import under
    `TYPE_CHECKING` does not count (the annotation never runs)."""
    for module in MODULES:
        tree = ast.parse(open(os.path.join(PKG, module + ".py"), encoding="utf-8").read())
        runtime = set()
        for node in tree.body:  # top-level only: an `if TYPE_CHECKING:` block is skipped
            if isinstance(node, ast.ImportFrom) and node.level == 1 and node.module:
                runtime.add(node.module.split(".")[0])
        for dep in runtime:
            assert LEVEL[dep] < LEVEL[module], f"{module} (level {LEVEL[module]}) imports {dep} (level {LEVEL[dep]})"


def test_no_module_outgrows_a_reader():
    for module in MODULES:
        lines = sum(1 for _ in open(os.path.join(PKG, module + ".py"), encoding="utf-8"))
        assert lines <= 1200, f"{module}.py is {lines} lines"


def test_what_the_redesign_retired_stays_retired():
    sources = {m: open(os.path.join(PKG, m + ".py"), encoding="utf-8").read() for m in MODULES}
    for m, src in sources.items():
        assert not re.search(r"^\s+global ", src, re.M), f"{m}.py has a `global` statement"
        assert not re.search(r"^(?:async )?def _.*_async", src, re.M), f"{m}.py has an async twin"
        assert 'sys.modules["shim"]' not in src and 'setdefault("shim"' not in src, f"{m}.py aliases the shim"
    assert not os.path.exists(os.path.join(PKG, "_shim.py"))
