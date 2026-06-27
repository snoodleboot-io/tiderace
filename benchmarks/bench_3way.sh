#!/usr/bin/env bash
# Three-way benchmark: pytest vs the OLD engine (legacy `tiderace`) vs the NATIVE engine
# (`riptide-daemon`), over the same corpus. Honest cold-full-run + warm scenarios.
#
# Usage:  benchmarks/bench_3way.sh [corpus-dir] [venv-python]
#   defaults: corpus = benchmarks/fixtures/fx_corpus, python = .riptide-fx-venv/bin/python
#
# Prereqs (built once):
#   cargo build --release --bin tiderace           # old engine (repo root)
#   cargo build --release -p engine-daemon         # native engine (engine/ workspace)
set -euo pipefail
R="$(cd "$(dirname "$0")/.." && pwd)"
CORPUS="${1:-$R/benchmarks/fixtures/fx_corpus}"
VENV="${2:-$R/.riptide-fx-venv/bin/python}"
TIDERACE="$R/target/release/tiderace"
RIPTIDE="$R/engine/target/release/riptide-daemon"
export RIPTIDE_PYTHON="$VENV" RIPTIDE_SHIM="$R/engine/py-shim/shim.py"

for b in "$TIDERACE" "$RIPTIDE"; do
  [ -x "$b" ] || { echo "missing $b — build it first (see header)"; exit 1; }
done
command -v hyperfine >/dev/null || { echo "needs hyperfine"; exit 1; }

cd "$CORPUS"   # all tools run with cwd = corpus (rootdir/conftest resolution must match)

echo "### Scenario 1 — COLD full run (everything executes; all four pass the same tests)"
hyperfine --warmup 1 --runs 8 \
  --prepare "true"                      -n "pytest"                 "$VENV -m pytest -q ." \
  --prepare "rm -f .tiderace.db"        -n "tiderace (old)"         "$TIDERACE . --all --python $VENV -n 0" \
  --prepare "rm -f .riptide-state.json" -n "native (fork)"          "$RIPTIDE run . --all" \
  --prepare "rm -f .riptide-state.json" -n "native --fast (nofork)" "$RIPTIDE run . --fast"

echo
echo "### Scenario 2 — WARM, no changes (re-run after a clean run; impact analysis skips all)"
rm -f .tiderace.db; "$TIDERACE" . --python "$VENV" >/dev/null 2>&1          # populate old-engine db
rm -f .riptide-state.json; "$RIPTIDE" run . >/dev/null 2>&1                  # populate native state
hyperfine --warmup 1 --runs 5 \
  -n "pytest (no warm mode)"        "$VENV -m pytest -q ." \
  -n "tiderace (old, impact-skip)"  "$TIDERACE . --python $VENV" \
  -n "native (impact-skip)"         "$RIPTIDE run ."

echo
echo "### Scenario 3 — INNER LOOP, warm rerun of ONE test (the daemon's pitch)"
D="$(mktemp -d)"; printf 'def test_one():\n    assert sum(range(100))==4950\n' > "$D/test_one.py"
echo "  native (warm, 1 test):"; "$RIPTIDE" bench "$D" 4 | sed 's/^/    /'
echo "  pytest (1 test, cold every time):"; ( cd "$D" && "$VENV" -m pytest -q . >/dev/null 2>&1; /usr/bin/time -f "    %e s wall" "$VENV" -m pytest -q . >/dev/null ) 2>&1 || true
rm -rf "$D"
