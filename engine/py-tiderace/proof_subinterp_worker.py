"""Sub-interpreter worker proof (ADR-E015 Phase 2 / TID-10). Drives the shim's `--subinterp` mode: a
batch of safe tests runs across a pool of isolated sub-interpreters, parallel via per-interpreter GILs
(PEP 684). Asserts correct outcomes; demonstrates the pool speedup on CPU-bound tests. Real shim.

Run:  python3 proof_subinterp_worker.py     (needs CPython 3.14+)
"""
from __future__ import annotations

import json
import os
import struct
import subprocess
import sys
import tempfile
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
_SHIM = os.path.join(_HERE, os.pardir, "py-shim", "shim.py")

# 5 outcome/style cases + 6 CPU-bound pure tests (to show parallelism).
CORPUS = """\
def test_pass():
    assert 1 + 1 == 2
def test_fail():
    assert 1 == 2
def test_error():
    raise RuntimeError("boom")
class TestG:
    def test_mpass(self):
        assert "x".upper() == "X"
    def test_mfail(self):
        assert []
""" + "".join(
    f"def test_cpu{i}():\n    s = 0\n    for k in range(3_000_000): s += k * k\n    assert s > 0\n"
    for i in range(6)
)


def _rd(f):
    n = f.read(4)
    return json.loads(f.read(struct.unpack("<I", n)[0])) if len(n) == 4 else None


def _wr(f, o):
    b = json.dumps(o).encode()
    f.write(struct.pack("<I", len(b)))
    f.write(b)
    f.flush()


def _run(root, batch, workers):
    p = subprocess.Popen(
        [sys.executable, _SHIM, root, "--subinterp"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        env={**os.environ, "TIDERACE_SUBINTERP_WORKERS": str(workers)},
    )
    _rd(p.stdout)  # ready
    t0 = time.perf_counter()
    _wr(p.stdin, {"batch": batch})
    resp = _rd(p.stdout)
    dur = time.perf_counter() - t0
    p.stdin.close()
    p.wait()
    return {r["node_id"]: r["outcome"] for r in resp["results"]}, dur


def main() -> int:
    print("=== sub-interpreter worker proof (ADR-E015 Phase 2) ===\n")
    try:
        import concurrent.interpreters  # noqa: F401
    except Exception:
        print("=== SKIP: concurrent.interpreters unavailable — needs CPython 3.14+ ===")
        return 0

    with tempfile.TemporaryDirectory() as root:
        open(os.path.join(root, "test_si.py"), "w").write(CORPUS)
        nodes = [("test_si.py::test_pass", "function"), ("test_si.py::test_fail", "function"),
                 ("test_si.py::test_error", "function"), ("test_si.py::TestG::test_mpass", "class_method"),
                 ("test_si.py::TestG::test_mfail", "class_method")]
        nodes += [(f"test_si.py::test_cpu{i}", "function") for i in range(6)]
        batch = [{"node_id": n, "style": s, "deadline_ms": 5000} for n, s in nodes]

        outcomes, _ = _run(root, batch, workers=4)
        by_leaf = {n.rsplit("::", 1)[1]: outcomes[n] for n, _ in nodes}
        want = {"test_pass": "passed", "test_fail": "failed", "test_error": "error",
                "test_mpass": "passed", "test_mfail": "failed"}
        checks = [(f"{k} → {v}", by_leaf.get(k) == v) for k, v in want.items()]
        checks.append(("all 6 cpu tests passed", all(by_leaf[f"test_cpu{i}"] == "passed" for i in range(6))))

        # Parallelism: same batch, 1 worker vs 4.
        _, seq = _run(root, batch, workers=1)
        _, par = _run(root, batch, workers=4)

        ok = all(good for _, good in checks)
        for label, good in checks:
            print(f"    {'ok' if good else '!!':<3} {label}")
        print(f"\n    pool speedup (11 tests, 6 CPU-bound): 1 worker {seq*1000:.0f} ms → 4 workers "
              f"{par*1000:.0f} ms  ({seq/max(par,1e-6):.1f}×)")
        print(f"\n=== VERDICT: {'GO — safe tests run correctly across a parallel sub-interpreter pool' if ok else 'NO-GO'} ===")
        return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
