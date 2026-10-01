"""Where a pass's JSON lands, and how it is written and read back (TID-112).

Every file sits beside this module: `report-<corpus>.json` is the per-node `--report` tiderace
wrote for that corpus, and each pass keeps its own results file (`parity.json`, `timing.json`,
`scale.json`, …). Four passes used to build the `report-<corpus>.json` name themselves and seven
wrote with `json.dump(x, open(path, "w"))`.
"""
from __future__ import annotations

import json
import os
from typing import Any

HERE = os.path.dirname(os.path.abspath(__file__))


def report_path(kind: str, tag: str | None = None) -> str:
    """`<kind>-<tag>.json` beside this module, or `<kind>.json` with no tag: `report_path("report",
    "click")`, `report_path("parity")`."""
    return os.path.join(HERE, f"{kind}-{tag}.json" if tag else f"{kind}.json")


def fresh_report(kind: str, tag: str | None = None) -> str:
    """The path a run's `--report` goes to, with any earlier file removed first: a stale report
    from a previous pass must never be read as this one's."""
    path = report_path(kind, tag)
    if os.path.exists(path):
        os.remove(path)
    return path


def write_json(path: str, obj: Any, indent: int = 1) -> str:
    with open(path, "w") as fh:
        json.dump(obj, fh, indent=indent)
    return path


def read_json(path: str, default: Any = None) -> Any:
    """The file's contents, or `default` when there is no file."""
    if not os.path.exists(path):
        return default
    with open(path) as fh:
        return json.load(fh)
