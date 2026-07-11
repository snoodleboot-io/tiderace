#!/usr/bin/env bash
# Clone the conformance targets at their EXACT pinned SHAs (manifest.tsv) into ./vendor/ (gitignored).
# Reproducible: pins to the sha, not a moving tag. Re-runnable (skips repos already at the right sha).
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p vendor

grep -v '^#' manifest.tsv | while IFS=$'\t' read -r name url tag sha; do
  [ -z "${name:-}" ] && continue
  dir="vendor/$name"
  if [ -d "$dir/.git" ] && [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" = "$sha" ]; then
    echo "ok   $name @ $sha (cached)"
    continue
  fi
  rm -rf "$dir"
  git init -q "$dir"
  git -C "$dir" remote add origin "$url"
  # Fetch just the pinned commit (GitHub allows fetch-by-sha); falls back to the tag if disabled.
  if git -C "$dir" fetch -q --depth 1 origin "$sha" 2>/dev/null; then
    git -C "$dir" checkout -q FETCH_HEAD
  else
    git -C "$dir" fetch -q --depth 1 origin "refs/tags/$tag"
    git -C "$dir" checkout -q FETCH_HEAD
  fi
  got="$(git -C "$dir" rev-parse HEAD)"
  [ "$got" = "$sha" ] || { echo "FAIL $name: got $got, expected $sha" >&2; exit 1; }
  echo "got  $name @ $sha"
done

echo
echo "run:  python3 conformance.py vendor/click vendor/cachetools"
