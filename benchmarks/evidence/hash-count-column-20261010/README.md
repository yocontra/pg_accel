# Native hash COUNT(column) candidate

2026-10-10. Extend the existing single-key, one-shot native hash aggregate from
COUNT(*) to a single COUNT(column) state. This is a **compiled native candidate,
not newly selectable SQL, GPU output qualification or a performance result**.

## Implementation

The existing ABI's COLUMN operation accepts BOOL, INT32, INT64, FLOAT32,
FLOAT64, DATE and TIMESTAMP physical inputs, with their existing canonical
widths and byte NULL sidecars. The measure uses only COUNT, I64 accumulation
and eight-byte state. Key/output formats, row/group caps, one-shot lifecycle,
no-filter/no-dimension restrictions and public record layouts are unchanged.

A launch-level choice instantiates the row kernel with or without measure-NULL
handling. COUNT(*) retains a specialization without the additional sidecar
read. COUNT(column) validates the NULL byte, establishes the key's hash group
even for NULL measures, and increments only for non-NULL values. It does not
read measure payloads. Thus all-NULL groups emit count zero, not a missing
group, and H3 key validation still executes even when a measure is NULL.
Any invalid NULL flag or key poisons publication through the existing barrier.

The initial metadata matrix found an unnecessary duplicate nonnull allocation:
126 assertions failed the requirement that COUNT(column) use the same workspace
as COUNT(*). `contracts.log` preserves that failure. The final layout omits
that unused lane for supported HASH shapes; the existing bucket count already
contains the non-NULL count. At the same row/group/key shape, queried workspace
bytes and alignment now equal COUNT(*). Dense layouts are unchanged.

## Verification and limits

- Native shared-library/device compilation and `test_grouped_agg_hash` build.
  Existing metadata-object deployment-target warnings remain visible in logs.
- `test_grouped_agg_hash --contract-only` passes 1,097 checks without device
  initialization: seven physical types, absent/present NULL sidecars, 0/17/1M
  rows, three group capacities, kernel-mode classification, exact workspace
  equality, invalid widths/missing inputs, unsupported partial lifecycle,
  SUM and WHERE. PROT_NONE mappings catch accidental row/sidecar payload reads
  during these metadata queries. This is a tested metadata boundary, not GPU
  execution or a complete static proof.
- Added device tests use an independent grouped-count oracle for 17 and 4,097
  groups, all seven types, absent/all/alternating/group-correlated NULLs,
  unsigned high-bit keys, a NULL key group, empty inputs, zero-count groups,
  insufficient capacity, invalid flags and successful retry using the same
  workspace. H3 composition checks parent grouping and an invalid key with a
  NULL measure. These tests compile but have **not executed on a GPU here**.
- CTest fails `test_grouped_agg_hash` with exit 1: pgaccel_init reports no SYCL
  device, followed by the expected inability to allocate device input. CTest
  exits 8. Its 1,097 preceding metadata passes do not turn that into a skip or
  a passing kernel test.
- Core suites pass 1,263 tests under each of PG18 and PG19. The new Rust matrix
  verifies planned mode, runtime capability and normal admission still reject
  hash column counts across nine SQL input types. Runtime Rust code was not
  broadened. Live PostgreSQL SQL parity is not established by these core tests.
- Strict Clippy passes under both PG18 and PG19.
- Documentation checks (21 tests), safety-contract checks, formatting and
  coverage-scope audit pass. The existing 229 Rust / 18 C++ / 36 CTest / 100 SQL
  inventory and 90% threshold remain unchanged.
- The fresh native export audit exits 1: 123 of 130 exports pass, with the
  same seven failing aggregate/join entrypoints as the previous bundle. That
  includes the grouped execution exports used here: their unresolved static
  device-work proofs are not waived by the metadata tests. No ABI rebaseline
  or audit exemption was introduced. `release-audit.json` retains the result.

No installed PostgreSQL module or running server was modified. There is no CPU
row-execution fallback and no latency/speedup claim. Integration still requires
resident measure binding, output reconstruction that does not assume counts
sum to input rows, GPU/live PG18/19 parity and performance qualification before
normal SQL admission. DISTINCT, multi-measure/multi-key hash aggregation,
streaming and the broader TODO remain unfinished.

## Reproduction

```sh
cmake --build pgaccel-kernels/build --target pgaccel_kernels_shared \
  test_grouped_agg_hash --parallel 2
pgaccel-kernels/build/test_grouped_agg_hash --contract-only
ctest --test-dir pgaccel-kernels/build -R '^test_grouped_agg_hash$' --output-on-failure
just test-standalone 18
just test-standalone 19
```

The source snapshot captures the complete dirty worktree, not only this change.
SHA256SUMS binds the evidence files. Git metadata is read-only; this candidate
has not been committed or pushed.
