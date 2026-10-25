# Normal H3 column counts and bounded dense admission

This milestone enables ordinary planner selection for one canonical
`h3_cell_to_parent(cell, constant_resolution)` key and one unfiltered
`COUNT(column)`. Supported SQL inputs are BOOLEAN, INT2/4/8, FLOAT4/8, DATE,
TIMESTAMP and TIMESTAMPTZ. It preserves NULL-key and zero-count all-NULL groups.
COUNT(*) retains its stricter partition validation. FILTER/WHERE, arithmetic
measures, multiple hash measures/keys and general raw-key hash admission are
not enabled by this milestone. Hash execution remains one-shot.

## Results and limits

Production PostgreSQL 18 and 19 both pass normal-planner nine-type GPU dispatch
and full native-result parity. Prepared value/key updates, rows loaded inside a
rolled-back subtransaction, truncate/empty/reload, unsupported native declines,
impossible pinned-raw budgets and recovery also pass. At the observed H3 floor
of 84,210 rows, 84,209 remains native with the row-floor reason; 84,210 and
84,211 dispatch. These are actual backend limits, not universal thresholds.

Warm performance uses PostgreSQL 18 on an Apple M2 Max, the production module,
five warmups and ten balanced raw wall-clock pairs per cell. Both arms retain
PostgreSQL parallel query settings; baseline disables pg_accel before planning.
Every cell has full output parity, actual typed ParallelHash dispatch and
same-accelerated-backend default-setting observations. No samples are removed.

| COUNT input / parent resolution | 1,000,017 rows | 2,000,017 rows |
| --- | ---: | ---: |
| INT4 / 0 | 12.07x | 10.90x |
| INT4 / 1 | 10.87x | 7.81x |
| BOOLEAN / 0 | 11.72x | 10.55x |
| BOOLEAN / 1 | 8.67x | 8.13x |

These are **warm resident median speedups**, not unconditional query speedups.
The INT4/resolution-0 medians are 5.189/62.627 ms (GPU/PostgreSQL) at 1M and
8.267/90.139 ms at 2M. At 2M its refresh-inclusive measurement loses:
119.916 ms versus 83.923 ms; preload is separately 152.673 ms. All six
refresh-inclusive comparisons in the other three runs also lose. GPU CV exceeds
15% in INT4/resolution-1 at 1M (22.58%), BOOLEAN/resolution-0 at 2M (16.77%),
and BOOLEAN/resolution-1 at 2M (20.02%). The initial 378.32 ms warmup is retained.
First use/refresh is not OS-cold. No PostgreSQL 19 performance, hosted 1-CU,
CUDA or cold-cache performance claim is made.

## Implementation and regression boundaries

Planner hash memory now comes from metadata-only native layout queries and
authoritative loaded rows. Exact cached artifact identity/currentness is
required to reuse its current group capacity. Eviction or fused policy restores
the theoretical capacity. Runtime protected quotes include raw/artifact owners
once, reclaim outside leases, and revalidate before charged allocation. Device
artifact construction gets one pressure-only local idle-buffer recovery retry.

Focused tests exercise exact-fit/one-byte-under execution, zero dispatch and
non-learning on rejection, protected-owner accounting, cached-to-fused capacity
reset, stale catalog row estimates, malformed inspection evidence, and actual
nonzero cached/fused native quotes for all nine SQL types.

A separate narrow dense correction removes a stale total-query one-shot limit
only when the exact budget selector has already proved a valid bounded plan.
No-decision, star, row-floor, cost and other capability checks remain. A default
production 4M-row query executes five verified calls with full native parity on
this M2 Max. A native-layout unit test uses the actual 1-CU hardware profile to
check bounded admission and call costing; it is not 1-CU hardware execution.

The catalog fixture now derives its later GPU-case size from the same backend's
grouped row floor, with checked arithmetic and a separate 4M test-resource
ceiling. Initial 500K preload/spoof-native stages are unchanged. Fixture growth
and native expected results are transactional. Both final production majors
pass the unchanged lifecycle assertions, with added sizing/decline diagnostics.

## Verification and provenance

- Ordinary engine suite: 1,338 passing tests after the dense correction.
- Benchmark suite: 596 passing tests after all three observed failures were fixed.
- Focused live H3 suite: 12 passing tests on each major; other tests are filtered.
- Fixture Python contract suite: six passing tests.
- Strict all-target PG18/PG19 pg_test Clippy and benchmark Clippy pass with
  warnings denied; edition-2024 formatting passes. Final reruns retain 1,338
  ordinary engine and 596 benchmark passes.
- Final production suites: both majors, using `normal-production.sql` and the
  exact catalog integration harness; no pg_test force hooks.
- Source, native-boundary, resource-safety, benchmark, cleanup and scoped
  performance reviews found no remaining blocker. This is not a whole-project
  security certification or a rerun of every pgrx test.

PostgreSQL 18 production module SHA256:
`886b82953287f74a88ce3511a842250511f5e0db242a2fcb7746d929ddee8a00`.
The final extension rebuild is byte-identical to all four performance runs.
The production-build source manifests differ from the performance manifest only
in cfg(test) store lint cleanup. Subsequent benchmark-only lint cleanup changes
three helper visibility declarations inside private binary modules and test
syntax; it does not change the extension. Final benchmark receipts separately
record its verification rather than relabeling earlier source manifests.
The final release benchmark executable is also byte-identical to the measured
one: `5af629b67b605a0e55494cf3e8a0283c995a41f3985beeb3719c0cc9c2c11d5e`.
`final-qualified-source.sha256` records the final source after all lint cleanup.
PostgreSQL 19 production module SHA256:
`b62ec35d7372bc2806910e14e769e5dada128d1f657d7172e15e6e5cbac3c796`.
Final manifests identify the exact source and Cargo inputs. Each owned cluster
was stopped and checked for no surviving server PID; the existing PG17 service
and unrelated installations were not changed.

Each benchmark server log retains two H3 deprecation warnings. The normal
production logs retain AdaptiveCpp/LLVM SDK-version mismatch diagnostics
(26.5 versus 27.0), but no PostgreSQL WARNING/ERROR/FATAL/PANIC. These are
existing-toolchain observations, not suppressed or claimed fixed.

## Reading and reproducing this bundle

`production18-*` benchmark directories contain raw reports, plans, full-result
diffs, device/settings observations and identity/shutdown receipts.
`production18-normal-first` and `production19-normal-first` contain final
production correctness/lifecycle results. `production*-build*` directories
identify build commands, exact inputs and modules. The first benchmark directory
also retains `dense-four-million.log` and catalog results.

The copied wrappers are exact historical local invocations with explicit owned
prefixes, not a portable installer. Use their recorded command/environment and
the existing Rust benchmark harness to reproduce on a configured host. Original
absolute scratch paths are retained as provenance. This is a selected text
publication, not every file named in the original harness manifests: binaries,
PGDATA, JIT caches, duplicate report formats and downloaded CI archives are
excluded. The publication seal covers the files actually included here.

Historical failure logs are deliberately retained: initial compile/import
issues, old unit expectations, malformed test fixtures, three benchmark audit/
registry expectations and lint attempts. Later successful receipts supersede
them. Formatting initially used the wrong edition (2021); the corrected 2024
check is the relevant result. No failure is rewritten as success.

## Remaining gates

Prior CI run 39904698642 failed the fixed-size catalog check on both macOS
majors. Its artifacts report an Apple Paravirtual 1-CU GPU. The derived 2M
grouped floor and the stale 64K total-row cap explain incompatible admission
conditions, but that failing backend did not capture its exact rejection.
The new fixture/planner correction still needs a hosted rerun. Hosted Metal
also exceeded its six-hour job limit; logs alone do not establish a kernel hang.
The separate seven static release-proof findings remain open and unwaived.
This evidence does not complete all TODO work or establish green hosted CI.
