"""N5 conformance — **run-through tier** (ROADMAP-v2 B6). Beyond the static auto-map %, this *executes*
a real suite **through tiderace's engine** and diffs every test's outcome against an oracle run, yielding
an **execution pass-rate** (the number that actually matters for adoption) and naming any divergences as
engine bugs to file.

This first target is a pure-`unittest` repo (cachetools): unittest needs **no migration** (tiderace
drives `TestCase` natively, ADR-E001), so it isolates the *execution* path. The oracle is stock
`unittest` (a `TestSuite` run, which honors `setUpClass`) — the correct oracle for a unittest suite;
pytest merely wraps the same machinery for these.

Usage:  python3 runthrough.py <repo-root> [--src <dir-to-add-to-path>] [--fork]
        python3 runthrough.py vendor/cachetools --src vendor/cachetools/src
"""
from __future__ import annotations

import argparse
import importlib
import os
import sys
import unittest

_HERE = os.path.dirname(os.path.abspath(__file__))


def _setup_paths(root: str, src: str | None) -> None:
    sys.path.insert(0, os.path.join(_HERE, os.pardir, "engine", "py-tiderace"))  # tiderace
    sys.path.insert(0, os.path.join(_HERE, os.pardir, "engine", "py-shim"))     # shim
    sys.path.insert(0, root)
    if src:
        sys.path.insert(0, src)


def _test_modules(root: str) -> list[tuple[str, str]]:
    """(import_name, module_key) for every test module under `root`."""
    out = []
    for cur, _dirs, files in sorted(os.walk(root)):
        if os.sep + ".git" in cur:
            continue
        for name in sorted(files):
            if name.endswith(".py") and (name.startswith("test_") or name.endswith("_test.py")):
                path = os.path.join(cur, name)
                rel = os.path.relpath(path, root)
                out.append((rel[:-3].replace(os.sep, "."), rel.replace(os.sep, "/")))
    return out


def _unittest_nodes(import_name: str, module_key: str) -> list[tuple[str, str]]:
    """(node_id, 'unittest_method') for each TestCase test method in a module (skips abstract mixins)."""
    module = importlib.import_module(import_name)
    nodes = []
    for cls_name, obj in vars(module).items():
        if isinstance(obj, type) and issubclass(obj, unittest.TestCase) and obj is not unittest.TestCase:
            for method in unittest.TestLoader().getTestCaseNames(obj):
                nodes.append((f"{module_key}::{cls_name}::{method}", "unittest_method"))
    return nodes


def _oracle(module_key: str, cls_name: str, method: str) -> str:
    """Stock-unittest outcome for one method (a one-test TestSuite — honors setUpClass/tearDownClass)."""
    module = importlib.import_module(_module_import(module_key))
    cls = getattr(module, cls_name)
    result = unittest.TestResult()
    unittest.TestSuite([cls(method)]).run(result)
    if result.errors:
        return "error"
    if result.failures:
        return "failed"
    if getattr(result, "unexpectedSuccesses", None):
        return "failed"
    if getattr(result, "expectedFailures", None):
        return "xfail"
    if result.skipped:
        return "skipped"
    return "passed"


def _module_import(module_key: str) -> str:
    return module_key[:-3].replace("/", ".") if module_key.endswith(".py") else module_key


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="runthrough", description="execute a suite through tiderace vs an oracle")
    ap.add_argument("root")
    ap.add_argument("--src", default=None, help="extra dir to add to sys.path (the package under test)")
    ap.add_argument("--fork", action="store_true", help="use the fork executor (default: in-process)")
    args = ap.parse_args(argv)

    root = os.path.abspath(args.root)
    src = os.path.abspath(args.src) if args.src else None
    _setup_paths(root, src)
    import shim  # noqa: E402 — after sys.path is set

    print(f"=== N5 run-through: {os.path.basename(root)} executed THROUGH tiderace vs unittest oracle ===\n")
    shim._preimport(root)
    reg = shim._discover(root)
    engine = shim.Engine(reg, no_fork=not args.fork, root=root)

    nodes: list[tuple[str, str]] = []
    for import_name, module_key in _test_modules(root):
        try:
            nodes.extend(_unittest_nodes(import_name, module_key))
        except Exception as exc:  # noqa: BLE001 — a module that won't import is itself a finding
            print(f"  [skip] {module_key}: import failed: {type(exc).__name__}: {exc}")

    match = mismatch = 0
    diffs: list[str] = []
    for node_id, style in nodes:
        _, cls_name, method = node_id.split("::")
        try:
            got = engine.run(node_id, style, 10000)["outcome"]
        except Exception as exc:  # noqa: BLE001
            got = f"engine-error:{type(exc).__name__}"
        want = _oracle(node_id.split("::")[0], cls_name, method)
        if got == want:
            match += 1
        else:
            mismatch += 1
            diffs.append(f"  DIFF {node_id}: tiderace={got} oracle={want}")
    engine.teardown_all()

    total = match + mismatch
    rate = 100.0 * match / total if total else 0.0
    print(f"[result] {total} tests executed through the engine")
    print(f"[result] execution pass-rate (engine == oracle): {match}/{total} = {rate:.1f}%")
    if diffs:
        print(f"\n[divergences] {len(diffs)} — each is an engine bug to file:")
        print("\n".join(diffs[:50]))
    else:
        print("\n[divergences] none — the engine reproduces the oracle exactly on this suite")
    return 0 if mismatch == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
