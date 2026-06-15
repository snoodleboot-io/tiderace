#!/usr/bin/env bash
# Real-world benchmark: tiderace vs pytest on common OSS libraries' own suites.
#
# Clones a few pure-Python libraries, installs them into a throwaway venv, and
# times pytest vs tiderace (cold full run, warm no-change run, and warm run after
# editing one source file). Honest framing: tiderace's win is the WARM/impact
# loop; a cold full run is comparable-to-slower (subprocess-per-worker startup).
#
# Usage:
#   benchmarks/real_world.sh                 # build release, set up venv, run
#   TIDERACE=/path/to/tiderace PY=/path/to/python benchmarks/real_world.sh
#
# Requires: git, cargo, and `uv` (or adapt the venv/pip lines).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${WORK:-/tmp/tiderace-real-world}"
VENV="${VENV:-$WORK/.venv}"
TIDERACE="${TIDERACE:-$ROOT/target/release/tiderace}"

# library | git url | test path (relative to repo root)
LIBS=(
  "cachetools|https://github.com/tkem/cachetools|tests"
  "jmespath|https://github.com/jmespath/jmespath.py|tests"
  "toolz|https://github.com/pytoolz/toolz|toolz/tests"
  "inflection|https://github.com/jpvanhal/inflection|."
)

echo "==> building tiderace (release)"
( cd "$ROOT" && cargo build --release --quiet )

echo "==> setting up venv at $VENV"
mkdir -p "$WORK"
uv venv "$VENV" --python 3.12 >/dev/null 2>&1 || python3 -m venv "$VENV"
PY="${PY:-$VENV/bin/python}"
uv pip install --python "$PY" --quiet pytest coverage >/dev/null 2>&1 || "$PY" -m pip install -q pytest coverage

# elapsed seconds of a command
t() { /usr/bin/time -f "%e" "$@" >/dev/null 2>/tmp/_tw; tail -1 /tmp/_tw; }

printf "\n%-12s %8s %9s %10s %10s\n" "library" "pytest" "rt-cold" "rt-warm" "rt-warm1"
printf "%-12s %8s %9s %10s %10s\n" "-------" "------" "-------" "-------" "--------"
for entry in "${LIBS[@]}"; do
  IFS='|' read -r name url path <<<"$entry"
  dir="$WORK/$name"
  [ -d "$dir" ] || git clone --depth 1 "$url" "$dir" >/dev/null 2>&1
  ( cd "$dir" && uv pip install --python "$PY" --quiet -e . >/dev/null 2>&1 || true )
  cd "$dir"
  "$TIDERACE" clear >/dev/null 2>&1; rm -rf .tiderace-coverage

  pt=$(t "$PY" -m pytest "$path" -q)
  "$TIDERACE" clear >/dev/null 2>&1
  cold=$(t "$TIDERACE" --all --python "$PY" "$path")
  "$TIDERACE" clear >/dev/null 2>&1; rm -rf .tiderace-coverage
  "$TIDERACE" --coverage --python "$PY" "$path" >/dev/null 2>&1
  warm=$(t "$TIDERACE" --python "$PY" "$path")
  src=$(find . -name '*.py' -not -path '*/.git/*' -not -path '*test*' \
        -not -name 'setup.py' -not -name 'conftest.py' | head -1)
  echo "# tiderace-bench" >> "$src"
  warm1=$(t "$TIDERACE" --python "$PY" "$path")
  "$TIDERACE" clear >/dev/null 2>&1

  printf "%-12s %7ss %8ss %9ss %9ss\n" "$name" "$pt" "$cold" "$warm" "$warm1"
done

echo
echo "rt-warm = no-change re-run (skips all); rt-warm1 = after editing one source file."
echo "tiderace wins the warm/impact loop; pytest/xdist win a cold full run."
