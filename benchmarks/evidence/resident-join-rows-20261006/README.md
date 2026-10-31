# Resident row-join foundation — correctness evidence

This is a native execution milestone, **not SQL planner support or a performance
claim**. Inner, left, semi and anti integer equijoins now produce resident row
pairs, rather than only a match count. Up to four INT4/INT8 key pairs preserve
NULLs, duplicate multiplicity and read-only self aliases. No CPU join or
row-proportional intermediate copyback is used. Capacity exhaustion returns the
required cardinality without partial output.

The Rust facade borrows typed inputs, reserves workspace and output bytes before
allocation, releases temporary charges on every result path, and retains the
output charge until its owner drops. Direct PostgreSQL tests compare 40 cases
(five input pairings, one/two keys, four kinds) with native SQL and check output
capacity, initialized-prefix access and ledger cleanup. C++ cases additionally
cover three/four keys, signed extremes, hot-bucket duplicates, prefix boundaries
through 65,537 probe rows, workspace reuse and malformed/aliased contracts.

## Validation

- Full native CTest: **34/34 passed** on Apple M2 Max / Metal.
- Standalone PG18 Rust tests: **1,213 passed**.
- Direct PostgreSQL oracle/ownership test: **passed on PG18.4 and PG19beta1**.
- Strict Clippy, all targets with pg_test: **passed on both majors**.
- Coverage tooling: **104 tests passed**; scope audit includes the new source
  and CTest (223 Rust files, 17 C++ sources, 34 CTests). This is an inventory
  check, not a fresh line-coverage measurement.
- Documentation parity: **21 tests passed**.
- CPU-work checker regression tests: **173 passed**; the actual production
  audit remains failing as described below, not waived by those checker tests.

Core commands (each build/test shell used `ulimit -n 4096`):

```sh
just gpu-build
ctest --test-dir pgaccel-kernels/build --output-on-failure --timeout 300
just test-standalone 18
cargo pgrx test --package pg_accel pg18 pg_resident_join_rows_native_parity_and_ownership
cargo pgrx test --package pg_accel pg19 pg_resident_join_rows_native_parity_and_ownership
# Run for each major, using that major's PG_CONFIG and PGRX_PG_CONFIG_PATH:
cargo clippy -p pg_accel --no-default-features --features pg18,pg_test --all-targets -- -D warnings
cargo clippy -p pg_accel --no-default-features --features pg19,pg_test --all-targets -- -D warnings
python3 -m unittest scripts.tests.test_coverage_tools
python3 -m unittest scripts.tests.test_cpu_cheat_audit
python3 -m unittest scripts.tests.test_doc_parity
python3 scripts/coverage_tools.py audit-scope --scope coverage/scope.json --repo-root .
python3 scripts/cpu_cheat_audit.py pgaccel-kernels/src/join_rows.cpp
just install-pg-accel 19
just install-pg-accel 18
```

Commands, output and original failure logs are retained alongside this file.
The initial C++ test run failed because its copyback arguments were reversed,
its zero-capacity pointer was noncanonical, and one type-mismatch mutation did
not change the type. Correcting those tests produced a passing run; expanded
tests and the full suite passed subsequently. Early Rust compile/lint iterations
are retained too. The native contract was not relaxed to accept those fixtures.

The conservative CPU-work audit still reports **two new export findings** for
metadata/control-flow proof. Its gate was not bypassed or given a name-specific
exception. The ABI inventory adds exactly two signatures, with an updated
literal integrity anchor. These findings remain a release blocker, alongside
the existing broader release work.

## Boundary still open

There is no CustomPath, pipeline wire/occurrence-identity migration, downstream
device consumer or normal SQL selection in this change. v1 inputs are limited
to INT32_MAX rows; output indices/counts/offsets and forecasts use 64 bits.
Skew-aware partitioning, bounded-work cancellation/admission, concurrent
workloads, larger-than-budget execution, CUDA validation and performance
qualification remain open. The hot-bucket correctness fixture is not evidence
of scalable skew performance.

Recorded on 2026-10-06 at base commit
`5c7fbd0a6381f823befd823ec47137a8a602e94d` with the existing dirty worktree
preserved. The source snapshot identifies the actual implementation, not that
base commit alone. Normal non-pg_test builds are restored after the oracle runs.
The oracle runs preceded the final defensive workspace-alignment check and
additional native hot-bucket/pre-initialization cases. The final source passed
standalone, full-native and both lint builds; the installed normal module
hashes are recorded in the install logs. SHA256SUMS binds these retained files.
