#!/usr/bin/env bash
# Build the tiderace wheel with maturin. Used identically by local dev and CI, so what CI ships is
# exactly what you can build and run here.
#
#   scripts/build-wheel.sh [maturin-args...]   # e.g. --release -o ../../../dist
#
#   1. Stage the canonical shim (engine/py-shim/shim.py) into the Python package so it ships in the
#      wheel and the binaries auto-locate it (engine_core::default_shim). The staged copy is
#      git-ignored — engine/py-shim/shim.py stays the single source of truth.
#   2. maturin build from the packaging crate (engine/crates/tiderace-dist), which owns both bins.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

SHIM_SRC="$ROOT/engine/py-shim/shim.py"
SHIM_DST_DIR="$ROOT/engine/py-tiderace/tiderace/_shim"
[ -f "$SHIM_SRC" ] || { echo "error: canonical shim not found at $SHIM_SRC" >&2; exit 1; }
mkdir -p "$SHIM_DST_DIR"
cp "$SHIM_SRC" "$SHIM_DST_DIR/shim.py"
echo "staged shim -> tiderace/_shim/shim.py"

# Build via -m (not `cd`), so any relative `-o <dir>` the caller passes stays relative to THEIR cwd,
# not the crate dir. maturin resolves python-source / include globs relative to the manifest either
# way. (A `cd` here silently misplaced the wheel under the crate dir — caught by the CI smoke test.)
exec maturin build -m "$ROOT/engine/crates/tiderace-dist/Cargo.toml" "$@"
