# Uniform grouped execution budget: live qualification

On 2026-10-12, the exact live fixture
`pg_dense_integer_execution_budget_and_bounded_pool_ownership` passed on
PostgreSQL 18.4 and 19beta1 with Apple M2 Max/Metal: **1/1 each, zero failures,
zero ignored tests**. The matrix process 44281 reached terminal exit 0; both
individual commands returned 0. PG18 fixture duration was 27.18s and PG19
23.33s, including pgrx installation/setup, not a performance measurement.

The fixture proves real dense integer device workspace/output allocation,
combined one-byte-short budget denial before allocation, cold/warm pool charge
ownership and reuse, exact zero-row native finalization, cleanup to baseline,
and zero-budget rejection. A missing GPU fails the fixture. It uses a
process-local pg_test execution ledger: it is not cross-backend shared-ledger
qualification. The earlier `h3_count_tests` 10/10 runs did not include this test.

`run.sh` preserves exact commands and SDK environment (`CFLAGS` and bindgen
isysroot workaround), `ulimit -n 4096`, serial test execution, and isolated
copied PostgreSQL prefixes. `pg{18,19}-config.log` proves relocated install
directories; `installed-module-sha256.log` identifies installed pg_test modules.
Production release modules and original PG17 installations/servers were not
overwritten. Disposable pgrx clusters were cleaned up on completion. Filtered
unrelated tests are not claimed as executed.

Source identity: 543/544 hashes match the sealed full-native bundle,
as recorded in `source-identity-verify.log`; the sole difference is the
independently updated Python proof checker `scripts/workspace_calls.py`, which
does not participate in these Rust/native fixture builds. All Rust and native
sources, including the tested fixture, match. The earlier snapshot is copied
here for those compiled sources, not as an exact snapshot of every current
repository script.
The current Cargo.toml is supplied separately as `Cargo.toml.current`; overlay
it when reproducing, because the later release-only `strip = "none"` toolchain
workaround was not present in that earlier snapshot. These fixture runs use
the unoptimized pg_test/dev profile, not a substitute for production SQL tests.

Complementary native qualification is sealed separately in
`../native-live-20261012`: 36/36 tests, including grouped aggregation. This
bundle does not rerun or expand that native result, prove new planner admission,
waive the seven static proof obligations, establish CUDA support, or qualify
cold-start latency. The unrelated retained PG19 full-SQL timeout warning is
not repaired or erased by this fixture passing.

`SHA256SUMS` seals this bundle after all fixture processes completed.
