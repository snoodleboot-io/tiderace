#!/usr/bin/env bash
# Benchmark: pytest vs the tiderace engine (`riptide-daemon`), over the same corpus. Honest
# cold-full-run + warm scenarios. The engine runs no-fork+restore by default; RIPTIDE_FORCE_FORK=1
# gives the fork-per-test baseline (a debug knob, not a user flag).
#
# Usage:  benchmarks/bench_3way.sh [corpus-dir] [venv-python]
#   defaults: corpus = benchmarks/fixtures/fx_corpus, python = .riptide-fx-venv/bin/python
#
# Prereqs (built once):
#   cd engine && cargo build --release -p engine-daemon
set -euo pipefail
R="$(cd "$(dirname "$0")/.." && pwd)"
CORPUS="${1:-$R/benchmarks/fixtures/fx_corpus}"
VENV="${2:-$R/.riptide-fx-venv/bin/python}"
RIPTIDE="$R/engine/target/release/riptide-daemon"
export RIPTIDE_PYTHON="$VENV" RIPTIDE_SHIM="$R/engine/py-shim/shim.py"

[ -x "$RIPTIDE" ] || { echo "missing $RIPTIDE — build it first (see header)"; exit 1; }
command -v hyperfine >/dev/null || { echo "needs hyperfine"; exit 1; }

cd "$CORPUS"   # all tools run with cwd = corpus (rootdir/conftest resolution must match)

echo "### Scenario 1 — COLD full run (everything executes; all three pass the same tests)"
# native runs no-fork+restore BY DEFAULT (no flag); RIPTIDE_FORCE_FORK=1 is a debug knob for the baseline.
hyperfine --warmup 1 --runs 8 \
  --prepare "true"                      -n "pytest"                   "$VENV -m pytest -q ." \
  --prepare "rm -f .riptide-state.json" -n "tiderace (default,no-fork)" "$RIPTIDE run . --all" \
  --prepare "rm -f .riptide-state.json" -n "tiderace (force-fork)"      "RIPTIDE_FORCE_FORK=1 $RIPTIDE run . --all"

echo
echo "### Scenario 2 — WARM, no changes (re-run after a clean run; impact analysis skips all)"
rm -f .riptide-state.json; "$RIPTIDE" run . >/dev/null 2>&1                  # populate native state
hyperfine --warmup 1 --runs 5 \
  -n "pytest (no warm mode)"    "$VENV -m pytest -q ." \
  -n "tiderace (impact-skip)"   "$RIPTIDE run ."

echo
echo "### Scenario 3 — INNER LOOP, warm rerun of ONE test (the daemon's pitch)"
D="$(mktemp -d)"; printf 'def test_one():\n    assert sum(range(100))==4950\n' > "$D/test_one.py"
echo "  tiderace (warm, 1 test):"; "$RIPTIDE" bench "$D" 4 | sed 's/^/    /'
echo "  pytest (1 test, cold every time):"; ( cd "$D" && "$VENV" -m pytest -q . >/dev/null 2>&1; /usr/bin/time -f "    %e s wall" "$VENV" -m pytest -q . >/dev/null ) 2>&1 || true
rm -rf "$D"
