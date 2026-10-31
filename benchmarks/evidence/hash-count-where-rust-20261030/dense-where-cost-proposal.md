# Narrow dense WHERE consumer-cost correction (proposal only)

No native changes, coefficients, thresholds, candidate suppression, fixture changes,
or assertion relaxation. Both failed default queries admitted the raw candidate;
the existing dense candidate had the lower modeled total. Diagnostic1 is retained.

## Source facts

- `DenseCountWork::supports` currently requires no fact filter. The native work
  query already accepts direct SQL WHERE in parallel dense COUNT and reports
  `sql_mask_checks = row_count` on ACCUMULATE, zero on FINALIZE. Both atomic and
  hierarchical COUNT workers read the SQL mask once per input row (cpp 3213,
  3264), not once per counter lane or group. Existing lifecycle summation already
  counts rows once across RESET/ACCUMULATE chunks and a zero-row FINALIZE.
- Integer atomic workers (cpp 3550+) and chunk workers (3674+) each read WHERE
  once per row per value worker. `dense_integer_worker_count` is value lanes minus
  an elided scalar-WHERE carrier. A direct SQL WHERE has no scalar carrier; one
  COUNT column or lowered COUNT bytecode is exactly one worker. COUNT(*) is the
  separate COUNT family, not another integer worker. Chunking partitions rows:
  do not multiply total rows by lifecycle calls or number of groups.
- Existing scalar-bytecode WHERE instruction pricing is distinct. Raw/H3 mask
  reads are already charged. Native COUNT work replaces aggregate cost, so an
  extra generic SQL mask term on that path would double count.
- Dense cached SQL masks are built by a host row loop in artifact.rs:1179–1198,
  together with dictionary construction. They are not the raw path's per-query
  GPU mask conversions. Dense policy has zero GPU construction launches and a
  byte-throughput construction estimate; that policy is not PG aggregate cost.

## Proposed implementation envelope

1. Extend Rust native COUNT-work pricing eligibility only to direct same-fact
   Boolean SQL WHERE, retaining all current raw-key/one direct COUNT/star,
   no aggregate FILTER, no join, no computed expression restrictions. Keep the
   existing no-WHERE envelope byte-for-byte equivalent. Native mode must still
   independently be ParallelDenseCount before querying work. Use existing
   `dense_count_work_requirements` and exact selected lifecycle. Its existing
   reduce/filter/serial coefficients price all work including SQL reads; no
   additive generic WHERE term for this family.
2. For the residual diagnosed legacy ParallelDenseInteger family, add a narrow
   exact-residency consumer correction after existing aggregate replacements:
   one COUNT-only measure (including a lowered checked scalar COUNT), direct
   same-fact Boolean SQL WHERE, no joins. Charge resident/estimated lifecycle
   rows * one worker * existing gpu_op_cost_filter. Exclude raw/H3, native COUNT,
   scalar-bytecode WHERE, non-COUNT/multiple-measure/numeric families. Selected
   native mode, not a speculative shape label, must prove the integer family.
   This fixes the actual one-worker omission without claiming a complete cost
   model for all integer/mixed aggregates. The general native multiplicity above
   is documented but not used to expand production scope.
3. No construction-cost change in this slice. Keep cache hit/miss and cold mask
   creation separate in evidence; no query-local GPU conversion charge on a
   cached host-built artifact. A future constructor model needs its own source
   and timing evidence rather than changing amortization to force selection.

Production scope: descriptor.rs eligibility/helper, generic_groupagg.rs exact
selected lifecycle pricing. Tests in those files and existing shape/tests.rs;
test-only diagnostic observer may include work facts/cost components. No ABI,
native, artifact policy, cache/currentness, budget, or path ordering changes.

## Required exact regression coverage

- Native COUNT facts for no-WHERE versus direct WHERE: all facts unchanged except
  sql_mask_checks, exact rows for atomic/hierarchical and bounded tail lifecycle;
  zero input and FINALIZE contribute zero reads. COUNT star/column, key widths,
  nullable key/value; malformed/nondirect WHERE and aggregate FILTER excluded.
- Price equals parallel*reduce + (null_checks+sql_checks)*filter + serial*serial;
  no-WHERE totals unchanged, launches independent, overflow/error propagation.
- Residual integer COUNT direct/bytecode one worker: delta exactly rows*filter,
  unchanged across atomic/chunk boundaries and selected chunk sizes; zero rows
  zero charge. No double charge for native COUNT, raw/H3, bytecode WHERE; other
  measures and numeric replacement branches unchanged.
- Real default candidate records for the same two failed SQL queries: selected
  mode, calls, rows, complete work products, base/replaced aggregate, mask-read
  increment, conversion, load/output/launch components and final comparator.
  Do not assert the corrected result must be raw; retain required v2 coverage.

## Fixed paired diagnostic before ranking claims

Use both unchanged failed query/fixture definitions on PG18, default coefficients
and natural executor capacities (the cap4096 in the correctness fixture remains
in its original test, not used to claim production ranking). A separately
reviewed test-only direct descriptor harness executes explicit Legacy and
RawIntegerCount selections without changing planner costs or production flags.
Both arms must consume full results, match stock full key/count maps, and prove
their actual selected physical mode; raw must execute v2 and stock fallback=0.
Freeze harness before timing. Five warmups per arm then ten balanced A/B pairs,
all samples retained, no best-of/retry. Report warm cache-hit timings separately
from ten paired fresh/invalidated-artifact executions including construction;
retain JIT preparation outside measured intervals and disclose construction
bytes/time/cache state. No speed threshold invented for this diagnostic.

After source review and both-major correctness, unchanged designated32 WHERE
cells, released22 matrix, G4096 and six strict controls against0af remain required.
If legitimate dense still wins a designated raw cell, report the outstanding
milestone gate rather than forcing a path or broadening assertions.
