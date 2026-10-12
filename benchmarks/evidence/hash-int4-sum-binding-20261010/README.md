# Staged INT4 hash SUM/COUNT Rust integration — 2026-10-10

This change connects the native SUM|COUNT implementation to the cached and
fused H3 Rust execution paths. It does not open normal SQL admission or claim
GPU/runtime/performance qualification. The TODO remains unchecked.

## Changes

- Typed measure binding accepts exactly INT4 SUM and COUNT of the same fact
  column, in either logical output order, with the existing optional exact
  Boolean FILTER. Other SUM types, SUM-only, extra states, WHERE and joins
  remain rejected. Resident values and NULL sidecars retain exact length/type
  checks and the existing borrow/ownership boundaries.
- SUM has a distinct `hash_int4_sum_count` specialization label. Existing count
  specializations and public admission gates are preserved.
- Completed compact output must have exact SUM byte lengths, matching COUNT
  and nonnull_count lanes, and sums inside INT4 bounds for each non-NULL count.
  Zero-count groups and unused slots require zero sum bits. This validates
  finalized output only; it does not recompute aggregates on resident rows.
- Existing integer materialization supplies bigint SUM values and SQL NULLs.
  A new real-catalog PostgreSQL fixture exercises this path for both cached and
  fused policies, nullable/filtered inputs, signed extremes, warm reuse,
  updates, subtransaction rollback, and empty input. PostgreSQL with acceleration
  disabled supplies the oracle. GPU absence is a failure, not a successful skip.

## Verification

The PG18 and PG19 standalone suites each passed 1,284 tests, including two new
host tests for typed binding, canonical output orders, dependency discovery,
rejected shapes, and malformed/extreme sum states. Both production strict lint
runs passed. Documentation parity (21 tests), safety contracts, scope audit,
formatting, and whitespace checks passed. Scope remains 43 SQL semantic families
and the unchanged 90% floor.

The live fixture is compile-checked separately for both PostgreSQL versions;
the accompanying live build logs record the results. It has not been executed
on this host. Native code is unchanged from the preceding
`hash-int4-sum-20261010` bundle: that bundle records GPU initialization failures
and seven unresolved native audit exports. Those gates remain open, with no
waiver, fallback, speedup claim, or release qualification.

An initial host-test compile referenced a nonexistent output-size method; it
was replaced with the real kernel-mode contract check. Initial strict lint
rejected two unnecessary clones of Copy types; both were removed. These failure
logs are retained alongside successful reruns.

## Provenance

Logs accompany this file. `source-snapshot.tar.gz` contains implementation and
documentation inputs, excluding Git metadata, generated builds and evidence.
`SHA256SUMS` seals the bundle. No benchmark ran for this change. The current
branch is main, but the session's read-only `.git` permission prevented commits
and pushes. No completed release item is claimed.
