"""How the runners scale with suite size, on the synthetic suites `scale_corpus` generates.

    python -m benchmarks.harness.scale_bench 2000 20000 --python .tiderace-fx-venv/bin/python

For each size: the suite generated under a temp dir, then — cold, no daemon — pytest, pytest -n
auto, tiderace, and the one thing a developer does most, `-k` one test by name: pytest's `-k`,
tiderace's `-k` without a daemon, and tiderace's `-k` through a warm daemon (after one full run
through it), three runs each. Trivial tests throughout, so what grows is the runner's own cost:
collection, dispatch, selection, reporting. Writes `scale.json` beside this file.
"""
import argparse
import os
import shutil
import statistics
import subprocess
import sys
import tempfile

from .corpora import R, TIDERACE, clean_env, load_average, tiderace_env
from .reports import report_path, write_json
from .runs import pytest_cmd, tiderace_cmd, timed


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("sizes", nargs="+", type=int, help="tests per suite, e.g. 2000 20000")
    ap.add_argument("--python", required=True)
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--xdist-path", default=os.environ.get("TIDERACE_XDIST_PATH", ""))
    a = ap.parse_args()
    py = os.path.abspath(a.python)
    base = clean_env()
    xenv = dict(base, PYTHONPATH=a.xdist_path) if a.xdist_path else base
    out_path = report_path("scale")
    results = {}
    for size in a.sizes:
        per_module = 50
        modules = max(size // per_module, 1)
        packages = max(modules // 20, 1)
        root = tempfile.mkdtemp(prefix=f"tiderace_scale_{size}_")
        subprocess.run([sys.executable, "-m", "benchmarks.harness.scale_corpus", "--packages", str(packages),
                        "--modules", str(modules // packages), "--tests", str(per_module), "--out", root],
                       check=True, capture_output=True, cwd=R)
        tests = os.path.join(root, "tests")
        tenv = dict(tiderace_env(), TIDERACE_PYTHON=py)
        local = dict(tenv, TIDERACE_NO_DAEMON="1")
        one = "test_p00m00_010"
        print(f"== {size} tests ({modules} modules) at {root}", flush=True)
        rows = {}

        def measure(label, cmd, env, cwd=root):
            samples = []
            for _ in range(a.rounds):
                t = timed(cmd, cwd, env)
                samples.append(round(t.seconds, 2))
                print(f"   {label:34} {samples[-1]:7.2f}s  rc {t.returncode}  load {load_average():.1f}", flush=True)
            rows[label] = {"median": statistics.median(samples), "runs": samples}

        measure("pytest", pytest_cmd(py, "tests"), base)
        measure("pytest -n auto", pytest_cmd(py, "tests", "-n", "auto"), xenv)
        measure("tiderace", tiderace_cmd(tests), local)
        measure("pytest -k one", pytest_cmd(py, "tests", "-k", one), base)
        measure("tiderace -k one, no daemon", tiderace_cmd(tests, "-k", one), local)
        subprocess.run([TIDERACE, "daemon", "start", tests], cwd=root, env=tenv, capture_output=True)
        timed(tiderace_cmd(tests), root, tenv)  # the image, and the records
        measure("tiderace -k one, warm daemon", tiderace_cmd(tests, "-k", one), tenv)
        measure("tiderace, warm daemon", tiderace_cmd(tests), tenv)
        subprocess.run([TIDERACE, "daemon", "stop", tests], cwd=root, env=tenv, capture_output=True)
        print("   medians: " + "  ".join(f"{k} {v['median']:.2f}s" for k, v in rows.items()), flush=True)
        results[str(size)] = {"modules": modules, "rows": rows}
        write_json(out_path, results, indent=1)
        shutil.rmtree(root, ignore_errors=True)
    print(f"wrote {out_path}")


if __name__ == "__main__":
    main()
