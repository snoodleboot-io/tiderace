#!/usr/bin/env bash
# Build the tiderace wheel with maturin. Used identically by local dev and CI, so what CI ships is
# exactly what you can build and run here.
#
#   scripts/build-wheel.sh [maturin-args...]   # e.g. --release -o ../../../dist
#
#   1. Stage the canonical shim — the entry file engine/py-shim/shim.py and the package beside it,
#      engine/py-shim/tiderace_shim/ (TID-116) — into the Python package so it ships in the wheel
#      and the binaries auto-locate the entry (engine_core::default_shim). The staged copies are
#      git-ignored — engine/py-shim/ stays the single source of truth.
#   2. maturin build from the packaging crate (engine/crates/tiderace-dist), which owns both bins.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SHIM_SRC_DIR="$ROOT/engine/py-shim"
SHIM_DST_DIR="$ROOT/engine/py-tiderace/tiderace/_shim"
[ -f "$SHIM_SRC_DIR/shim.py" ] || { echo "error: canonical shim entry not found at $SHIM_SRC_DIR/shim.py" >&2; exit 1; }
[ -f "$SHIM_SRC_DIR/tiderace_shim/modes.py" ] || { echo "error: shim package not found at $SHIM_SRC_DIR/tiderace_shim" >&2; exit 1; }
mkdir -p "$SHIM_DST_DIR"
rm -rf "$SHIM_DST_DIR/tiderace_shim"
cp "$SHIM_SRC_DIR/shim.py" "$SHIM_DST_DIR/shim.py"
cp -r "$SHIM_SRC_DIR/tiderace_shim" "$SHIM_DST_DIR/tiderace_shim"
rm -rf "$SHIM_DST_DIR/tiderace_shim/__pycache__"
echo "staged shim -> tiderace/_shim/shim.py + tiderace/_shim/tiderace_shim/"

# Build via -m (not `cd`), so any relative `-o <dir>` the caller passes stays relative to THEIR cwd,
# not the crate dir. maturin resolves python-source / include globs relative to the manifest either
# way. (A `cd` here silently misplaced the wheel under the crate dir — caught by the CI smoke test.)
exec maturin build -m "$ROOT/engine/crates/tiderace-dist/Cargo.toml" "$@"
