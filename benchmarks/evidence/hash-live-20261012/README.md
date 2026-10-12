# Staged hash aggregation live qualification — 2026-10-12

The complete staged hash qualification module now passes on PostgreSQL 18.4 and
19beta1 with a real Apple M2 Max GPU through Metal (38 compute units, out-of-order
queue available; no native FP64 or 64-bit atomics). Device availability is proved
by the repository's `test_device` executable, not inferred from the host model.

This is staged execution evidence, not new normal planner admission or a speedup
claim. The ten live fixtures on each major cover independent and identical direct
Boolean WHERE/FILTER, nullable typed COUNT, integer SUM/AVG/MIN/MAX rollups,
direct INT2/INT4/INT8 keys and H3 parents, high-cardinality output sorting,
budget/capacity learning, predicate-only mutation, rollback, empty input, and
interrupt cleanup/retry. The WHERE fixture includes the redundant FILTER
elimination: identical validated predicates use one mask/conversion while
retaining dependencies and exact COUNT(*) partition validation.

## Results

| Check | Result | Log |
| --- | --- | --- |
| Device initialization and lifecycle | Pass, real Metal GPU | `device.log` |
| General native grouped suite | Pass, 103.26 seconds | `native-grouped.log`, `native-grouped-raw-initial.log` |
| Native hash suite after fixture correction | 10,309,427 checks pass; zero failures | `native-hash-fixed.log` |
| Native hash CTest wrapper after correction | Pass, 7.65 seconds | `native-hash-fixed-ctest.log`, `native-hash-fixed-raw.log` |
| PG18 standalone host suite | 1,311 pass, zero failed/ignored | `core18.log` |
| PG19 standalone host suite | 1,311 pass, zero failed/ignored | `core19.log` |
| PG18/19 strict production workspace Clippy | Pass both | `lint18.log`, `lint19.log` |
| PG18/19 strict pg_test Clippy after fixture correction | Pass both | `live18-compile-fixed-sdk.log`, `live19-compile-fixed-sdk.log` |
| PG18 focused `where_` run | 17 pass, including both new live WHERE fixtures | `where-live18-sdk-cflags.log` |
| PG18 complete `h3_count_tests` module after correction | 10 live fixtures pass | `hash-live18-fixed.log` |
| PG19 complete `h3_count_tests` module after correction | 10 live fixtures pass | `hash-live19.log` |

The focused run mixes host tests and seven PostgreSQL tests; it is not 17 new
live hash fixtures. The separate integration-test executable reports zero
matching tests and is not counted as coverage. Host suites include preexisting
device-guarded cases; their totals are not substitutes for the live module.

## Retained failures and corrections

The first native hash run passed 10,309,037 checks but failed twice in the
zero-row WHERE fixture. Its descriptor specified nonnullable output for empty
input while its output constructor allocated a NULL sidecar. Correcting the
fixture to match its descriptor, plus diagnostic assertions, yielded the final
passing count above. Production native code was not changed by that fix.
`native-hash-raw-initial.log` and the diagnostic/build logs retain the failure.

The first complete PG18 module passed nine fixtures and failed the nullable
COUNT fixture with PostgreSQL `smallint out of range`. SQL `-32768::smallint`
casts positive 32768 before negation. The corrected fixture casts the complete
parenthesized expression in both INSERT and UPDATE and explicitly checks all
signed integer minima before loading. No production binding change was required.
The failing `hash-live18.log` is retained alongside both passing final modules.

Fresh pgrx builds also exposed the installed PostgreSQL build's obsolete
MacOSX26.5.sdk path. The first attempt failed bindgen; a bindgen-only SDK override
then reached a C-shim compile failure. Both `BINDGEN_EXTRA_CLANG_ARGS` and
`CFLAGS` were set to `-isysroot $(xcrun --show-sdk-path)` for successful fresh
builds. Initial Clippy used cached artifacts and passed; later fresh Clippy
without the overrides reproduced the SDK failure, then passed with both
overrides. All these environment-failure logs are retained. No system SDK or
repository production source was changed to work around this environment issue.

## Reproduction and isolation

Every build/test shell used `ulimit -n 4096`. Host and production lint commands
were `just test-standalone 18`, `just test-standalone 19`, `just lint 18`, and
`just lint 19`. Strict pg_test lint used each matching `PG_CONFIG` and
`PGRX_PG_CONFIG_PATH` with:

```sh
cargo clippy -p pg_accel --no-default-features \
  --features pg18,pg_test --all-targets -- -D warnings
```

The PG19 command substitutes `pg19`. Native tests were rebuilt from current
source and run with the CTest selector `^test_grouped_agg(_hash)?$`; the corrected
hash test was rerun directly and through its registered CTest wrapper.

PostgreSQL installations were copied into
`.codex/scratch/hash-implied-filter-20261012/postgres/{18.4,19beta1}`. The cloned
real `pg_config` executables were checked to report relocated bindir, sharedir
and pkglibdir before pgrx installed anything. An isolated `PGRX_HOME` config
selected those copies; disposable pgdata directories remained under the same
scratch directory. Existing installed extension modules and the existing PG17
server were not replaced or stopped. Pgrx stopped its own test servers after
each terminal run.

With those path variables and SDK overrides, live execution used:

```sh
RUST_TEST_THREADS=1 cargo pgrx test --package pg_accel \
  --pgdata "$PWD/.codex/scratch/hash-implied-filter-20261012/pgdata18" \
  pg18 h3_count_tests
```

PG19 substitutes its version and pgdata directory. The focused PG18 command
substitutes `where_` for the final filter. Full command expansions appear in
the logs. No benchmarks ran; test durations are not performance evidence.

## Remaining gates

Normal SQL admission for the new raw-key/value/FILTER/WHERE shapes remains
closed. General expression predicates, mask-aware reusable H3 transforms,
performance/cost evidence, broader concurrency/corpus/release verification and
CUDA qualification remain separate work. The prior linked-object audit still
has seven unresolved proofs (two grouped and five join exports); it was not
rerun or waived by this live qualification. No larger TODO item is marked
complete, and no commit/push is claimed by this evidence document.

These logs are collected before the coordinating agent's final source capture
and checksum sealing. Historical sealed evidence bundles are unchanged.
