# Reusable spatial device workspace — 2026-10-10

Spatial aggregate preparation now allocates one largest-chunk device workspace,
instead of five device buffers for every chunk. It retains one control block,
failure word, tri-state lane, uncertainty-index lane and uncertainty-count word.
Preflight and actual workspace accounting agree on this capacity. Empty input
allocates no chunk workspace. The final private mask remains relation-sized.

The staged transform launches the first chunk under its original resident-store
borrow. Finalization checks native outcomes and sticky status, downloads the
uncertain prefix, performs exact PostGIS rechecks and patches the mask, then
services interrupts. Only after those gates succeed can another chunk reuse the
buffers. Before each subsequent dispatch, a non-owning identity token processes
pending invalidations and revalidates generation, global generation, relfilenode,
type and row count under a fresh short borrow. This happens even when the previous
chunk had no uncertainty. Raw source pointers do not survive the launch callback;
status mapping, readbacks and PostgreSQL calls remain outside the borrow.

Compaction binds exact active prefixes of reusable scratch, including a short
final chunk and an unaligned final-mask subspan. Existing exact-shape constructors
retain their stricter length checks. Native evaluation begins a new failure chain;
compaction preserves that chain's failures, and patching follows a successful
finish. No failed chunk publishes the private mask. Native code, C ABI, SQL
admission and installed PostgreSQL extension modules are unchanged.

## Accounted memory reduction

For the existing 12,748,986-row / 2,237-coordinate polygon accounting fixture,
chunks contain at most 29,999 rows (425 chunks). Device scratch changes from
114,909,174 bytes to 270,387 bytes: 114,638,787 bytes avoided. The corresponding
scratch allocation count changes from 2,125 to five. These are exact allocation
layout calculations checked by host tests, not measured query speedups.

The final 12,748,986-byte device mask, retained source geometry, base/constant
allocations and selective exact-recheck storage remain separately charged.
This is not beyond-residency-budget streaming, spill or an asynchronous pipeline.

## Verification

PG18 and PG19 standalone suites each pass 1,308 tests. Strict production and
pg_test Clippy pass on both majors. Documentation parity (21 tests), safety
contracts, coverage scope, formatting and whitespace checks pass. Coverage
floors are unchanged. Existing standalone tests include no-device guarded cases;
the two new host tests run unconditionally.

The new tests check active-prefix lengths, mask bounds, overflow and maximum
chunk sizes, plus source type/row/stamp mismatch, eviction and pending
invalidation. Successful source reborrowing allocates zero times and releases
the store guard after each callback. Accounting tests cover empty, partial,
many-chunk and polygon-complexity-limited inputs.

The existing real-PostGIS three-chunk fixture now additionally injects ordinary
error and cancellation in the one-row final chunk, after no-recheck and patched
workspace reuse. It asserts query-reset cleanup, prepared-cache release and a
successful complete-mask retry. It also retains cold/warm parity, changing source
byte layouts, NULL/EMPTY and empty-relation cases. This fixture was compiled on
both PG majors, not executed: this host exposes no GPU. Live correctness and
end-to-end performance qualification remain open.

The initial host run caught an incorrect numeric expectation in the new memory
assertion; strict lint caught four test-only unwrap calls. Both failed logs are
retained, along with successful final reruns. No native audit was rerun because
native sources did not change; the prior sealed narrow-key audit's seven
unresolved findings remain open. No broader release gate is claimed complete.

Logs and a source snapshot accompany this report; SHA256SUMS seals the bundle.
The source snapshot excludes Git metadata, builds and evidence bundles. Work
remains uncommitted on main because this session explicitly makes `.git`
read-only; no commit or push was performed.
