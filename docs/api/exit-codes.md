# Exit Codes

Both binaries return process exit codes via Rust's `ExitCode`. The values below are exactly what
`engine-cli/src/main.rs` and `engine-daemon/src/main.rs` return.

| Code | Meaning | Returned by |
|---|---|---|
| `0` | Success — all tests passed (or nothing needed to run / a non-`run` mode completed cleanly) | `collect`; `run` with no failures; `serve`/`watch`/`bench` clean exit |
| `1` | Test failure **or** runtime error — one or more tests failed/errored, or an internal error (collection failure, wellspring launch failure, missing `TIDERACE_SHIM`, watch/serve I/O error) | both binaries |
| `64` | Usage error — wrong/missing arguments, unknown command/mode, or `serve` on a non-Unix platform | both binaries |

Notes:

- **Test failure and internal error share code `1`.** Neither binary distinguishes a failing test
  from an engine error in the exit code (`run` returns `ExitCode::FAILURE` for both); the cause is on
  stderr. The `tiderace run` pytest-style code comes from `RunReport::exit_code()` (`0` unless any
  outcome is a failure).
- **`64`** is the conventional `EX_USAGE` value, returned for argument/usage problems (e.g.
  `tiderace` with fewer than two args, an unknown `tiderace-daemon` mode) and for `tiderace-daemon serve`
  on a platform without Unix sockets.
- A **missing `TIDERACE_SHIM`** is treated as a runtime error and exits `1` (not `64`).

CI systems should treat any non-zero code as a failed build; `1` specifically marks failing tests.
