"""How the runners scale with suite size, on the synthetic suites `scale_corpus.py` generates.

    python benchmarks/harness/scale_bench.py 2000 20000 --python .tiderace-fx-venv/bin/python

For each size: the suite generated under a temp dir, then — cold, no daemon — pytest, pytest -n
auto, tiderace, and the one thing a developer does most, `-k` one test by name: pytest's `-k`,
tiderace's `-k` without a daemon, and tiderace's `-k` through a warm daemon (after one full run
through it), three runs each. Trivial tests throughout, so what grows is the runner's own cost:
collection, dispatch, selection, reporting. Writes `scale.json` beside this file.
"""
import argparse, json, os, shutil, statistics, subprocess, sys, tempfile, time
sys.path.insert(0, os.path.dirname(__file__))
from corpora import TIDERACE, clean_env, load, tiderace_env

HERE = os.path.dirname(os.path.abspath(__file__))


def timed(cmd, cwd, env):
    t0 = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=3600)
    return round(time.perf_counter() - t0, 2), p.returncode


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
    results = {}
    for size in a.sizes:
        per_module = 50
        modules = max(size // per_module, 1)
        packages = max(modules // 20, 1)
        root = tempfile.mkdtemp(prefix=f"tiderace_scale_{size}_")
        subprocess.run([sys.executable, os.path.join(HERE, "scale_corpus.py"), "--packages", str(packages),
                        "--modules", str(modules // packages), "--tests", str(per_module), "--out", root],
                       check=True, capture_output=True)
        tests = os.path.join(root, "tests")
        tenv = dict(tiderace_env(), TIDERACE_PYTHON=py)
        local = dict(tenv, TIDERACE_NO_DAEMON="1")
        one = "test_p00m00_010"
        print(f"== {size} tests ({modules} modules) at {root}", flush=True)
        rows = {}

        def measure(label, cmd, env, cwd=root):
            samples = []
            for _ in range(a.rounds):
                secs, rc = timed(cmd, cwd, env)
                samples.append(secs)
                print(f"   {label:34} {secs:7.2f}s  rc {rc}  load {load():.1f}", flush=True)
            rows[label] = {"median": statistics.median(samples), "runs": samples}

        measure("pytest", [py, "-m", "pytest", "-q", "-p", "no:cacheprovider", "tests"], base)
        measure("pytest -n auto", [py, "-m", "pytest", "-q", "-p", "no:cacheprovider", "-n", "auto", "tests"], xenv)
        measure("tiderace", [TIDERACE, "run", "-q", tests], local)
        measure("pytest -k one", [py, "-m", "pytest", "-q", "-p", "no:cacheprovider", "-k", one, "tests"], base)
        measure("tiderace -k one, no daemon", [TIDERACE, "run", "-q", "-k", one, tests], local)
        subprocess.run([TIDERACE, "daemon", "start", tests], cwd=root, env=tenv, capture_output=True)
        timed([TIDERACE, "run", "-q", tests], root, tenv)  # the image, and the records
        measure("tiderace -k one, warm daemon", [TIDERACE, "run", "-q", "-k", one, tests], tenv)
        measure("tiderace, warm daemon", [TIDERACE, "run", "-q", tests], tenv)
        subprocess.run([TIDERACE, "daemon", "stop", tests], cwd=root, env=tenv, capture_output=True)
        print("   medians: " + "  ".join(f"{k} {v['median']:.2f}s" for k, v in rows.items()), flush=True)
        results[str(size)] = {"modules": modules, "rows": rows}
        json.dump(results, open(os.path.join(HERE, "scale.json"), "w"), indent=1)
        shutil.rmtree(root, ignore_errors=True)
    print(f"wrote {os.path.join(HERE, 'scale.json')}")


if __name__ == "__main__":
    main()
