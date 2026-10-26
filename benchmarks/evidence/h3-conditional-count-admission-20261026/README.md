# Conditional H3 counts and bounded GPU partitioning

Local production correctness and warm-resident qualification passed on the
scope below. This evidence does not constitute release approval.

## Evidence map

- `raw/repair-production{18,19}-normal-final1/`: production compatibility.
- `raw/repair-production{18,19}-cap-final1/`: real-cap partition, spill,
  cancellation and recovery, including plans and numeric receipts.
- `raw/repair-production{18,19}-integration-final2/`: corrected full eight-test
  suites; `concurrency/` retains exact failed and successful soak artifacts.
- `raw/repair-performance18-measured-*/report.json`: all ten final reports,
  paired samples, settings and nested correctness/plan artifacts. Earlier
  `production18-*` reports are provisional prior-source measurements.
- `raw/repair-production-build{18,19}-final2/`: final test-source manifests and
  module/benchmark identity. `final1` and PG18 `perf1` retain original identities.
- `SHA256SUMS`: exact published inventory; original receipts retain original
  local paths, so their embedded absolute paths are provenance, not portable
  replay instructions.

## Implemented scope

The default planner can accelerate a single H3-parent grouping key and one
COUNT(*) or COUNT(column), with an optional direct same-fact Boolean FILTER.
COUNT(column) covers BOOLEAN, INT2/4/8, FLOAT4/8, DATE, TIMESTAMP and TIMESTAMPTZ.
FALSE and NULL suppress the measure, not group creation. Zero-count groups,
NULL keys, counted-column NULLs and native H3 key-evaluation semantics remain
observable. Arbitrary FILTER expressions and general multi-measure hash
admission are not included.

The H3 parent transform preserves native permissive raw-bit behavior, including
zero at resolution zero. The canonical cell universe is an initial allocation
heuristic, not a cardinality bound. Typed capacity exhaustion triggers bounded
growth and then disjoint GPU prefix partitions plus a separate NULL partition.
Failed attempts publish no partial results. Successful leaves are retained in
a query-owned PostgreSQL tuplestore, which can spill to disk. Aggregation stays
on the GPU; this does not introduce CPU aggregation fallback.

Ownership and cancellation checks cover workspace replacement, retained masks,
fresh execution fingerprints, tuplestore lifecycle, PostgreSQL error boundaries
and cleanup. EXPLAIN reports actual attempts, scanned rows, overflows, completed
leaves and spill; these are scoped to the last completed execution on rescans,
not silently presented as whole-query totals.

## Verified production correctness

Release builds without `pg_test` passed on PostgreSQL 18 and 19. Both normal
suites cover all nine types, canonical and noncanonical keys, prepared-plan DML,
rollback/truncate/reload, cold unpinned loading, native error parity, budget
decline/recovery and actual device-floor boundaries. Separate test builds passed
19 focused H3 tests per major, including fault injection and actual rescans.

Both production majors also passed the real device-capacity test:

- Device single-call cap: 16,776,704 groups.
- Input and output: 16,776,706 groups, including NULL.
- Full native multiset comparison: zero differences.
- 23 aggregate attempts: 20 overflows and three completed leaves.
- 385,864,238 scanned rows; actual output spill to disk.
- 28 total GPU calls include five artifact-construction calls, not 28 aggregate
  attempts.

Real statement cancellation requested at 500 ms returned SQLSTATE 57014 after
532.613 ms on PG18 and 541.410 ms on PG19, with 19 and 17 actual GPU calls.
Both sessions then completed GPU/native full-parity recovery. Idle-normalized
ledger ownership returned exactly to its warmed baseline. Retained idle output
allocations remain charged; this is not a claim that cancellation emptied pools
or made the raw ledger zero.

The corrected full eight-filter integration suite passes on both majors.
Each worker owns 2,105,256 raw bytes, 1,052,628 derived bytes, 8,832 current idle
workspace bytes and 2,624 current idle output bytes: exactly 3,169,340 total.
Eight workers account for 25,354,720 bytes. The 25 MiB budget leaves 859,680
bytes, less than the ninth worker's raw allocation, and that worker is rejected.
Releasing one worker leaves exactly 22,185,380 bytes; all workers exiting leaves
the shared ledger at zero without restarting the postmaster. The earlier
11,456-byte mismatch was an omitted idle-pool expectation, not a tolerance
introduced into the test or a production-ledger policy change.

Final unit and native checks include 1,344 engine tests, 613 benchmark tests,
10,309,735 native hash checks, 1,176 H3 checks, 54,722 grouped checks, workspace
and host-API regressions, and five live OOM-invariant families. Strict PG18/19
engine and benchmark lint and formatting pass. Dedicated PostgreSQL C-error
tests verify original error preservation and exactly-once cleanup on both
majors; these fault-injection receipts are distinct from production builds.

## Warm resident performance, not first-use performance

Apple M2 Max, 12 CPU cores, 64 GB RAM, macOS 27.0.1; PostgreSQL 18 production
release. Ten workload lanes cover four unfiltered INT4/BOOLEAN lanes and six
filtered COUNT(*)/INT4/BOOLEAN lanes at parent resolutions zero and one. Each
lane uses 1,000,017 and 2,000,017 rows, five warmups and ten balanced AB/BA pairs.
Timing is raw wall clock, with actual parallel PostgreSQL as the baseline.
Planner cost multiplier is one and minimum batch size is 65,536. Full parity,
actual verified GPU execution and zero baseline GPU calls are checked.

Across all 20 cells, median warm speedups range from 5.995 to 16.752 times.
Six accelerator cells and three baseline cells have CV above 15%; maximum CVs
are 33.45% and 25.20%. All samples are retained. There was no outlier removal,
best-run selection or repeated searching for a favorable final measurement.

Refresh-inclusive execution loses in 19 of 20 cells. The sole win is unfiltered
BOOLEAN/resolution one at 1,000,017 rows: 69.35 ms versus 75.89 ms. Loading and
refresh costs are not hidden inside a warm-speedup claim. These runs do not
establish OS-cold performance, a general first-use win, or zero overhead versus
the previous implementation; no controlled old-versus-new comparison was run.
Cold-cache and broader release qualification remain open.

## Artifact identity and evidence boundaries

The measured PG18 module SHA256 is
`e43fa6f9328e09bd82d8e4c7b923cf0a2991b494baa1926b5019bdbfedb44680`;
the PG19 production module is
`d36d4cf7c280ddf0d6baac87697c5faa41d2ed586123e226d1b787c5d65a56bd`.
The measured release benchmark SHA256 is
`f55e18052d64295d5914788ca66e126d3db20943ab0dccc45097afc9f7ffdfb2`.
Per-run source, binary and harness seals, settings, plans, raw samples and
cleanup receipts are authoritative. Later test-only corrections require new
source manifests and verified binary comparisons, not relabeled old receipts.

Retained failures include the original accepted-zero incompatibility, genuine
Metal watchdog failures, provenance-preflight rejection and initial integration
fixtures that omitted current idle pool ownership. Earlier performance runs
belong to earlier source versions and are provisional, not final measurements.
The generated-shader investigation and successful repair are documented without
copying entire compiler caches or database directories.

The PG19 final integration server log is losslessly compressed as
`raw/repair-production19-integration-final2/server.log.gz`; its adjacent
`server-log-compression.json` records raw and compressed hashes and sizes.
The raw 153,032,991-byte log exceeded GitHub's file limit. It contains 1,000,054
installed-H3 deprecated-alias warnings from a correctness fixture, not a timed
benchmark. No lines were removed; the original raw log remains locally intact.

The static device-work/source-proof release gate remains open. Runtime parity
and GPU execution do not substitute for that proof. General joins, broad hash
operators, CUDA, hosted release qualification and the remaining TODO scope are
not declared complete by this milestone.

The fresh final source-only audit (`raw/repair-source-audit-final.json`) examines
133 exports: 122 pass and 11 remain unproved. Its 12 findings comprise four
grouped-aggregation exports, two join metadata queries, five join execution/
projection exports and missing release-object binding. Input manifests match
before and after the audit. This source-only invocation does not establish the
compiled release-object proof; its count must not be conflated with the older
hosted 131-export release report. No exemption or positive proof overlay was
added to make this result pass. The unfinished compiler-extractor work is not
part of this milestone's source changes.
