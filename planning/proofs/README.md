# Proof scripts (archived)

These scripts were the evidence behind ADRs and roadmap items as the engine was built: each one
drove the real shim (or the migration codemod) over a scratch corpus and printed a `VERDICT`. They
were never wired into CI — the acceptance suites under `engine/crates/*/tests/` are what gate a
change — so they are kept here as a record (TID-110), not as tests. They import the shim package as
it stood on the day they were archived (`from tiderace_shim import …` with `engine/py-shim` on
`sys.path`) and are not maintained against it.

| proof | what it demonstrated | covered now by |
| -- | -- | -- |
| `proof_b2_uses.py` | `@tiderace.uses(Type)` — native `usefixtures` | `parity_gaps_acceptance`, `param_ids_acceptance` |
| `proof_b3_inference.py` | migration type inference for untyped fixtures | the migration conformance numbers (`docs/guides/migration.md`); no test |
| `proof_b5_async_providers.py` | `async def @tiderace.provides` on the test's loop | `anyio_backend_acceptance`; `py-shim/tests/test_invoke.py` |
| `proof_b5_provider_params.py` | provider-level parametrization | `param_ids_acceptance`, `parametrize_reporting_acceptance` |
| `proof_migrate_async_and_assertions.py` | `migrate` on coroutines; never an unbound `pytest` | the migration conformance run; no test |
| `proof_migrate_novalue.py` | `migrate` on a value-less yield fixture (TID-8) | the migration conformance run; no test |
| `proof_n3_shim.py` | native providers resolved by type through the shim | `builtin_resources_acceptance` |
| `proof_n5_builtins.py`, `proof_n5b_caplog.py` | the builtin resources (`monkeypatch`, `tmp_path`, `capsys`, `capfd`, `caplog`) | `builtins_acceptance`, `request_fixture_acceptance` |
| `proof_n6_coverage.py` | per-test coverage capture (ADR-E006) — stale, NO-GO since before the redesign | `coverage_capture_acceptance`, `module_footprint_acceptance` |
| `proof_n7_assertions.py` | lazy assertion introspection / RichDiff (ADR-E009) | `differential`; `py-shim/tests/test_invoke.py` |
| `proof_n8_async_unittest.py` | async tests and `unittest` fidelity | `unittest_and_node_acceptance`, `unittest_skip_acceptance`, `unittest_class_setup_acceptance` |
| `proof_pure_batching.py` | pure tests batched in-process with identical outcomes | `runner_tier_acceptance`, `verdict_store_acceptance` |
| `proof_purity_guard.py` | the purity guard's verdicts | `verdict_store_acceptance`, `state_fingerprint_acceptance` |
| `proof_snapshot_restore.py` | snapshot/restore isolation for impure tests — stale, NO-GO since before the redesign | `nofork_acceptance`, `restore_identity_acceptance`, `sys_modules_restore_acceptance` |
| `proof_static_purity.py` | the static AST impurity pre-filter | nothing: the pre-filter had no caller on the production path and was removed with this archive |
| `proof_subinterp_probe.py` | sub-interpreter safety probe (ADR-E015) | `module_probe_acceptance`, `subinterp_acceptance` |
| `proof_subinterp_worker.py` | the `--subinterp` pool — stale, NO-GO since before the redesign | `subinterp_acceptance`, `subinterp_tier_acceptance`, `subinterp_cache_acceptance` |
| `proof_trusted_pure.py` | a recorded-pure test on the bare tier (TID-1) | `verdict_store_acceptance`, `runner_tier_acceptance` |
| `proof_type_di.py` | native type-driven authoring (ADR-E012) | `builtin_resources_acceptance`, `parity_gaps_acceptance` |
| `proof_windows_opaque_fork.py` | opaque modules on a fork-less platform | `nofork_acceptance` |
