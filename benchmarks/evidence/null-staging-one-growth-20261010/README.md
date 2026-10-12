# Coalesced NULL-prefix backfill reservation

2026-10-10. The shared primitive/text/NUMERIC/geometry/raster staging helper now
calculates prefix restoration and next-batch growth before allocating. A late
first NULL no longer requires two consecutive growth calls. The chosen geometric
capacity matches the preceding two-step policy in the tested matrix; final mask
bytes, retained accounting, ABI and SQL admission are unchanged. NULL-free scratch
remains bounded to one 8,192-row batch.

The infrequent allocation/backfill function is out of line, while the small
reserve wrapper is explicitly inlined. The combined size is checked before
mutation, and allocation/overflow errors leave pending bytes intact. This avoids
one allocator call and a potential full-prefix relocation; it does **not** reduce
final mask capacity or prove an end-to-end PostgreSQL loading speedup.

## Correctness

- PG18 and PG19 each pass **1,282 standalone tests**; strict production workspace
  Clippy passes on both. Existing loader payload, late-NULL and malformed-input
  retry tests remain green.
- New allocation counting proves two growth calls for the original sequence
  versus one for the coalesced sequence, with identical capacity and bytes.
- A 360-case matrix compares capacity/payload across tail, prefix and next-batch
  sizes against explicit two-step Vec growth. Combined-size overflow tests verify
  unchanged bytes and successful retry. Existing exhaustive overlapping-backfill
  tests remain unchanged.
- Safety contracts, doc parity (21 tests), unchanged 90% scope gate, formatting
  and whitespace checks pass. No native source/library or installed PG module
  changed; GPU/live qualification and native audit findings remain open.

## Matched host microbenchmarks

The baseline helper was extracted from the sealed
`hash-fused-cancel-20261010/source-snapshot.tar.gz`. Both executables use the same
driver, extended with second-batch, midpoint and late-batch first-NULL patterns.
They were compiled with `rustc -O --edition=2024`, Rust 1.96.0 / LLVM 22.1.2 on
aarch64 Darwin 25.4.0. Each invocation warms three times, alternates 21 eager/lazy
pairs, verifies every mask and synthetic i64 value, and retains all raw samples.
Executable order is reversed on the repeat. No builds/tests overlapped timing.

Final candidate/baseline median ratios (lower is faster):

| NULL pattern | 12,748,986 rows, batch 8192 | Repeat | 1M rows, reserve per row | Repeat |
| --- | ---: | ---: | ---: | ---: |
| None | 1.005 | 0.997 | 1.010 | 0.982 |
| First | 0.726 | 0.974 | 1.004 | 1.006 |
| Last | 0.806 | 1.006 | 0.983 | 0.967 |
| Late batch | 0.797 | 0.881 | 1.015 | 0.961 |
| Second batch | 0.937 | 0.886 | 1.055 | 0.993 |
| Midpoint | 0.868 | 0.828 | 1.003 | 1.000 |
| Every 997th | 0.932 | 0.875 | 0.990 | 1.002 |
| Alternating | 0.941 | 0.898 | 1.003 | 1.000 |
| All | 0.927 | 0.872 | 1.002 | 0.993 |

Large late-batch medians are 18.117/17.768 ms versus 22.731/20.158 ms. Sparse
medians are 21.584/21.786 ms versus 23.155/24.893 ms, and 1.008/1.001 times the
eager policy in the same candidate executable. Capacity matches baseline for
every measured pattern: NULL-free 8,192 bytes, late-batch 25,493,504, second-batch
16,777,216, midpoint 12,763,136. Per-row second-batch includes a 5.5% slower
sample. First/last speedups vary substantially across repeats. These are local
synthetic staging measurements, not statistical release qualification or full
NUMERIC/raster parsing, PostgreSQL scan or GPU/query timings. The broader
sparse-NULL performance gate remains open.

## Rejected experiments retained

- `candidate-large.jsonl`: reserving only prefix+next-batch cut late-batch
  capacity but increased second-batch capacity from 16 MiB to 24 MiB. Rejected.
- Pairing mask capacity to the value vector passed correctness tests but can
  enlarge midpoint allocations. Rejected before performance qualification;
  production call sites are unchanged in the final version.
- `coalesced-*` and `final-*`: larger non-inlined helpers regressed frequent
  reservation timing. Symbol inspection showed baseline reserve inlined while
  the intermediate candidate retained a call. The retained version (`hot-*`)
  explicitly inlines only the small wrapper and keeps restoration out of line.
  Earlier logs are diagnostic experiments, not evidence for the final timings.

## Reproduce and remaining work

```sh
rustc -O --edition=2024 scripts/bench_null_staging.rs -o /tmp/bench-null-staging
/tmp/bench-null-staging 12748986 values 8192
/tmp/bench-null-staging 1000000 values 1
just test-standalone 18
just test-standalone 19
just lint 18
just lint 19
```

`hot-*` JSONL files are final timing evidence; `*-verified.log` files are final
Rust gates. Baseline source, raw experiments, compiler details and the current
implementation-source/documentation snapshot are bound by SHA256SUMS. Snapshots
exclude Git metadata, generated builds and evidence directories. Formatting-only
changes followed timing; benchmark binaries were not rebuilt after measurement.
Live loading, GPU execution and full performance qualification remain necessary.
TODO/planning documents remain. Git metadata is read-only, so nothing was
committed or pushed.
