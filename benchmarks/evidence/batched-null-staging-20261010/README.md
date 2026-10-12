# Batch-bounded resident NULL-mask staging

2026-10-10. This change reduces temporary host memory for NULL-free resident
columns; it does not establish an end-to-end query speedup or qualify a release.

## Runtime change

Primitive, text, numeric, geometry and raster builders previously accumulated
one byte of NULL flags per row even when the final resident column would omit
the entirely zero mask. They now retain only a scratch batch while no NULL has
been seen. The direct table scan reserves every 8,192 rows; numeric and raster
builders also reserve before individual appends. Each reserve discards the
previous all-zero prefix. Its length remains implicit in the value-row count.

The existing per-row append and `saw_null` update stay unchanged. After the first
NULL, the next reserve or finalization restores the implicit zero prefix before
publishing the full byte-per-row sidecar. An overlapping move preserves the
pending tail. Allocation failure during backfill leaves that tail unchanged.
Text dictionary remapping uses the finalized mask, including the no-mask case.

No SQL admission, ABI, final mask layout, retained-memory accounting or GPU
kernel changes are made. The scratch bound requires the producer's reserve
protocol; an arbitrary private builder used without batch reserves is not
bounded by this helper alone. Once any NULL exists, the full mask is retained.

## Validation

- PG18.4 and PG19beta1 standalone Rust suites: 1,258 passed each.
- Nine additional tests cover primitive types, text dictionary remapping,
  exact numeric/geometry/raster payloads, late NULLs across multiple batches,
  empty/all-NULL inputs, malformed-input retries, overlapping backfill and
  impossible allocation without pending-tail mutation.
- Strict Clippy results, formatting, safety contracts, documentation parity
  and scope checks are retained alongside the test logs.
- The new production module is included in the coverage inventory: 229 Rust
  files rather than 228. The 90% threshold, SQL baseline and C++ scope are not
  reduced. An initial scope-drift failure is retained before the explicit
  inventory update.

These are standalone host checks, not fresh live PostgreSQL integration tests.
No installed extension module was replaced. This host exposes no SYCL GPU;
there is no new GPU, CUDA or complete-release qualification. The seven existing
release-audit findings and the native-only row-join admission boundary remain.

## Microbenchmark and tradeoff

`scripts/bench_null_staging.rs` includes the production helper directly and
compares it with the former eager flag-buffer policy. Both use the same
batch boundaries and append loop. It tests mask-only and synthetic i64
value-plus-mask staging, checks every output outside timing, warms each variant
three times, and alternates the order of 21 paired observations. Raw timings
and peak mask capacities are in the `bench-batch-*.jsonl` files.

At 1,000,000 NULL-free rows, mask capacity falls from 1,048,576 to 8,192 bytes.
At 12,748,986 rows, it falls from 16,777,216 to 8,192 bytes (99.95%). This is
allocated mask capacity, not a measurement of total process RSS or retained
column size. The larger row count matches the NYC month corpus size, but the
benchmark uses synthetic values, not PostgreSQL table loading or taxi data.

Value-plus-mask median candidate/eager ratios at 12,748,986 rows:

| NULL pattern | First run | Repeat |
| --- | ---: | ---: |
| None | 0.895 | 0.923 |
| First row only | 0.990 | 0.973 |
| Last row only | 0.867 | 0.832 |
| Every 997th row | 1.263 | 1.167 |
| Alternating | 0.975 | 0.973 |
| All | 1.046 | 1.083 |

Lower is faster. Sparse NULLs are slower in both large runs; all-NULL inputs
are also slower. These unfavorable results are retained, not dismissed as
noise. At 1M rows the six value-plus-mask ratios range from 0.980 to 1.028.
The accepted improvement is bounded scratch on NULL-free columns. Broader
load/refresh timing and nullable-column performance remain follow-up work.

An earlier per-row lazy-mask candidate added branching to every row and was
rejected after substantially slower timings (about 2x in the value-lane
experiment). `rejected-rowwise-source.tar.gz` and the non-`batch` benchmark
logs preserve that investigation; they are not evidence for the final code.
The earliest mask-only experiment also used byte input rather than bool input.
Initial core/lint failures and their successful reruns are retained separately.

## Reproduction and source identity

```sh
ulimit -n 4096
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
rustc -O --edition=2024 scripts/bench_null_staging.rs -o /tmp/bench-null-staging
/tmp/bench-null-staging 1000000
/tmp/bench-null-staging 1000000 values
/tmp/bench-null-staging 12748986
/tmp/bench-null-staging 12748986 values
```

Measurements ran on the Apple M2 Max host without overlapping native/Rust
builds. This remains a local microbenchmark, not statistical release evidence.
`source-snapshot.tar.gz` records the complete dirty source state, and
`SHA256SUMS` binds all files in this evidence directory. The runtime delta from
the preceding workspace-call-proof snapshot is limited to residency's loader,
numeric, geometry and module files plus the new NULL helper. `.git` is
read-only in this session: none of these local changes is committed or pushed.
