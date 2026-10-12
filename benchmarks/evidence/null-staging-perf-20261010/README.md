# NULL-sidecar reserve fast path and single-write backfill

2026-10-10. Host staging improvement, **not** live PostgreSQL/GPU qualification
or an end-to-end load/query speedup. The original sparse batched regression
remains open.

## Runtime change

The shared NULL helper now inlines reserve and bypasses normalization when
`saw_null` is true and its length already equals the published value-row count.
That equality proves there is no omitted prefix to restore. The ordinary
fallible Vec reserve still runs. Invalid row bounds, the first NULL transition,
and no-NULL scratch clearing retain the normalization checks.

During first-NULL backfill, resize has already initialized newly exposed bytes
to zero. After moving the pending tail, only the old tail bytes before the
destination need clearing: `min(tail, prefix)`, not the entire prefix. For
12,748,986 rows with the first NULL last and 8,192-row batches, this removes
12,744,518 redundant byte writes. This is an operation-count fact, not a
measured bandwidth or query-speed claim. Overlapping source/destination moves
remain supported without new unsafe code.

All callers keep the same append loop, memory policy, byte-per-row final mask,
fallible allocation, ABI, accounting and admission behavior. NULL-free scratch
remains at most one 8,192-row batch. The NUMERIC and raster builders additionally
reserve before each append, making the fast path relevant beyond batch-boundary
primitive loading.

## Benchmark evidence

The benchmark now accepts `rows [values|mask] [batch_rows]`, defaulting to the
previous 8,192-row cadence. Both variants use the same driver source. The
previous helper was extracted unchanged from the sealed
`batched-null-staging-20261010/source-snapshot.tar.gz`; it was also verified
identical to the helper in the immediately preceding live-H3-test snapshot.
That previous helper and the candidate were each compiled with `rustc -O`.
Each run warms three times, alternates 21 eager/lazy pairs, verifies every mask
and synthetic value, and retains raw samples. Previous/candidate executable
order was reversed on the repeat. No builds/tests overlapped timed runs.
Compiler and OS versions are in `host.txt`; the sandbox denied the attempted
sysctl hardware probe, so this bundle does not claim a fresh hardware inventory.

At **1M synthetic value-plus-mask rows, reserve cadence 1**, candidate/previous
median ratios were:

| NULL pattern | First | Repeat |
| --- | ---: | ---: |
| None | 0.554 | 0.530 |
| First only | 0.519 | 0.528 |
| Last only | 0.544 | 0.541 |
| Every 997th | 0.530 | 0.541 |
| Alternating | 0.535 | 0.535 |
| All | 0.528 | 0.529 |

Medians fall from approximately 4.5–4.6 ms to 2.4–2.5 ms. These are helper plus
synthetic i64 staging timings, not full NUMERIC decoding, raster parsing or
PostgreSQL scans. Candidate still costs about 1.26–1.30 times the eager policy
in these per-row runs; the benefit is relative to the previous bounded helper.

At **12,748,986 rows, cadence 8,192**, NULL-free capacity remains 8,192 bytes
versus eager's 16,777,216. The original driver reproduced sparse candidate/eager
ratios of 1.196 before the change and 1.178 afterward. With the configurable
driver, candidate sparse ratios are 1.154 and 1.178; previous-helper ratios are
1.026 and 1.167. This variability and the remaining slowdown do not establish
a fix for sparse batched loading. All-NULL candidate ratios remain 1.017 and
1.021. No unfavorable run was discarded. Mask-only large-run results are also
retained, but do not substitute for value-lane or live loading evidence.

## Checks and remaining work

- PG18 and PG19 standalone suites each pass **1,268 tests**.
- New exhaustive test covers all 2,047 bit patterns for tails of length 0–10
  at nine prefix lengths (18,423 cases), including overlap, zero prefix/tail,
  long prefix, and a second no-allocation normalization/reserve.
- Strict workspace Clippy passes for both majors. Existing loader tests cover
  primitive, dictionary, NUMERIC, geometry and raster payload preservation,
  malformed-input retries and late NULLs.
- Formatting, whitespace, safety contracts, documentation parity and scope
  checks are recorded alongside the raw logs. No thresholds are relaxed.

No native source, installed extension, database server or SQL admission changes
were made. Existing native-export proof failures and GPU/live-PG qualification
remain open. TODO and planning files are not removed. This session cannot write
Git metadata, so the change is uncommitted and unpushed.

## Reproduction

```sh
rustc -O --edition=2024 scripts/bench_null_staging.rs -o /tmp/bench-null-staging
/tmp/bench-null-staging 1000000 values 1
/tmp/bench-null-staging 12748986 values 8192
/tmp/bench-null-staging 12748986 mask 8192
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
```

The source snapshot captures the accumulated dirty worktree; SHA256SUMS binds
the snapshot, this report and raw logs. The previous source is recoverable from
the linked sealed snapshot; benchmark binaries and compiler IR are local
diagnostics, not release artifacts.
