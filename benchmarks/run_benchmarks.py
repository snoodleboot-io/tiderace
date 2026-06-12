#!/usr/bin/env python3
"""Benchmark harness: riptide vs. Python test runners on the reference fixture.

This harness measures wall-clock time for several *honest* scenarios using
``hyperfine`` and writes a comparison table to ``benchmarks/RESULTS.md`` and the
raw data to ``benchmarks/results.json``.

Why both cold AND warm scenarios?
---------------------------------
By default riptide runs tests **batched** — one ``pytest`` process per worker
(ADR-009 in ``docs/design/decisions.md``) — so a cold full run pays one
interpreter startup per worker, not per test. That is much faster than the
legacy one-process-per-test path, but still leaves single-process pytest
competitive on a cold full run of many fast tests. riptide's real win is
*impact analysis*: on a *warm* run it hashes sources, consults its persisted
dependency graph, and re-runs only the tests affected by a change — skipping
everything else. To compare fairly we measure both regimes for riptide and the
closest equivalents for the other runners (pytest-testmon also has a cold/warm
story; pytest-xdist parallelises the cold run).

Each scenario runs against the SAME freshly generated fixture, in the fixture
directory, under hyperfine with ``--warmup`` and ``--runs``. Per-run ``--setup``
/ ``--prepare`` hooks establish deterministic state so every timed iteration
measures the intended thing (e.g. a cold run really clears state each time).

Reproduce:
    python benchmarks/run_benchmarks.py
    python benchmarks/run_benchmarks.py --modules 50 --tests-per-module 10 --runs 10
"""
from __future__ import annotations

import argparse
import json
import shlex
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

# --------------------------------------------------------------------------- #
# Paths — everything is anchored to the repo root so the harness is runnable
# from any working directory.
# --------------------------------------------------------------------------- #
BENCH_DIR = Path(__file__).resolve().parent
REPO_ROOT = BENCH_DIR.parent
GENERATE_PY = BENCH_DIR / "fixtures" / "generate.py"
FIXTURE_DIR = BENCH_DIR / "fixtures" / "sample_project"
DEFAULT_RIPTIDE = REPO_ROOT / "target" / "debug" / "riptide"
DEFAULT_VENV_PY = REPO_ROOT / ".riptide-bench-venv" / "bin" / "python"
RESULTS_MD = BENCH_DIR / "RESULTS.md"
RESULTS_JSON = BENCH_DIR / "results.json"

# A source module that exactly one pytest module depends on. Touching it should,
# once impact analysis is healthy, cause only that module's tests to re-run.
TOUCHED_MODULE = "src/mod_0.py"


@dataclass
class Scenario:
    """A single benchmarked command plus the hooks that frame each timed run."""

    key: str            # stable identifier (also the export-json basename)
    label: str          # human-readable row label for RESULTS.md
    command: str        # the command hyperfine times (shell string)
    setup: str = ""     # run once before all runs in this scenario
    prepare: str = ""   # run before EVERY timed run
    cleanup: str = ""   # run once after all runs in this scenario


@dataclass
class ScenarioResult:
    """Outcome of one scenario: timings on success, or an error string."""

    key: str
    label: str
    command: str
    mean: float | None = None
    stddev: float | None = None
    min: float | None = None
    max: float | None = None
    error: str | None = None
    raw: dict = field(default_factory=dict)


# --------------------------------------------------------------------------- #
# Fixture generation
# --------------------------------------------------------------------------- #
def regenerate_fixture(modules: int, tests_per_module: int, work_ms: int,
                       python: str) -> int:
    """Regenerate the fixture deterministically; return the total test count."""
    cmd = [
        python, str(GENERATE_PY),
        "--modules", str(modules),
        "--tests-per-module", str(tests_per_module),
        "--work-ms", str(work_ms),
        "--out", str(FIXTURE_DIR),
    ]
    print(f"  regenerating fixture: {shlex.join(cmd)}")
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        sys.stderr.write(proc.stdout + proc.stderr)
        raise SystemExit("fixture generation failed")
    print("    " + proc.stdout.strip().replace("\n", "\n    "))
    # pytest tests + the 5 unittest tests baked into the generator.
    return modules * tests_per_module * 2 + 5


# --------------------------------------------------------------------------- #
# Scenario construction
# --------------------------------------------------------------------------- #
def build_scenarios(riptide: str, venv_py: str) -> list[Scenario]:
    """Construct every scenario. Commands run with cwd == FIXTURE_DIR.

    Hook commands also run with cwd == FIXTURE_DIR (hyperfine inherits cwd), so
    relative paths like ``tests/`` and ``.riptide.db`` are resolved there.
    """
    rt = shlex.quote(riptide)
    py = shlex.quote(venv_py)

    # riptide invocations -------------------------------------------------- #
    rt_clear = f"{rt} clear --db .riptide.db"
    # Cold full run: ignore impact analysis, run everything.
    rt_all = f"{rt} --all --python {py} tests/"
    # Impact-aware run (no --all): re-run only affected tests.
    rt_impact = f"{rt} --python {py} tests/"
    # Priming run builds the test->file dependency graph; coverage (-c) is what
    # populates test_deps in the state db, so impact analysis has data to use.
    rt_prime = f"{rt} clear --db .riptide.db >/dev/null 2>&1; {rt} -c --python {py} tests/"

    # Restore the pristine generated module then touch it, so each warm-impact
    # run sees exactly one changed source file. We regenerate just that file's
    # content from a saved pristine copy kept in .pristine/.
    restore_touch = (
        f"cp .pristine/{TOUCHED_MODULE} {TOUCHED_MODULE}; "
        f"printf '\\n# benchmark touch\\n' >> {TOUCHED_MODULE}"
    )

    # pytest / testmon / unittest invocations ------------------------------ #
    pytest_q = f"{py} -m pytest -q"
    pytest_xdist = f"{py} -m pytest -q -n auto"
    pytest_testmon = f"{py} -m pytest -q --testmon"
    unittest_cmd = f"{py} -m unittest discover -s tests -t ."

    scenarios = [
        Scenario(
            key="riptide_cold_full",
            label="riptide — cold full run (`--all`)",
            command=rt_all,
            # Each timed run starts from a cleared db => genuinely cold.
            prepare=rt_clear,
        ),
        Scenario(
            key="riptide_warm_noop",
            label="riptide — warm run, no changes (skips all)",
            command=rt_impact,
            # Prime the dep graph once; subsequent timed runs change nothing,
            # so impact analysis should skip every test.
            setup=rt_prime,
        ),
        Scenario(
            key="riptide_warm_one_module",
            label="riptide — warm run, one source module touched",
            command=rt_impact,
            setup=rt_prime,
            # Before every timed run: restore pristine module, then touch it.
            prepare=restore_touch,
            # Leave the fixture pristine afterwards.
            cleanup=f"cp .pristine/{TOUCHED_MODULE} {TOUCHED_MODULE}",
        ),
        Scenario(
            key="pytest_cold",
            label="pytest — baseline (`-q`)",
            command=pytest_q,
        ),
        Scenario(
            key="pytest_xdist",
            label="pytest-xdist — parallel (`-n auto`)",
            command=pytest_xdist,
        ),
        Scenario(
            key="pytest_testmon_cold",
            label="pytest-testmon — cold (no prior data)",
            command=pytest_testmon,
            # Remove testmon's state before each run => always cold.
            prepare="rm -f .testmondata",
        ),
        Scenario(
            key="pytest_testmon_warm",
            label="pytest-testmon — warm, no changes (skips all)",
            command=pytest_testmon,
            # Prime testmon once; timed runs change nothing.
            setup="rm -f .testmondata; " + pytest_testmon,
        ),
        Scenario(
            key="unittest",
            label="unittest — discover",
            command=unittest_cmd,
        ),
    ]
    return scenarios


def snapshot_pristine() -> None:
    """Save a pristine copy of the fixture sources for restore-and-touch hooks."""
    pristine = FIXTURE_DIR / ".pristine"
    if pristine.exists():
        shutil.rmtree(pristine)
    (pristine / "src").mkdir(parents=True)
    shutil.copy(FIXTURE_DIR / TOUCHED_MODULE, pristine / TOUCHED_MODULE)


# --------------------------------------------------------------------------- #
# Running scenarios via hyperfine
# --------------------------------------------------------------------------- #
def run_scenario(sc: Scenario, runs: int, warmup: int,
                 json_dir: Path) -> ScenarioResult:
    """Time one scenario with hyperfine; capture errors instead of crashing."""
    export_json = json_dir / f"{sc.key}.json"
    export_md = json_dir / f"{sc.key}.md"
    cmd = [
        "hyperfine",
        "--runs", str(runs),
        "--warmup", str(warmup),
        "--shell", "bash",
        "--export-json", str(export_json),
        "--export-markdown", str(export_md),
        "--ignore-failure",  # some runners exit non-zero when 0 tests run; record time anyway
    ]
    if sc.setup:
        cmd += ["--setup", sc.setup]
    if sc.prepare:
        cmd += ["--prepare", sc.prepare]
    if sc.cleanup:
        cmd += ["--cleanup", sc.cleanup]
    cmd += ["--command-name", sc.key, sc.command]

    print(f"\n=== {sc.label} ===")
    print(f"    cmd: {sc.command}")
    proc = subprocess.run(cmd, cwd=FIXTURE_DIR, text=True)

    result = ScenarioResult(key=sc.key, label=sc.label, command=sc.command)
    if proc.returncode != 0 or not export_json.exists():
        result.error = f"hyperfine exited {proc.returncode}"
        return result

    try:
        data = json.loads(export_json.read_text())
        entry = data["results"][0]
    except (json.JSONDecodeError, KeyError, IndexError) as exc:
        result.error = f"could not parse hyperfine output: {exc}"
        return result

    result.raw = entry
    result.mean = entry.get("mean")
    result.stddev = entry.get("stddev")
    result.min = entry.get("min")
    result.max = entry.get("max")
    return result


# --------------------------------------------------------------------------- #
# Reporting
# --------------------------------------------------------------------------- #
def write_results(results: list[ScenarioResult], meta: dict) -> None:
    """Write RESULTS.md (markdown table) and results.json (raw)."""
    # Relative speed is computed against the fastest successful scenario.
    successful = [r for r in results if r.mean is not None]
    baseline = min((r.mean for r in successful), default=None)

    lines: list[str] = []
    lines.append("# riptide benchmark results")
    lines.append("")
    lines.append(
        "Generated by `benchmarks/run_benchmarks.py`. "
        "All scenarios ran on the same fixture; times are wall-clock seconds "
        "measured by hyperfine."
    )
    lines.append("")
    lines.append("## Configuration")
    lines.append("")
    lines.append(f"- modules: **{meta['modules']}**")
    lines.append(f"- tests per module: **{meta['tests_per_module']}**")
    lines.append(f"- total tests: **{meta['total_tests']}**")
    lines.append(f"- per-test work: **{meta['work_ms']} ms** "
                 "(0 = pure-CPU; interpreter startup dominates)")
    lines.append(f"- hyperfine runs: **{meta['runs']}** (warmup {meta['warmup']})")
    lines.append(f"- riptide: `{meta['riptide']}`")
    lines.append(f"- python: `{meta['python']}`")
    lines.append("")
    lines.append("## Results")
    lines.append("")
    lines.append("| scenario | mean (s) | min (s) | max (s) | relative |")
    lines.append("|----------|---------:|--------:|--------:|---------:|")
    for r in results:
        if r.mean is None:
            lines.append(f"| {r.label} | — | — | — | ERROR: {r.error} |")
            continue
        rel = (r.mean / baseline) if baseline else 1.0
        lines.append(
            f"| {r.label} | {r.mean:.3f} | {r.min:.3f} | {r.max:.3f} "
            f"| {rel:.2f}× |"
        )
    lines.append("")
    lines.append("`relative` is each scenario's mean divided by the fastest "
                 "successful scenario's mean (lower is faster).")
    lines.append("")
    lines.append("## Reading these numbers")
    lines.append("")
    lines.append(
        "- **riptide warm runs are the win.** With an unchanged tree riptide "
        "skips every test; after a one-module edit it re-runs only the affected "
        "tests. This is the everyday edit→test loop — compare it against "
        "**pytest-testmon warm** and the full **pytest** baseline."
    )
    lines.append(
        "- **riptide cold full run** (`--all`) runs tests **batched** — one "
        "pytest process per worker (ADR-009), far faster than one process per "
        "test — but still pays one interpreter startup per worker. Its cost is "
        "roughly flat with test count (dominated by the fixed per-worker "
        "startups), so on many fast tests single-process **pytest** can still "
        "edge it out. For running *everything* once, pytest or pytest-xdist is "
        "often fastest."
    )
    lines.append(
        "- **Caching matters.** The cold scenario clears `.riptide.db` before "
        "each timed run to measure the uncached worst case. In real use you pay "
        "that once: the first run (or any `--all` run) is full; every run after "
        "that uses the persisted state for impact analysis. In CI, cache "
        "`.riptide.db` (and `.riptide-coverage/`) across runs to get the warm "
        "speedup — a fresh checkout with no cache is always a cold full run."
    )
    lines.append("")
    RESULTS_MD.write_text("\n".join(lines))
    print(f"\nwrote {RESULTS_MD}")

    payload = {
        "meta": meta,
        "results": [
            {
                "key": r.key,
                "label": r.label,
                "command": r.command,
                "mean": r.mean,
                "stddev": r.stddev,
                "min": r.min,
                "max": r.max,
                "error": r.error,
                "raw": r.raw,
            }
            for r in results
        ],
    }
    RESULTS_JSON.write_text(json.dumps(payload, indent=2))
    print(f"wrote {RESULTS_JSON}")


# --------------------------------------------------------------------------- #
# Preflight
# --------------------------------------------------------------------------- #
def preflight(riptide: str, venv_py: str) -> None:
    """Fail early with a clear message if a required tool is missing."""
    if shutil.which("hyperfine") is None:
        raise SystemExit("error: hyperfine not found on PATH (need v1.x).")
    if not Path(riptide).exists():
        raise SystemExit(
            f"error: riptide binary not found at {riptide}. "
            "Run `cargo build` in the repo root."
        )
    if not Path(venv_py).exists():
        raise SystemExit(
            f"error: venv python not found at {venv_py}. "
            "Lane 0 provisions .riptide-bench-venv/."
        )
    if not GENERATE_PY.exists():
        raise SystemExit(f"error: fixture generator missing at {GENERATE_PY}.")


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #
def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--modules", type=int, default=50,
                    help="number of source modules / pytest test modules")
    ap.add_argument("--tests-per-module", type=int, default=10,
                    help="pytest tests per module (×2: compute + bounded)")
    ap.add_argument("--work-ms", type=int, default=0,
                    help="fixed sleep per test in ms (0 = pure CPU, default)")
    ap.add_argument("--runs", type=int, default=10,
                    help="hyperfine timed runs per scenario")
    ap.add_argument("--warmup", type=int, default=1,
                    help="hyperfine warmup runs per scenario")
    ap.add_argument("--riptide", default=str(DEFAULT_RIPTIDE),
                    help="path to the riptide binary")
    ap.add_argument("--python", default=str(DEFAULT_VENV_PY),
                    help="python interpreter for the runners (the bench venv)")
    return ap.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    preflight(args.riptide, args.python)

    total = regenerate_fixture(args.modules, args.tests_per_module,
                               args.work_ms, args.python)
    snapshot_pristine()

    json_dir = BENCH_DIR / ".hyperfine"
    json_dir.mkdir(exist_ok=True)

    scenarios = build_scenarios(args.riptide, args.python)
    results: list[ScenarioResult] = []
    for sc in scenarios:
        try:
            results.append(run_scenario(sc, args.runs, args.warmup, json_dir))
        except Exception as exc:  # never let one runner kill the whole harness
            results.append(ScenarioResult(
                key=sc.key, label=sc.label, command=sc.command,
                error=f"harness exception: {exc}",
            ))

    meta = {
        "modules": args.modules,
        "tests_per_module": args.tests_per_module,
        "total_tests": total,
        "work_ms": args.work_ms,
        "runs": args.runs,
        "warmup": args.warmup,
        "riptide": args.riptide,
        "python": args.python,
    }
    write_results(results, meta)

    errored = [r for r in results if r.error]
    if errored:
        print(f"\n{len(errored)} scenario(s) recorded an error "
              "(see RESULTS.md / results.json):")
        for r in errored:
            print(f"  - {r.key}: {r.error}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
