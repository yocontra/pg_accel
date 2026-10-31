# Adaptive WHERE COUNT correctness qualification

The final source passed **35 focused live tests on PostgreSQL 18.4 and 35 on
19beta1**, using Apple M2 Max/Metal. This qualifies the tested correctness
envelope, not performance, other devices, the full upstream regression suite,
static source proofs, or release readiness.

## Source and final results

Implementation checkpoint: `89da8dba2bcbfdb906882b350b5a8b194c5db454`.
The tested repository base is `27bc3cfb659472e211864ac03d754f87bcc76ab7` plus
[qualification-tests.patch](qualification-tests.patch), which contains the two
test-file corrections and added spill case. Production code did not change
during these final corrections. Per-run source hashes bind the exact inputs.
The native partition-v2 foundation has its own
[qualification archive](../hash-count-where-native-20261030/README.md).

| Suite | PG18 | PG19 |
| --- | --- | --- |
| Raw COUNT, including WHERE and cancellation | 9 passed; 110.87 s | 9 passed; 136.00 s |
| H3, including WHERE and actual final-tuple spilling | 22 passed; 169.77 s | 22 passed; 167.96 s |
| Planner equivalence and admission boundaries | 4 passed; 25.00 s | 4 passed; 25.27 s |

[qualification4/terminal.log](qualification4/terminal.log) records complete/0.
Each `pgtests*-qualification4` directory retains its full log, exact named-test
check, source/harness/protected-file checks, module hash and loader links. The
inventories are `raw.expected`, `h3-22.expected`, and `planner.expected`.
The trailing Cargo integration-test binary has zero matching tests and exactly
55 filtered tests; it is not counted toward these 70 tests. The checker still
requires a nonempty main suite with every expected test passing and none ignored.

`h3-compile1` records both-major test builds; `h3-validate1` records 1,365 ordinary
tests, strict Clippy and formatting. Their before/after source checks match.
Execution evidence includes verified GPU modes, native-call counts and complete
bidirectional result parity, not merely an EXPLAIN path label or test discovery.

## Covered contracts

- Nullable INT2/INT4/INT8 keys and all nine fixed-width COUNT column types;
  direct Boolean WHERE with independent or folded FILTER masks.
- Rejected rows precede H3 key validation; accepted invalid cells still error.
  Successful partition populations conserve WHERE input independently of FILTER.
- Empty input, predicate changes, rollback, capacity learning, interruption,
  cleanup, retry, exact budget boundaries and no stock fallback in asserted paths.
- The compact H3 typed matrix genuinely partitions but has only 123 final groups,
  which fit in memory. A separate 4,098-group WHERE case actually spills at 64kB,
  with NULL/zero-count groups, three mask forms and full stock parity.

Test capacity caps (including 32, 512 and 4096 in relevant fixtures) exercise
correctness paths. They do not establish natural-capacity planner ranking or
performance. Default cost coefficients and competing production paths were not
changed to make these tests select a preferred strategy.

## Preserved failures and corrections

Earlier ordinary/build/lint failures, source snapshots, review patches and terminal
receipts remain in this archive. Failed runs have not been relabeled successful.

1. Initial live WHERE queries selected dense paths. Candidate diagnostics exposed
   omitted dense WHERE consumer reads. Narrow accounting uses existing coefficients
   and actual selected native mode; dense construction policy remains unchanged.
2. The cancellation test incorrectly required overflow-only EXPLAIN fields after
   a valid one-pass retry. Full-plan diagnostics proved verified raw hash execution
   and exact lifecycle counts. The correction preserves cancellation, allocation,
   parity and call-count checks, and validates coherent overflow evidence when present.
3. A budget fixture read memory sizes from fingerprint-tail positions that now hold
   WHERE metadata. It now uses its existing independently checked typed native quote,
   retaining one-byte-short rejection and exact-fit success.
4. The small H3 matrix incorrectly required disk spill from 123 final groups.
   Its partition checks remain, and the dedicated larger WHERE case supplies the
   positive spill coverage instead of forcing a misleading assertion.
5. Qualification2's raw tests passed, but the old parser rejected Cargo's separate
   zero-match auxiliary suite. `check-live2.py` narrowly recognizes that exact
   auxiliary section. Its 30 offline tests and `raw9-reinterpretation.log` are
   retained; the original queue remains failed. Qualification4 reran all 70 tests.

## Integrity and limitations

`copy-inventory.json` records original artifact paths, lengths and hashes. Its
copy-status/remaining-work descriptions are the preserved pre-copy planning
snapshot, not the final archive status; the final file set is bound below.
`SHA256SUMS` covers the archive contents other than itself. No PostgreSQL data
directories, installed prefixes, binaries, JIT caches or unfinished paired
performance implementation are included. Some logs contain local usernames,
paths and synthetic SQL; credential-pattern checks are not a secrecy proof.

Each live stage rebuilds and reinstalls the owned extension module. Historical
module hashes are receipts, not retained executable artifacts: later stages
overwrite that path. Do not claim every historical module can be rehashed now.
The final modules and all current source/protected-file checks were verified;
these receipts are not a hermetic-build or loaded-image provenance proof.

Paired candidate timings, the fixed WHERE performance matrix, released-workload
controls, broader predicates/types, static proofs and release gates remain open.
The included paired-harness recommendation is a proposal, not measured evidence.
