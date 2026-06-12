# Installation

## Prerequisites

- Python 3.8+ with `pip`
- `pytest` and `coverage` Python packages
- Linux x86_64
- [Rust toolchain](https://rustup.rs) 1.75+ (to build from source)

> **Pre-release:** riptide is not yet published to crates.io or GitHub Releases. Build from source with `cargo build --release` (binary at `target/release/riptide`). The download URLs below are placeholders for a future release.

## From Source (Recommended)

This is the working path today. With the [Rust toolchain](https://rustup.rs) 1.75+ installed:

```bash
cargo build --release

# binary at target/release/riptide; copy it onto your PATH, e.g.
install -m 0755 target/release/riptide /usr/local/bin/riptide

# Verify
riptide --version
```

## Binary (Future / Illustrative)

Once releases are published, you will be able to download a pre-built binary. The commands below are placeholders for that future release:

```bash
# Linux x86_64 (placeholder URL — not yet available)
curl -sSfL https://github.com/snoodleboot-io/riptide/releases/latest/download/riptide-linux-x86_64 \
  -o /usr/local/bin/riptide && chmod +x /usr/local/bin/riptide
```

## Cargo Install (Future / Illustrative)

Once published to crates.io, this will also work (not yet available):

```bash
cargo install riptide
```

## Python Dependencies

riptide shells out to `pytest` and optionally `coverage`. Install them in your project environment:

```bash
pip install pytest coverage
# or
uv add --dev pytest coverage
```

## CI / GitHub Actions

The [CI workflow](https://github.com/snoodleboot-io/riptide/blob/main/.github/workflows/ci.yml) builds and tests automatically. Until prebuilt binaries are published, build from source in CI:

```yaml
- name: Build riptide
  run: |
    cargo build --release
    install -m 0755 target/release/riptide /usr/local/bin/riptide

- name: Run tests
  run: riptide tests/ --coverage -n 4
```

## Add to .gitignore

```gitignore
# riptide state — machine-local, do not commit
.riptide.db
.riptide-coverage/
```
