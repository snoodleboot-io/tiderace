"""Reconstruct the scheduler's bins from a `--report` and charge them what was actually measured.

    python benchmarks/harness/analyse_bins.py benchmarks/harness/report-pirn-agents.json

`LocalityScheduler` is deterministic, so the bins it produced for a run can be rebuilt from the
algorithm and then charged the per-node durations the report recorded — the bins that ran, not a
simulation of them. This is how TID-52 found that pirn-agents' bins were 121/97/66/34/31/25/23/19
seconds: 2.32x the perfect-balance floor, the machine 57% idle.

Two units matter and they are not the same. The scheduler packs **collected** items — what the
regex collector found — and a parametrized item is one of those however many cases it expands into.
The report lists **expanded** nodes. So every collected item is folded back together here.

Packings compared:
  cold          static partition, every item weighted 1 — what shipped before TID-52
  warm          the same partition given each item's measured duration
  group-steal   one unit per module from a queue, cold order (by count) and warm order (by duration)
                — what ships now (TID-52), before and after recorded durations (TID-62)
  steal         per-test work stealing: the floor a dynamic scheduler reaches (xdist's model)
"""
import json, os, statistics, sys

WORKERS = int(os.environ.get("WORKERS", 8))
SPLIT_THRESHOLD = 1.5


def locality_key(node_id):
    return node_id.split("::", 1)[0]


def collected_id(node_id, expanded):
    if expanded and node_id.endswith("]") and "[" in node_id:
        return node_id[: node_id.rindex("[")]
    return node_id


def load(path):
    raw = json.load(open(path))
    items = {}
    for t in raw["tests"]:
        cid = collected_id(t["node_id"], t.get("expanded", False))
        items[cid] = items.get(cid, 0) + t.get("duration_ms", 0)
    return items, raw


def pack(items, weights, workers=WORKERS, split_threshold=SPLIT_THRESHOLD):
    groups = {}
    for cid in items:
        groups.setdefault(locality_key(cid), []).append(cid)
    total = sum(weights.values())
    split_cap = int((total / workers) * split_threshold)
    ordered = sorted(((k, v, sum(weights[c] for c in v)) for k, v in groups.items()),
                     key=lambda g: (-g[2], g[0]))
    bins, load_ = [[] for _ in range(workers)], [0] * workers
    for _key, members, gtotal in ordered:
        if gtotal > split_cap and len(members) > 1 and split_cap > 0:
            for c in sorted(members, key=lambda c: -weights[c]):
                w = load_.index(min(load_)); bins[w].append(c); load_[w] += weights[c]
        else:
            w = load_.index(min(load_))
            for c in members:
                bins[w].append(c); load_[w] += weights[c]
    return bins


def steal(items, workers=WORKERS):
    load_ = [0] * workers
    for _cid, ms in sorted(items.items(), key=lambda kv: -kv[1]):
        w = load_.index(min(load_)); load_[w] += ms
    return load_


def group_steal(items, workers=WORKERS, order="count"):
    groups = {}
    for cid, ms in items.items():
        g = groups.setdefault(locality_key(cid), [0, 0]); g[0] += ms; g[1] += 1
    queue = sorted(groups.values(), key=lambda g: -(g[1] if order == "count" else g[0]))
    load_ = [0] * workers
    for ms, _n in queue:
        w = load_.index(min(load_)); load_[w] += ms
    return load_


def report(name, path):
    items, raw = load(path)
    total = sum(items.values()); floor = total / WORKERS
    durs = sorted(items.values(), reverse=True)
    print(f"\n=== {name} ===")
    print(f"{raw['total']} reported nodes from {len(items)} collected items; total test time "
          f"{total / 1000:.1f}s; perfect-balance floor at {WORKERS} workers {floor / 1000:.1f}s")
    print(f"per-item ms: median {statistics.median(durs):.0f}  mean {statistics.mean(durs):.0f}  "
          f"p90 {durs[int(len(durs) * 0.10)]:.0f}  p99 {durs[int(len(durs) * 0.01)]:.0f}  max {durs[0]:.0f}")
    top1 = sum(durs[: max(1, len(durs) // 100)])
    print(f"the slowest 1% of items are {100 * top1 / total:.1f}% of all test time")
    uniform = {c: 1 for c in items}
    for label, bins in (
        ("cold: static partition, weight 1        ", [sum(items[c] for c in b) for b in pack(items, uniform)]),
        ("warm: static partition, true durations  ", [sum(items[c] for c in b) for b in pack(items, items)]),
        ("group-steal, cold order (by test count) ", group_steal(items, order="count")),
        ("group-steal, warm order (by duration)   ", group_steal(items, order="time")),
        ("steal: per-test work stealing (xdist)   ", steal(items)),
    ):
        span, idle = max(bins), sum(max(bins) - b for b in bins)
        print(f"  {label}  makespan {span / 1000:6.1f}s  idle {idle / 1000:6.1f}s "
              f"({100 * idle / (span * WORKERS):4.1f}%)  {span / floor:.2f}x floor")
        print(f"  {'':40s}  bins {' '.join(f'{b / 1000:.0f}' for b in sorted(bins, reverse=True))}")
    forked = sum(1 for t in raw["tests"] if t.get("must_fork"))
    print(f"  demoted (must_fork) {forked}   of {raw['total']} nodes")


if __name__ == "__main__":
    for arg in sys.argv[1:]:
        report(os.path.basename(arg).replace("report-", "").replace(".json", ""), arg)
