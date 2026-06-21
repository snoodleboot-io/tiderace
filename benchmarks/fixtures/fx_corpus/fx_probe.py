"""Deterministic instrumentation shared by the fx_corpus fixtures.

No randomness, no timestamps: every value is derived from explicit arguments so
the corpus is regenerable byte-for-byte and the recorded events are stable
across runs (modulo pytest's own collection order, which is itself stable).

Two artifacts are written into a per-run directory (provided by the
``probe_dir`` session fixture, NOT the committed tree):

    events.log  - one line per setup/teardown, in execution order
    counts.json - {fixture_name: times_its_body_ran}

The Rust side asserts: (1) teardown order == reverse(setup order) per scope,
(2) wider-scope fixture bodies ran once while function fixtures ran per test.
"""
import json
import os
from pathlib import Path

ENV_DIR = "FX_CORPUS_PROBE_DIR"


def _dir() -> Path:
    d = os.environ.get(ENV_DIR)
    if not d:
        raise RuntimeError(
            "FX_CORPUS_PROBE_DIR not set; the probe_dir session fixture must "
            "run first"
        )
    p = Path(d)
    p.mkdir(parents=True, exist_ok=True)
    return p


def record_event(line: str) -> None:
    """Append a single ordered event line to events.log."""
    with (_dir() / "events.log").open("a", encoding="utf-8") as fh:
        fh.write(line + "\n")


def bump_count(name: str) -> int:
    """Increment and return the run-count for a fixture body."""
    path = _dir() / "counts.json"
    counts = {}
    if path.exists():
        counts = json.loads(path.read_text(encoding="utf-8"))
    counts[name] = counts.get(name, 0) + 1
    path.write_text(json.dumps(counts, sort_keys=True), encoding="utf-8")
    return counts[name]


def setup(name: str) -> None:
    record_event("SETUP   " + name)
    bump_count(name)


def teardown(name: str) -> None:
    record_event("TEARDOWN " + name)
