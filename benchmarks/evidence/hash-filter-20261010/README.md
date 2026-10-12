# Native conditional hash counts

2026-10-10. Implements the native measure-FILTER step for conditional
high-cardinality/H3 rollups. **No new SQL admission or GPU qualification.**

## Implementation

The existing one-key, one-count, one-shot HASH route now accepts a canonical
exact SQL measure mask for COUNT(*) or COUNT(column). Dedicated filtered kernel
specializations read TRUE/FALSE/UNKNOWN; only TRUE contributes. Unfiltered
specializations do not read a mask. No layout, ABI export, row/group limit,
planner gate, cost coefficient or CPU fallback changed.

Every row still creates or finds its group. Rejected rows preserve zero-count
groups and consume group capacity; selected_count remains the input row count.
The key, including fused H3 validation, is evaluated even when FILTER rejects.
COUNT(column) reads its canonical NULL byte only when FILTER accepts and never
reads value payloads. Bad evaluated mask/NULL bytes suppress publication through
the existing failure barrier. RECHECK, range/scalar filters, WHERE, multiple
measures and persistent/chunked hash state remain outside this capability.

## Verification

- Shared library and native hash test executable build successfully. The
  existing metadata-object macOS deployment-target link warning is retained.
- Protected-memory metadata queries pass **1,901 checks**, including the prior
  COUNT(column) cases. Row buffers are PROT_NONE, so even one accidental payload
  read would fault. Filtered/unfiltered workspace sizes and alignments match.
- GPU fixtures compile for all seven physical column types plus COUNT(*),
  optional NULL sidecars, all/mixed/no accepted rows, SQL UNKNOWN, small and
  parallel-sorted outputs (17 versus 16,385 actual groups), warm reuse, malformed
  masks, failure/retry, short group capacity, host-only/overlapping mask pointers
  and fused H3 keys. A 1,000,017-row hot-group fixture accepts only rows in the
  second launch and injects a late mask error to check all-or-nothing publication. These
  fixtures are **not executed successfully**: the CTest fails because no SYCL
  GPU is visible. This is not a successful skip or CPU substitute.
- PG18/PG19 standalone suites each pass **1,268 tests**. These are host tests,
  not live PostgreSQL FILTER equivalence or GPU result evidence.
- Safety contracts, documentation parity (21 tests), scope audit and whitespace
  checks pass. Coverage thresholds and the 130-export ABI inventory are unchanged.
- The first metadata run found 57 incorrect test expectations, not admitted
  malformed descriptors: RESET without ACCUMULATE is invalid for nonempty
  input, and ACCEPT_NULL is a valid-but-unsupported scalar predicate only for
  the INT32 column case. Final tests distinguish those contracts; initial
  failures are retained instead of discarded.

The linked release audit exits 1: 123 exports pass and the same seven remain
unresolved (the two grouped-aggregate execution exports and five join
execution/count/projection exports). The marker predates the source edit and
shared-library rebuild; the audit consumes that rebuilt object's current
metadata-binding receipt. Its report and raw GPU failure logs are included. No failing
audit rule has been relaxed. GPU/PostgreSQL correctness, performance and release
qualification remain open; there is no speedup claim.

## Remaining integration

Bind resident predicate masks with exact lengths, dependency/currentness and
ownership proofs. Teach Rust compact-output checks that filtered COUNT(*) can
emit zero-count groups and has a count total no larger than selected_count,
without weakening unfiltered COUNT(*) partition checks. Add live cached/fused
H3 qualification for prepared plans, DML, rollback and failure recovery. Only
then consider normal SQL admission and calibrated performance gates.

## Reproduction

```sh
ulimit -n 4096
cmake --build pgaccel-kernels/build --target pgaccel_kernels_shared test_grouped_agg_hash --parallel 2
pgaccel-kernels/build/test_grouped_agg_hash --contract-only
ctest --test-dir pgaccel-kernels/build -R '^test_grouped_agg_hash$' --output-on-failure
just test-standalone 18
just test-standalone 19
just safety-contract-audit
just doc-parity
```

The source snapshot captures the complete accumulated dirty worktree, not just
this increment. SHA256SUMS binds the report, logs and snapshot. Git metadata is
read-only in this session, so no commit/push was possible. TODO and planning
docs remain because the full objective is unfinished.
