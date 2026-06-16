#!/usr/bin/env bash
# Phase-1 spike verification: correctness (C1–C3) + benchmark (C6). Reproducible, pipeline-owned.
set -uo pipefail
cd "$(dirname "$0")"
ROOT="$(cd .. && pwd)"
export SPIKE_PYTHON="$ROOT/.riptide-spike-venv/bin/python"
export SPIKE_SHIM="$PWD/shim.py"
BIN=./target/release/spike
CORPUS=corpus

DIFF=(
  pytest_func:test_basic::test_addition_passes
  pytest_func:test_basic::test_numpy_sum_passes
  pytest_func:test_basic::test_subtraction_fails
  unittest_method:test_unit_case::ArithmeticCase::test_mul_passes
  unittest_method:test_unit_case::ArithmeticCase::test_div_fails
)

echo "############ C1 — differential vs pytest (outcomes must match) ############"
"$BIN" warm "$CORPUS" "${DIFF[@]}" | sort > /tmp/engine.txt
"$SPIKE_PYTHON" -m pytest -rA -q "$CORPUS/test_basic.py" "$CORPUS/test_unit_case.py" 2>/dev/null \
  | grep -E '^(PASSED|FAILED)' \
  | awk '{s=tolower($1); id=$2; sub(/^corpus\//,"",id); sub(/\.py::/,"::",id); print id"\t"s}' \
  | sort > /tmp/pytest.txt
echo "-- engine --";  cat /tmp/engine.txt
echo "-- pytest --";  cat /tmp/pytest.txt
if diff -q /tmp/engine.txt /tmp/pytest.txt >/dev/null; then echo "C1: PASS (engine == pytest)"; else echo "C1: FAIL"; diff /tmp/engine.txt /tmp/pytest.txt; fi

echo; echo "############ C2 — fork isolation (engine passes BOTH; pytest fails the 2nd) ############"
echo "-- engine (expect both passed) --"
"$BIN" warm "$CORPUS" \
  pytest_func:isolation/test_isolation::test_a_mutates_global \
  pytest_func:isolation/test_isolation::test_b_sees_pristine_state
echo "-- pytest one-process (expect test_b FAILED — the divergence is the isolation win) --"
"$SPIKE_PYTHON" -m pytest -q "$CORPUS/isolation/test_isolation.py" 2>/dev/null | tail -2

echo; echo "############ C3 — fault handling (crash + timeout -> error; Wellspring survives) ############"
"$BIN" warm "$CORPUS" \
  pytest_func:test_faults::test_hard_crash \
  pytest_func:test_faults::test_hang_times_out \
  pytest_func:test_basic::test_addition_passes
echo "(addition_passes after a crash AND a timeout proves the Wellspring survived)"

echo; echo "############ C6a — benchmark: small 5-test suite (warm vs fresh vs pytest) ############"
hyperfine -N --warmup 1 -r 5 \
  -n "warm (fork-from-warm)"  "$BIN warm $CORPUS ${DIFF[*]}" \
  -n "fresh (process/test)"   "$BIN fresh $CORPUS ${DIFF[*]}" \
  -n "pytest"                 "$SPIKE_PYTHON -m pytest -q $CORPUS/test_basic.py $CORPUS/test_unit_case.py" \
  2>/dev/null

echo; echo "############ C6b — scale: 50 executions, warm vs fresh (import amortization) ############"
SCALE=()
for _ in $(seq 1 10); do
  SCALE+=(pytest_func:test_basic::test_addition_passes pytest_func:test_basic::test_numpy_sum_passes \
          pytest_func:test_basic::test_subtraction_fails \
          unittest_method:test_unit_case::ArithmeticCase::test_mul_passes \
          unittest_method:test_unit_case::ArithmeticCase::test_div_fails)
done
hyperfine -N --warmup 1 -r 3 \
  -n "warm x50 (1 import + 50 forks)"  "$BIN warm $CORPUS ${SCALE[*]}" \
  -n "fresh x50 (50 imports)"          "$BIN fresh $CORPUS ${SCALE[*]}" \
  2>/dev/null
