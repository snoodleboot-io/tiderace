# tiderace

A pure-Rust test engine for Python — its own runner, not a pytest wrapper. Isolation without the fork
tax, and only runs what changed.

`pip install tiderace` ships the `tiderace` and `tiderace-daemon` binaries plus the native authoring
package. Docs: https://snoodleboot-io.github.io/tiderace/

Requires Python 3.12+. Prebuilt wheels: Linux x86_64 / aarch64 (glibc 2.28+), macOS arm64 (11.0+) /
x86_64 (10.13+), Windows x86_64.
