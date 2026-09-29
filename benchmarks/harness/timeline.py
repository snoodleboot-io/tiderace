#!/usr/bin/env python3
"""Draw a run's schedule from a `--report` file (TID-78).

Every node in a report from the parallel runner carries `worker`, `unit`, `unit_started_ms` and
`unit_ended_ms` (relative to the run's start). This prints one lane per worker — the units it ran,
when, and how long it sat idle — plus the critical path and the ideal makespan computed from the
same durations, so a benchmark number comes with its explanation rather than a guess.

    timeline.py report.json            # lanes, idle, critical path, ideal
    timeline.py report.json --units 12 # also the twelve longest units
"""
import argparse
import collections
import json
import sys


def load(path):
    with open(path, encoding="utf-8") as fh:
        report = json.load(fh)
    tests = [t for t in report["tests"] if t.get("unit") is not None]
    if not tests:
        sys.exit("no schedule in this report: nodes carry no `unit` (not a parallel-runner run?)")
    units = collections.OrderedDict()
    for t in tests:
        u = units.setdefault(t["unit"], {
            "unit": t["unit"], "worker": t["worker"], "start": t["unit_started_ms"],
            "end": t["unit_ended_ms"], "nodes": [], "dur": 0, "modules": set(),
        })
        u["nodes"].append(t)
        u["dur"] += t.get("duration_ms", 0)
        u["modules"].add(t["node_id"].split("::")[0])
    return report, list(units.values())


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("report")
    ap.add_argument("--units", type=int, default=0, help="also list the N longest units")
    ap.add_argument("--width", type=int, default=100)
    args = ap.parse_args()
    report, units = load(args.report)

    lanes = collections.defaultdict(list)
    for u in units:
        lanes[u["worker"]].append(u)
    for lane in lanes.values():
        lane.sort(key=lambda u: u["start"])
    wall = max(u["end"] for u in units)
    first = min(u["start"] for u in units)
    workers = len(lanes)
    total_dur = sum(u["dur"] for u in units)
    total_span = sum(u["end"] - u["start"] for u in units)
    longest = max(units, key=lambda u: u["end"] - u["start"])
    last = max(units, key=lambda u: u["end"])

    print(f"{len(units)} units on {workers} workers; first unit at {first/1000:.1f}s (start-up), last ends at {wall/1000:.1f}s")
    print(f"test time {total_dur/1000:.1f}s, unit spans {total_span/1000:.1f}s "
          f"(overhead inside units {(total_span-total_dur)/1000:.1f}s), "
          f"busy {100*total_span/(workers*(wall-first)):.0f}% of the parallel window")
    ideal = first + max((longest["end"] - longest["start"]), total_span / workers)
    print(f"ideal makespan from these spans: {ideal/1000:.1f}s "
          f"(critical unit {(longest['end']-longest['start'])/1000:.1f}s: {sorted(longest['modules'])[0]}, "
          f"{len(longest['nodes'])} tests)")
    print(f"last to finish: unit {last['unit']} on worker {last['worker']}, "
          f"{last['start']/1000:.1f}–{last['end']/1000:.1f}s: {sorted(last['modules'])[0]} ({len(last['nodes'])} tests)")
    print()

    scale = args.width / max(wall, 1)
    for w in sorted(lanes):
        lane = lanes[w]
        row = [" "] * args.width
        for i, u in enumerate(lane):
            a, b = int(u["start"] * scale), max(int(u["start"] * scale) + 1, int(u["end"] * scale))
            ch = "#" if i % 2 == 0 else "="
            for x in range(a, min(b, args.width)):
                row[x] = ch
        busy = sum(u["end"] - u["start"] for u in lane)
        end = max(u["end"] for u in lane)
        idle = (end - first) - busy
        print(f"w{w:<2} |{''.join(row)}| ends {end/1000:5.1f}s  idle {idle/1000:4.1f}s  {len(lane)} units")
    print(f"     {'0s':<{args.width//2}}{wall/2000:.0f}s{'':>{args.width//2-6}}{wall/1000:.0f}s")

    if args.units:
        print(f"\n{args.units} longest units (span, test time, tests, worker, start–end):")
        for u in sorted(units, key=lambda u: u["start"] - u["end"])[: args.units]:
            print(f"  {(u['end']-u['start'])/1000:6.1f}s {u['dur']/1000:6.1f}s {len(u['nodes']):4d}  w{u['worker']}  "
                  f"{u['start']/1000:5.1f}–{u['end']/1000:5.1f}s  {', '.join(sorted(u['modules']))[:70]}")


if __name__ == "__main__":
    main()
