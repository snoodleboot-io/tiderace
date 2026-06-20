# ADR-E011 ② spike — RESULTS: **GO**

> Throwaway go/no-go spike for the in-process / FFI execution backend (ADR-E011 ②).
> engine-core untouched. Disposable crate + disposable venv.

## Thesis

ADR-010 rejected **N PEP-684 subinterpreters per process** — single-phase-init C extensions
(`_decimal`, numpy, pandas, …) corrupt/segfault when a *second* interpreter re-inits them. ② is a
**different** shape: **one** main interpreter, embedded via PyO3, with the Rust↔Python control plane as
**FFI calls returning native values** instead of JSON frames over pipes. The spike asks one question:
*does one embedded interpreter, hosted by Rust, run pytest AND the C-ext that crashed subinterpreters?*

## How to reproduce

```bash
# A CPython that ships libpython + headers (uv standalone 3.11.15 here); a venv on it for pytest.
BASE=.../uv/python/cpython-3.11.15-linux-x86_64-gnu
"$BASE/bin/python3" -m venv /tmp/inproc-spike-venv && /tmp/inproc-spike-venv/bin/pip install pytest

cd spike-inproc
RUSTFLAGS="-L native=$BASE/lib" PYO3_PYTHON=/tmp/inproc-spike-venv/bin/python cargo build
LD_LIBRARY_PATH="$BASE/lib" \
  PYTHONPATH=/tmp/inproc-spike-venv/lib/python3.11/site-packages \
  ./target/debug/inproc-spike
```

## Result (exit 0)

```
[0] embedded interpreter: cpython 3.11.15   (single MAIN interpreter — not a subinterpreter)
[A] _decimal C-ext in-process (ADR-010's segfault module):
    Decimal(1.1)+Decimal(2.2) = 3.3
    5000-term Decimal reduction → 29-digit result, NO crash ✓
[B1] in-process unittest → Rust values: ran=3 failed=1 errored=0
[B2] in-process pytest.main(["-q", ...]) → exit code 0 (0 = all passed)
=== VERDICT: GO ===
```

## What this proves

- **PyO3 embeds one CPython and Rust drives it by FFI** — `pytest.main(...)` and a `unittest` suite run
  *as function calls*, and their verdicts come back as **Rust values** (ints), not bytes over a pipe.
  This is exactly the `InProcessTransport::exchange` shape ② plugs behind the `ShimTransport` seam.
- **ADR-010's failure mode does not occur with one interpreter.** `_decimal` — the precise module that
  produced `mpd_setminalloc ... a second time → munmap_chunk(): invalid pointer → core dump` under
  subinterpreters — imports and runs heavy arithmetic with no crash. The single-phase-init hazard is a
  *multi-interpreter* hazard; one interpreter is the same world every pytest plugin already runs in.
- **The ADR-010 "missing headers" wall was incidental.** A uv-managed standalone CPython ships
  `libpython3.11.so` + `Python.h` + `python3.11-config`; no sudo, no system `python3-dev`.

## What this does NOT prove (open questions for the real ② phase)

1. **Per-test isolation.** The spike runs tests in one shared interpreter — fine for a feasibility
   check, but the product needs isolation between tests. In ②, isolation must still come from
   **`fork()` of the embedded interpreter** (retained; the watermark/wellspring model already does this),
   *not* from subinterpreters (rejected). "Embed + FFI" replaces the **pipe/JSON control plane**, not the
   fork-based isolation. This is the central design question the phase must answer.
2. **Broader C-ext smoke test.** `_decimal` is a faithful representative, but a real ② should also boot
   numpy / pandas / pydantic-core in-process before declaring ecosystem parity.
3. **`PyConfig.home` plumbing.** The cosmetic `Could not find platform dependent libraries` /
   `sys.executable=/usr/bin/python3` warnings come from feeding `PYTHONPATH`/`LIBDIR` by hand against a
   relocated standalone; a real backend configures `PyConfig` (home, program name, venv prefix) properly.
4. **Performance.** Not measured here. The win ② targets is eliminating per-test pipe/exec syscalls; the
   actual speedup must be benchmarked against the subprocess `PipeTransport` baseline.
5. **engine-core integration.** This is a standalone crate; wiring `InProcessTransport: ShimTransport`
   into engine-core (and deciding whether engine-core takes a PyO3 dependency, or only a separate
   `engine-embed` crate does) is the build work, not proven here.

## Recommendation

Technical risk for ② is **retired** — the thing ADR-010 made everyone fear (C-ext crash) is genuinely a
subinterpreter-only problem. Proceed to **ratify ADR-E011 ②** and scope a phase whose headline design
question is **isolation under an embedded interpreter** (fork-from-embedded vs. per-test module reset),
not "can we embed at all" (answered: yes).
