# Device-resident spatial mask publication: 2026-09-29

Spatial artifact preparation now writes each chunk into a checked subspan of
one private device mask. Exact PostGIS rechecks patch that same buffer. Only
after every launch, failure flag, uncertainty count/index, exact result and
catalog/dependency check succeeds does ownership move to the resident artifact.
Errors and cancellation discard the unpublished buffer. Native calls complete
synchronously, including unaligned byte-offset compaction/patch stores.

This removes the full host mask, per-chunk mask downloads, final whole-mask
upload and separate final device-mask allocation. For 12,748,986 rows in 425
chunks, it removes 12,748,986 bytes of host staging, 25,497,972 bytes of mask
transfers, 425 net device allocations and the additional full device mask that
previously existed during finalization. These are structural savings, not
measured peak-RSS claims. Persistent artifact bytes remain unchanged. Preflight
and actual workspace accounting agree; existing separate transient/persistent
ledger reservations are preserved. Exact snapshots and other chunk scratch
remain. This is not shared residency, bounded-memory streaming or spill.

The logical plan, C ABI, admission gates and cost coefficients are unchanged.
The new compaction builder checks overflow, capacity, chunk shape and mask
subspan bounds before pointer arithmetic. Its full-buffer companion retains
the prior exact-length contract.

## Verification

- 1,150 core tests pass, including exact full-month preflight accounting with
  no relation-sized host mask.
- Seven live spatial tests pass on each of PostgreSQL 18 and 19. They include
  subspan offset/guard preservation, uncertainty compaction, prefix transfers,
  exact-result errors/cancellation, injected GPU failure without fallback,
  rescan, prepared queries and DML/catalog invalidation.
- Ten targeted production SQL files pass on each major: 84, 90, 96, 112, 114,
  118, 120, 122, 123 and 125. Device availability is required; all completion
  markers are present, with no warnings/skips. Logs are in `sql-pg18/` and
  `sql-pg19/`. These cover transaction/selected/declined lifecycle behavior,
  holes, multipolygons, decimal/integer/extrema aggregates, spatial/scalar
  conjunctions and the 240-shape NULL-aware matrix.
- Native CTests pass 32/32 in 68.13 seconds. Both extension Clippy builds,
  formatting, documentation tests (21), scope audit and whitespace checks pass.
  The coverage inventory remains 88 SQL files / 412 assertions / 37 families.

This turn did not rerun the full 88-file suites, upstream PostgreSQL suites,
CUDA, concurrency or release qualification. Those are not implied by the
targeted tests above. Verification logs are in `verification/`.

Installed module SHA-256:

- Baseline PG18: `ec9bc48d1db8f25dbd748284b455fd30b40032c4459970ce5a2931656a696d35`
- Candidate PG18: `441cfcbf07925c6f54e219a9effdc6540f868798c24f018196ea39d1d71f19b1`
- Candidate PG19: `91c335bb46479e28aef448faabc2fab98f49f7880ccd1b2bde3cf551a968915a`

Each benchmark verifies its installed PG18 module against the matching release
artifact. The three runs have identical captured GUC settings; the canonical
settings SHA-256 is
`b1cee31c06894ad0ce288d11c039006e82ccb27473d1a15aede722390e76f4da`.
Postmaster start times differ because the candidate was installed/restarted.

## Matched local observations

Apple M2 Max / 64 GB / Metal / PostgreSQL 18.4, using the unchanged
`nyc_taxi_airport_nullable_passengers` workload. Each run retains five warmups,
ten paired raw warm observations against configured parallel PostgreSQL,
separate initial preload and one first-use refresh/lifecycle observation per
scale. No test/build jobs overlap timing. The unchanged real NYC corpus has
zero NULL passenger counts; actual NULL semantics are exercised by SQL125.

| Rows | Build/run | Preparation ms | Refresh ms | First-use ms | Native first-use ms | Warm pg_accel / PG ms | Separate preload ms |
|---|---|---:|---:|---:|---:|---:|---:|
| 1M | Baseline | 125.301 | 207.970 | 339.494 | 40.590 | 7.694 / 40.732 | 245.514 |
| 1M | Candidate | 124.215 | 212.957 | 345.166 | 40.051 | 8.584 / 40.401 | 257.466 |
| 1M | Repeat | 150.276 | 209.895 | 367.603 | 45.419 | 7.190 / 42.452 | 250.177 |
| 12,748,986 | Baseline | 4049.777 | 2754.230 | 6837.444 | 263.624 | 40.867 / 283.949 | 2730.527 |
| 12,748,986 | Candidate | 3176.372 | 2764.020 | 5971.747 | 292.062 | 36.364 / 304.190 | 2754.974 |
| 12,748,986 | Repeat | 3352.664 | 2772.243 | 6158.770 | 329.241 | 40.495 / 293.202 | 2761.915 |

All six cells match PostgreSQL exactly, verify `parallel_dense_integer`,
process every measured input row on the GPU and record zero stock fallback.
All three failure ledgers are empty. Raw samples, plans, correctness diffs,
provenance and lifecycle details are retained in `baseline/`, `candidate/` and
`candidate-repeat/`.

Full-month preparation is 17–22% lower in the two candidate observations,
with total first use 10–13% lower. At 1M, preparation and first use do not
consistently improve; the repeat is slower. Three individual lifecycle probes
are not statistical first-use certification. Warm execution is unchanged by
design and varies across runs; the warm ratios are not evidence of fresh
classification speed. First use still loses badly to native PostGIS. These
are local dirty-tree observations, not a universal performance claim. The
broader first-use, streaming, shared-residency and release work stays open.
