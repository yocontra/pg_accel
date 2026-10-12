# Staged Rust hash WHERE binding — 2026-10-10

The staged single-key hash path now binds a direct fact Boolean WHERE column
independently of aggregate FILTER. It owns and charges both SQL Boolean masks
through synchronous dispatch, tracks predicate columns as resident dependencies,
and validates selected-row counts separately from input rows and measure counts.
Raw integer keys support this binding for INT2, INT4 and INT8. A validated
all-rejected raw-key execution can learn a one-group allocation bound; source
generation changes invalidate that learned bound.

H3 WHERE requests force fused key evaluation, including when the qualification
seam requests caching. This preserves rejection before H3 evaluation; cached
parent artifacts reject WHERE until mask-aware transforms are implemented.
Normal SQL admission has not been broadened. Expression predicates, reusable
masked H3 transforms, live GPU parity, performance qualification and admission
remain open in TODO.md.

## Verification

PG18 and PG19 standalone suites each pass 1,310 tests. The new unconditional
host cases cover predicate dependencies, empty binding, malformed columns,
cancellation, closed SQL admission, H3 fused ordering policy, selected-row
bounds, empty-result capacity learning and one/two-mask forecast arithmetic.
Production and pg_test Clippy pass with warnings denied on both majors.
Documentation parity passes 21 tests; safety-contract and coverage-scope checks
pass without lowered floors. Formatting and whitespace checks pass.

Two new PostgreSQL GPU fixtures compile but have not executed. They cover
independent nullable WHERE/FILTER, integer aggregate parity, cold/warm runs,
predicate-only mutations and rollback, empty capacity learning, cancellation
cleanup and retry, plus forced fused H3 execution. No live parity or speedup
is claimed: this host exposes no GPU.

Native source is unchanged from the preceding sealed hash-where-20261010
bundle. Its metadata suite passed 5,852 checks; its GPU suites failed at device
initialization. Its full native audit completed with 124 passing exports and
seven unresolved grouped/join execution findings, with no waivers. Those
native results are not evidence of successful execution of this Rust binding.

The first Rust build failure, caused by moving a mutable cancellation callback
instead of reborrowing it, is retained alongside the successful final logs.
SHA256SUMS seals this bundle and the current source snapshot. Installed
PostgreSQL extension modules were not replaced. Changes remain uncommitted on
main because this session explicitly grants only read access to .git; no commit
or push was performed.
