# Staged hash integer MIN/MAX binding — 2026-10-10

Cached/fused H3 and raw BIGINT-key hash bindings accept any nonempty,
duplicate-free subset of INT2/INT4 SUM/AVG/COUNT/MIN/MAX outputs. They request
only necessary native value lanes, with COUNT retained internally for validity
and AVG. COUNT-only specialization is preserved. Extrema use the new private
`hash_int4_extrema` specialization label; existing SUM/COUNT labels are stable.

Compact output validation checks exact enabled-lane widths, matching validity
counts, logical type bounds, zero-count states, ordered extrema, SUM bounds
implied by requested extrema, and singleton consistency. Existing typed tuple
materialization supplies SQL MIN/MAX in the input type. No CPU aggregation,
extra residency conversion, public ABI change or SQL admission expansion occurs.

## Verification scope

PG18/19 standalone suites pass 1,301 tests each. Strict production and pg_test
Clippy pass on both versions. Documentation parity passes 21 tests; safety,
coverage-scope, formatting and whitespace checks pass with unchanged floors.

Host matrices cover all 31 output subsets on both logical types, cached/fused
binding, FILTER, minimal native lane masks, and all 120 full-output orderings.
State validation tests cover optional lanes, NULLs, malformed output, joint
state inconsistencies and counts through UINT32_MAX.

Real-catalog pg_test fixtures add mixed extrema output orderings to H3 and
16,388-group raw BIGINT-key rollups, NULL/FILTER, warm reuse, mutation/rollback
and interruption/retry. The previously expanded SMALLINT fixture still had an
INT4-only assertion; this increment fixes it and uses typed INT2/INT4 Datum
conversion. Live fixtures are compile-checked, not executed on this host.

Initial failed compile/lint logs are retained alongside final verification
logs. No GPU performance or live correctness claim is made. The separate
native bundle records 2,489 metadata checks and seven unresolved release-audit
findings across 131 exports; those findings remain open. Installed extension
modules are unchanged. The broader TODO objective is incomplete.

The source snapshot excludes Git metadata, builds and evidence bundles.
SHA256SUMS seals the collected files. Branch main remains uncommitted because
the session has read-only access to `.git`; no push occurred.
