# Staged native hash INT32 MIN/MAX — 2026-10-10

The full release audit completed with seven unresolved findings across 131
exports (124 pass). This is not a release qualification or an audit waiver.

The single-INT64-key native hash path accepts optional MIN/MAX in any
combination with SUM, with COUNT required as the shared non-NULL validity lane.
Input remains one INT32 column, optionally SQL-mask filtered; all previous
row/group caps and one-shot/no-WHERE/no-dimension restrictions remain.

Signed 32-bit atomic bucket extrema initialize to INT32_MAX/MIN. Only accepted
non-NULL values update them; every key still creates its group. Compaction
widens extrema exactly to I64, publishing zero bits for zero-count groups.
Serial heapsort and parallel merge carry every enabled state with its key/count.
COUNT-only launches still avoid value payload reads. The full failure barrier
continues to gate all output lanes, including newly enabled extrema.

Each extremum needs four bytes per bucket and eight bytes per compact group;
parallel sort adds another eight bytes per group, plus alignment. Private
workspace and KernelParams sizes change, so callers must query the current
layout. Public ABI layouts and exported symbols do not change.

## Verification scope

- Native shared library and both grouped test targets build successfully.
- Protected-input metadata contracts pass 2,489 checks without GPU init,
  including all seven SUM/MIN/MAX subsets, required COUNT and FILTER layouts.
- Device fixtures compile for all extrema combinations, NULL/filtered groups,
  distinct per-group values to detect sort misalignment, signed endpoints,
  17 and 16,385 groups, 1,000,017 input rows, scratch reuse, corruption/retry,
  output aliases, capacity exhaustion and empty input.
- The two grouped CTests fail at GPU initialization (0/2 passed). No device
  correctness or performance result is claimed.
- PG18/19 standalone Rust suites pass 1,299 tests each against the updated
  native code, and both strict production lint runs pass. Documentation parity
  passes 21 tests; safety, scope, formatting and whitespace checks pass.

At this snapshot, Rust MIN/MAX binding and SQL admission are unfinished. This is
native implementation evidence only; the broader TODO objective is incomplete.
Installed PostgreSQL modules were not replaced. Build logs retain the existing
macOS deployment-target warning.

Logs, audit JSON, metadata binding, source snapshot and SHA256SUMS accompany
this report. The snapshot excludes Git metadata, builds and evidence bundles
and predates the separate Rust extrema-binding increment.
The branch is main, but read-only `.git` prevents commits and pushes.
