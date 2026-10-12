# Staged SMALLINT/INTEGER hash keys — 2026-10-10

Verification is complete within the scope below; SHA256SUMS seals this bundle.

The single-key native hash path and staged Rust RawHash owner now accept
SMALLINT/INTEGER keys as well as BIGINT. The former borrow existing resident
INT32 lanes (SMALLINT already uses lossless residency widening); device key
loads sign-extend to the existing INT64 equality/sort representation. There is
no new widened input buffer, dictionary, H3 dependency or host distinct scan.
H3 transforms remain restricted to INT64.

Both input widths publish INT64 compact keys. Native workspace layout, Rust
output allocation, reusable output pool identity and memory forecasts agree
on that eight-byte representation. Output validation checks logical INT2/INT4
bounds before accepting results or learning a group capacity. Typed PostgreSQL
Datums preserve SMALLINT/INTEGER result types. Unsigned physical ordering is
internal and does not promise SQL ORDER BY semantics. Public ABI structures
and exported symbols are unchanged; the newly supported key-output convention
is documented in the public header.

## Verification scope

- Native shared library and grouped test targets build. Protected-input
  metadata tests pass 2,969 checks, including both physical key widths,
  SUM/MIN/MAX subsets, COUNT forms, FILTER, equal workspace accounting and
  rejection of INT32 H3 transforms.
- Device fixtures cover signed endpoints, duplicates/NULLs, COUNT(*) and
  COUNT(column), all value-lane combinations, FILTER, 17/16,385 groups,
  1,000,017-row launches, warm workspace reuse, malformed sidecars,
  wrong output widths, alias/capacity failure and retry.
- Both grouped GPU CTests fail at initialization/allocation because no GPU is
  visible (0/2 pass). This is not device correctness or performance evidence.
- Rust host cases cover INT2/INT4/INT8 raw key binding, all five logical
  aggregate kinds, FILTER, output pooling/width/accounting, canonical sign
  extension, typed Datums and rejection of out-of-domain compact keys.
- PG18/19 standalone suites pass 1,302 tests each. Production and pg_test
  strict Clippy pass on both versions. Documentation parity passes 21 tests;
  safety-contract, coverage-scope, formatting and whitespace checks pass.
  The existing coverage scope and 90% floor are unchanged.
- A new real-catalog live fixture covers INT2/INT4 keys, 16,388 groups,
  COUNT(*) and mixed MIN/AVG/MAX/SUM/COUNT outputs, FILTER, warm reuse,
  key-changing DML/rollback, all-NULL and empty input. It is compile-only
  evidence on this host, not a live PostgreSQL execution result.

The freshly linked full native audit reports 124 passes and seven unresolved
findings across 131 exports, unchanged from the preceding increment. No
device-work exemption or lowered gate was introduced. Audit JSON and linked
metadata binding accompany the logs.

Normal SQL hash admission remains unchanged. Native audit findings and the
broader TODO objective remain open. Installed extension modules were not
replaced; no query speedup or release qualification is claimed. Build logs
retain the existing macOS deployment-target warning.

Source snapshots exclude Git metadata, build outputs and evidence bundles.
This snapshot predates the separate spatial exact-selection increment.
The branch remains main; read-only `.git` prevents committing or pushing.
