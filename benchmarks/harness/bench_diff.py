"""Two cold-timing passes side by side: `python -m benchmarks.harness.bench_diff timing-oct1.json timing-oct2.json`.

Both files are `timing_rr` output (medians and peak memory per suite and tool). Prints each suite's
medians, the change, and tiderace's ratio to pytest and to xdist in each pass — the first thing to
read after a pass, before any table is updated. A change the other tools do not share is the
runner's; one they all share is the machine's.
"""
import json
import os
import sys

from .reports import HERE

TOOLS = ["pytest", "pytest -n auto", "tiderace"]


def load(name: str) -> dict:
    path = name if os.path.exists(name) else os.path.join(HERE, name)
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def main(argv):
    if len(argv) != 2:
        sys.exit(__doc__)
    a, b = load(argv[0]), load(argv[1])
    print(f"{'suite':12} {'tool':15} {'before s':>9} {'after s':>9} {'change':>8} {'before MB':>10} {'after MB':>9}")
    for suite in a:
        if suite not in b:
            continue
        for tool in TOOLS:
            m1, m2 = a[suite]["median"].get(tool), b[suite]["median"].get(tool)
            r1, r2 = a[suite].get("peak_rss_mb", {}).get(tool), b[suite].get("peak_rss_mb", {}).get(tool)
            change = f"{(m2 - m1) / m1 * 100:+.0f}%" if m1 and m2 else "-"
            print(f"{suite:12} {tool:15} {m1 if m1 is not None else '-':>9} {m2 if m2 is not None else '-':>9} "
                  f"{change:>8} {r1 if r1 is not None else '-':>10} {r2 if r2 is not None else '-':>9}")
    print()
    for suite in a:
        if suite not in b:
            continue
        p1, x1, t1 = (a[suite]["median"].get(k) for k in TOOLS)
        p2, x2, t2 = (b[suite]["median"].get(k) for k in TOOLS)
        ratio = lambda p, t: f"{p / t:.2f}x" if p and t else "-"  # noqa: E731
        print(f"{suite:12} vs pytest {ratio(p1, t1):>7} -> {ratio(p2, t2):>7}   vs xdist {ratio(x1, t1):>7} -> {ratio(x2, t2):>7}")


if __name__ == "__main__":
    main(sys.argv[1:])
