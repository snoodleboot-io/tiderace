"""N5 conformance — run `riptide migrate` over real, pinned pytest repos and report (a) the auto-map
rate and (b) the can't-map distribution, ranked. Pure `ast` (no install, no execution): it measures how
much real-world pytest authoring maps to riptide's type-DI surface, and exactly what doesn't — which
data-drives what to build next (builtins, request handling, …).

Usage:  python conformance.py vendor/<repo> [vendor/<repo> ...]
"""
from __future__ import annotations

import collections
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(_HERE, os.pardir, "engine", "py-riptide"))

from riptide.migrate import migrate_source  # noqa: E402

# can't-map message -> bucket. First match wins, so order specific → general.
CATEGORIES = [
    ("untyped provider", ("has no return type",)),
    ("untyped fixture param", ("provider is untyped",)),
    ("parametrized fixture", ("is parametrized",)),
    ("request introspection", ("uses `request`",)),
    ("usefixtures", ("usefixtures",)),
    ("pytest builtin", ("builtin",)),
    ("pytest_* hook", ("hook",)),
    ("from-pytest import", ("rewrite imports",)),
]


def categorize(msg: str) -> str:
    for label, keys in CATEGORIES:
        if any(k in msg for k in keys):
            return label
    return "other"


def _is_test_file(name: str) -> bool:
    return name == "conftest.py" or name.startswith("test_") or name.endswith("_test.py")


def scan_repo(root: str):
    files = mapped = cant = errors = 0
    buckets: collections.Counter = collections.Counter()
    for cur, dirs, names in os.walk(root):
        if os.sep + ".git" in cur:
            continue
        for fname in names:
            if not fname.endswith(".py") or not _is_test_file(fname):
                continue
            path = os.path.join(cur, fname)
            try:
                src = open(path, encoding="utf-8", errors="replace").read()
                _migrated, report = migrate_source(src)
            except SyntaxError:
                errors += 1
                continue
            files += 1
            mapped += len(report.mappings)
            for finding in report.cant_map:
                cant += 1
                buckets[categorize(finding.message)] += 1
    return files, mapped, cant, errors, buckets


def _rate(mapped: int, cant: int) -> float:
    total = mapped + cant
    return 100.0 * mapped / total if total else 0.0


def main(argv: list) -> int:
    if not argv:
        print("usage: python conformance.py vendor/<repo> [...]", file=sys.stderr)
        return 2

    print("=== N5 conformance: `riptide migrate` over real pytest repos (pure ast) ===\n")
    tot_files = tot_mapped = tot_cant = tot_err = 0
    agg: collections.Counter = collections.Counter()

    for root in argv:
        name = os.path.basename(root.rstrip("/"))
        files, mapped, cant, errors, buckets = scan_repo(root)
        print(f"[{name}]  {files} test files  |  mapped {mapped}  |  can't-map {cant}  "
              f"|  auto-map {_rate(mapped, cant):.0f}%" + (f"  (skipped {errors} unparseable)" if errors else ""))
        for label, n in buckets.most_common():
            print(f"      - {label}: {n}")
        print()
        tot_files += files
        tot_mapped += mapped
        tot_cant += cant
        tot_err += errors
        agg += buckets

    print(f"=== TOTAL  {tot_files} files  |  mapped {tot_mapped}  |  can't-map {tot_cant}  "
          f"|  auto-map {_rate(tot_mapped, tot_cant):.0f}% ===")
    print("\ncan't-map distribution — what to build next, ranked:")
    for label, n in agg.most_common():
        share = 100.0 * n / tot_cant if tot_cant else 0.0
        print(f"    {label:<24} {n:>4}  ({share:.0f}%)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
