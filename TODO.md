# Remaining Work

This file records unfinished work after the Resident v2 rebuild. Completed
implementation history belongs in Git and `CHANGELOG.md`. A checked item below
is local evidence, not a substitute for hosted CI, independent hardware, or a
published release artifact.

## Workspace Proof Tooling

- [x] Add standalone compiler-resolved workspace address and explicit-control
  facts, with stable-base, initializer/update, metadata-use and dependency-ID
  checks. The separate control result rejects unsupported predicates, escapes
  and flow atomically; address-only behavior remains unchanged. The extracted
  five-file package passes 52 compiler-backed tests independently of pending
  engine changes; 56 focused tests pass in the accumulated tree, including four
  adjacent output-escape regressions. Current-source inspection counts 65
  address calls, but all five callers still decline the stronger qualification.
  Ordinary host effects, external aliases, bounds, device execution and exact
  build binding remain separate obligations. No SQL admission, release approval
  or waiver of the seven grouped/join execution findings is introduced.
  Evidence: `benchmarks/evidence/workspace-control-proof-20261012/README.md`.

- [x] Handle libc's explicit `nonnull` annotation only for the exact external
  builtin `memcmp` metadata contract, retaining strict general callable,
  object-origin and extent checks. Full redeclaration validation rejects later
  attributes and implementations, including function-try bodies. Integrated
  local suites pass 26 effects, 15 build-binding and 11 link-binding tests;
  old failures and compiler-valid regressions are preserved. At `e5de264f`,
  [Linux PG18](https://github.com/yocontra/pg_accel/actions/runs/38323607556/job/115057309352)
  passed analyzer fixtures, 1,311 standalone Rust tests and 588 harness tests;
  [Linux PG19](https://github.com/yocontra/pg_accel/actions/runs/38323607556/job/115057309344)
  passed 1,311 standalone Rust tests, with analyzer fixtures skipped. Full CI
  still failed twelve macOS live assertions, and release audit retained seven
  device/address proof findings; no release approval is claimed. Evidence:
  `benchmarks/evidence/metadata-memcmp-nonnull-20261012/README.md`.

- [x] Establish an isolated pinned-compiler exact-replay prerequisite: opt-in
  canonical 64-bit HCF IDs and deterministic deduplicated kernel-name ordering.
  A fresh 37-object plugin build passes eight tests without skips; seven whole
  object comparisons are byte-identical, including real join shared/static
  variants at O0 and O3/DNDEBUG. Native registration inspection covers O3;
  malformed/repeated IDs and actual 32-bit target explicit mode reject.
  Default mode and all twenty fixture names are retained. No production wiring,
  projection proof or closure of the seven findings is introduced. Evidence:
  `benchmarks/evidence/sscp-replay-controls-20261017/README.md`.

  - [x] Wire the committed replay patch into the production setup dispatcher
    after coverage, validating exact pinned/coverage/combined target bytes and
    rejecting partial or drifted states. Nine local pinned-fixture integration
    tests pass, including fresh/repeated application and coverage-only upgrade;
    original-dispatcher and missing-fixture negative controls fail as expected.
    Bash syntax and Ruff pass. This qualifies generic-backend source patch
    composition only, not an installed compiler build, projection device proof,
    CI or closure of the seven findings. Evidence:
    `benchmarks/evidence/bootstrap-replay-20261022/README.md`.

## Active OLAP And Geo Expansion

The September implementation plan expands the objective beyond the historical
1.0 decisions below. Checked evaluations and issue deferrals below do not prove
completion of this expansion. Retain this file and planning docs until the
complete objective is implemented and verified.

- [x] Repair direct Rust fetch error cleanup while preserving the guarded
  PostgreSQL callback. Tests require exact SQLSTATE/message, exactly-once
  executor release on direct and ExecScan paths, cursor-close safety, repeated
  cancellation with exact pool accounting/reclamation and GPU recovery.
  Capability tests now require exact native parity for supported shapes and
  stable declines for unsupported shapes; cross-column NUMERIC OR remains
  unsupported. Local PG18.4 and PG19beta1 each pass the full unfiltered
  1,839-test mixed Rust/live suite plus 55 integration tests, zero ignored or
  filtered. SQL-subtransaction error tests avoid native SPI snapshot warnings;
  an old-guard negative control still fails the independent eager-release check.
  Both final visible lifecycle runs prove Metal dispatch and have zero warning,
  error or fatal entries in their client/server logs.
  No planner admission expansion, upstream PostgreSQL regression-suite
  completion, performance or hosted-release qualification is claimed. Evidence:
  `benchmarks/evidence/scan-cleanup-20261017/README.md`.

  - [x] Remove the integer AVG test's forced cost and require its existing
    selection/dispatch/NULL/binary-parity assertions at transaction-local DEFAULT.
    The released-admission audit and new invented-exemption regression pass;
    all 590 local benchmark tests pass. Targeted live AVG passes on PG18.4 and
    PG19beta1 (one test each, not full-suite reruns); separate real Metal probes
    verify default cost 1 and two kernel calls without warnings/errors.
    No gate exception was added. Hosted CI, resident-join OOM accounting and
    SQL121 memory-budget failures remain separate. Evidence:
    `benchmarks/evidence/default-avg-20261018/README.md`.

  - [x] Distinguish resident-join COUNT OOM from benign interrupted retries in
    test-only status accounting, preserving production all-non-OK counters.
    Deterministic retry proves exact count, two attempts and one timeout; the
    old aggregate-counter assertion fails on that same successful retry.
    Zero/rows/MAX capacities retain exact results without fake capacity OOM;
    genuine Rust workspace OOM has zero native dispatch/failure increments and
    unchanged live ledger. Raw native OOM conversion remains a counted hard
    error (not a forced native allocator-failure test). PG18.4/PG19beta1 each
    pass two targeted live tests and one status unit; real Metal probes invoke
    both wrappers with 20,704 kernels and no warnings/errors. No full-suite,
    hosted-CI or SQL121 budget-resolution claim. Evidence:
    `benchmarks/evidence/join-status-20261018/README.md`.

- [x] Correct pinned AdaptiveCpp cache-path handling. Native observation,
  targeted clearing, artifact capture and diagnostics now use the actual
  `apps/<application>/jit-cache` layout with runtime default/legacy/token rules.
  Clearing protects home/base aliases and rejects ambiguous relative targets;
  owned fixture regressions preserve old failures. Fresh local Metal validation
  passes all 37 native CTests plus 14 cache and 22 artifact tests, including
  native/Python parity; CI also invokes the host filesystem regressions.
  Linux per-executable behavior has macOS host algorithm coverage, not live
  Linux qualification. The recursive `resident_concurrency` fingerprint is
  unchanged. Seven grouped/join release-proof findings remain open; no new SQL,
  performance or cold-latency qualification is claimed. Evidence:
  `benchmarks/evidence/cache-resolution-20261012/README.md`.

- [ ] Generalize exact aggregation: multiple keys/measures, numeric/dictionary
  inputs, arithmetic/Boolean/IN/CASE/date expressions, FILTER, and parallel
  dense/hash partial aggregation. Support native HAVING/order/limits.
  - [x] Native HAVING over already-projected nonvolatile aggregate/group inputs:
    SQL105 passes on PG18/19, including real dispatch, NULL/empty results,
    parameters, DML/rollback and SQLSTATE recovery. Both complete local SQL
    suites pass 68/68 files. `grouped_int4_native_having` passes exact output
    comparison and the 1.15x warm floor at 250K and 1M: latest medians are
    2.55/8.27 ms (3.25x) and 5.27/18.58 ms (3.52x), respectively, against
    PostgreSQL with parallel workers enabled. Ten paired measurements per
    scale, explicit native CPU-filter reporting, independent output oracles,
    and raw first-use/lifecycle costs are retained in
    `.codex/scratch/having-warm-boundary-20260921`. This is dirty-tree local
    diagnostic evidence, not a release certification. Preload costs of
    42.87/81.47 ms are excluded from those warm medians; first use still loses.
  - [x] Separate internal scan and SQL result projections: materialize hidden
    HAVING aggregates and grouping/sort keys, normalize value/count layout,
    and let PostgreSQL filter/project the results. Qualified kernel gates
    still apply to the complete internal shape; unsupported states and
    volatile predicates remain native. SQL105 now covers hidden inputs,
    reordered outputs, no-qual projection, generic parameters and rollback.
    Complete PG18/19 SQL suites pass 68/68 files, with 366 declared assertions;
    artifacts are `.codex/scratch/hidden-having-pg18-20260921` and the matching
    `pg19` directory. The distinct `grouped_int4_hidden_having` benchmark
    verifies exact outputs and real dispatch in ten paired warm measurements
    per scale: 2.28/8.33 ms at 250K (3.65x) and 5.11/18.71 ms at 1M (3.66x).
    Preload costs of 39.74/84.21 ms are separate; first use still loses.
    `.codex/scratch/hidden-having-warm-20260921-b` retains this local diagnostic
    evidence, not release certification. The initial unsuffixed attempt was
    rejected before measurement because the local artifact was built for PG19
    rather than the PG18 benchmark backend; no timings were discarded.
  - [x] Make exact bool-keyed int2/int4 SUM and AVG independently selectable,
    with optional COUNT(*), while preserving widened sums, NUMERIC AVG and
    all-NULL group activity. SQL107 covers all 12 type/output combinations,
    generic prepared plans, hidden keys, HAVING, updates, rollback and empty
    inputs; SQL104 retains adjacent unqualified MIN-state decline checks.
    Full PG18/19 SQL suites pass 70/70 files (372 declared assertions), retained
    in `.codex/scratch/optional-aggregates-pg18-20260921` and the matching `pg19`
    directory. Rust extension tests pass 1089, benchmark tests 562, and native
    grouped-aggregate tests pass 5159 checks, including optional-lane atomic,
    hierarchical and chunked/reset execution. The new `grouped_int4_avg_only`
    benchmark passes exact comparison, independent NUMERIC output oracles,
    real dispatch and the unchanged 1.15x floor over ten paired warm timings:
    2.19/7.97 ms at 250K (3.64x) and 5.80/17.75 ms at 1M (3.06x).
    Preload costs of 41.26/83.29 ms are excluded; first use still loses.
    `.codex/scratch/optional-aggregates-avg-warm-20260921` retains these
    dirty-tree local Metal diagnostics, not release or CUDA qualification.
  - [x] Extend the exact int2/int4 SUM/AVG family to one, two or three supported
    fact-column dictionary keys, including mixed text/date/bool dimensions and
    a measure that is itself a grouping key. SQL108 checks every supported
    primitive key type, NaN/signed zero, bpchar, NULLs, prepared hidden keys,
    HAVING/order/limit, dictionary-changing DML and rollback. Dense capacity
    remains bounded by the product of dictionary domains; this does not
    complete high-cardinality or multiple-measure aggregation. Complete PG18/19
    SQL suites pass 71/71 files (374 assertions), retained in
    `.codex/scratch/multikey-pg18-20260921` and the matching `pg19` directory.
    SQL108 was repeated on both installed versions after the equivalent
    Clippy-driven predicate refactor (`multikey-sql108-pg{18,19}-20260921-b.log`).
    The new `grouped_multikey_integer` sales-rollup benchmark checks all 225
    results against both PostgreSQL and an independent Rust-derived oracle.
    Ten paired warm observations pass real dispatch and the unchanged 1.15x
    floor: 4.48/10.89 ms at 250K (2.43x), 6.77/28.07 ms at 1M (4.15x).
    Preloads of 69.21/195.66 ms and first-use lifecycle costs of 73.23/275.86 ms
    are separate; first use still loses. Local dirty-tree Metal evidence is
    `.codex/scratch/multikey-warm-20260921-b`, not release/CUDA qualification.
    The unsuffixed attempt failed before timing because its independent
    oracle used an unsupported PostgreSQL FULL JOIN condition. That failed
    artifact remains; the oracle now uses an exact nullable composite JSONB
    key, and correctness-error artifacts retain server error details.
  - [x] Compute exact SUM/NUMERIC AVG across up to four independent int2/int4
    fact columns (three with optional COUNT(*)), including mixed input widths
    and independent NULL patterns. The planner and physical descriptor mirror
    admit the new `dense_multi_integer_sum_avg_plain` specialization; shared
    parallel kernels retain separate states and count selected rows only once.
    Workgroups partition by measure/chunk without multiplying local scratch,
    and bounded execution sizes chunks for all states within the 32 MiB
    partial budget. Filters/joins and other measure types remain unfinished.
    SQL110 covers reordered/hidden outputs, four-state HAVING, prepared plans,
    DML/rollback, all-NULL/empty input, and a 1,000,017-row / 1024-group bounded
    execution. The latter matches PostgreSQL with multiple verified parallel
    calls; it does not prove spill or inputs beyond resident memory.
    Final PG18/19 suites pass 73/73 files (379 assertions), retained in
    `.codex/scratch/multimeasure-pg{18,19}-20260921-c`. Rust extension tests pass
    1096, benchmark tests 564, native grouped tests 6481 checks, and full native
    CTest passes 32/32. Native tests include later-lane failure without partial
    publication, poisoned-session recovery, atomic/hierarchical/chunked paths,
    independent NULL counts and integer endpoints. Both Clippy versions pass.
    The new `grouped_multimeasure_integer` sales-rollup benchmark verifies all
    225 results against PostgreSQL and an independent three-state Rust oracle.
    Ten paired warm observations pass dispatch and the unchanged 1.15x floor:
    6.19/12.31 ms at 250K (1.99x), 7.43/32.99 ms at 1M (4.44x), retained in
    `.codex/scratch/multimeasure-warm-20260921`. Preloads of 66.46/157.32 ms and
    first-use lifecycle costs of 47.71/158.15 ms are separate; first use still
    loses. The existing one-measure control still passes at 2.50x/5.45x in
    `.codex/scratch/multimeasure-single-control-warm-20260921`. All timings are
    dirty-tree local Metal diagnostics, not release/CUDA qualification.
    Earlier SQL110 attempts are retained: one was interrupted by an extension
    restart, and another incorrectly expected above-one-shot queries to remain
    native. The measured exact bounded dispatch showed that assumption was
    wrong; the final test requires parallel multi-batch execution instead.
  - [x] Add independent int4 interval FILTER predicates to the exact int2/int4
    SUM/AVG family, including equality, one-sided and strict comparisons across
    one to three dictionary keys. A predicate may use another raw int4 column
    already represented by a SUM/AVG state. Predicate NULLs and value NULLs are
    independent; rejected aggregate rows do not remove groups or change an
    unfiltered COUNT(*). The early shape builder, runtime capability backstop
    and native descriptor mirror all enforce the binding/type contract.
    Filtered COUNT, unbound predicate columns, general Boolean expressions,
    query-level filters and joins remain outside this specialization.
    SQL111 covers same/cross-column filters, signed endpoints, all-rejected
    groups, hidden HAVING states, generic prepared plans, DML/rollback and
    empty/all-NULL input. SQL98 retains its original selected bounded-filter
    test and replaces the now-supported one-sided negative case with an
    unsupported disjunction. Full PG18/19 suites pass 74/74 files (381 declared
    assertions), retained in `.codex/scratch/multifilter-pg{18,19}-20260921`.
    Rust tests pass 1097, benchmark tests 565, native grouped tests 6691 checks,
    full native CTest 32/32, and both Clippy versions pass. Native tests also
    check independent NULLs, multi-interval descriptor evaluation, atomic and
    chunked/hierarchical execution, and no publication after predicate failure.
    The new `grouped_filtered_integer` sales/returns rollup verifies all 225
    results against PostgreSQL and an independent three-state Rust oracle.
    Ten paired warm observations pass exact output, verified parallel dispatch,
    zero stock fallback and the unchanged 1.15x floor: 6.94/12.54 ms at 250K
    (1.81x), 7.46/33.53 ms at 1M (4.50x). Artifacts are retained in
    `.codex/scratch/multifilter-warm-20260921`. Preloads of 59.94/147.72 ms and
    first-use lifecycle costs of 44.95/146.14 ms are separate; first use loses.
    The existing bounded-filter control still passes at 2.24x/7.16x in
    `.codex/scratch/multifilter-legacy-control-warm-20260921`. These are local
    dirty-tree Metal diagnostics, not release/CUDA qualification. The initial
    focused SQL failure is retained: it exposed an earlier shape-builder gate
    that still rejected the expanded filters before runtime admission; that
    gate now uses the same qualified predicate contract.
  - [x] Lower single-column INT4 AND/OR/NOT and constant IN/NOT IN/ANY/ALL
    aggregate predicates into the existing parallel SUM/AVG interval lane.
    TRUE and FALSE sets are tracked independently so UNKNOWN is preserved
    through negation, including NULL array members. Duplicate, overlapping
    and adjacent ranges merge before enforcing the frozen four-interval ABI
    capacity. Planning is bounded to 64 predicate nodes and 256 array members;
    no row filtering or aggregation moves to the CPU. Existing per-measure
    input bindings, exact integer/NUMERIC output semantics and kernel gates
    remain in force. NULL-accepting, always-rejected, mixed-column, dynamic and
    over-capacity predicates stay native rather than being approximated.
    SQL111 adds 19 selected differential cases covering endpoints, reversed
    comparisons, array lower bounds, NULLs, IN/NOT IN, ANY/ALL and compound
    predicates; nine adjacent unsupported shapes must remain native. Prepared
    hidden-HAVING tests now use compound predicates across DML, rollback,
    all-NULL and empty inputs. SQL98's former one-column OR negative case now
    uses a genuinely unsupported mixed-column OR. Standalone Rust tests pass
    1110, benchmark tests pass 568, both Clippy builds and doc/coverage/safety
    audits pass. Source coverage includes the new pure interval module and
    the SQL assertion inventory grows to 389 without lowering any floor.
    Logs are `.codex/scratch/boolean-filter-*-20260921*`. The distinct
    `grouped_boolean_filtered_integer` workload adds an independent Rust
    oracle for three overlapping/NULL-bearing conditional states and all
    225 text/date/bool groups; its performance evidence is recorded below.
    Both installed PostgreSQL versions pass all 77 SQL files (389 assertions)
    in `.codex/scratch/boolean-filter-pg{18,19}-20260921`.
    PG18 module SHA-256 is
    `0127ad2615d620c1447dfa2ae2f38123aef13b747a247f8bf22baf9c369485d2`;
    PG19 is `56faae27b78acb1c6e9b81789ab4b8075b2183579b68905616b7e195e4811b21`.
    Ten paired warm observations in
    `.codex/scratch/boolean-filter-warm-20260921` pass exact PostgreSQL and
    independent-oracle comparison, verified parallel device mode, zero stock
    fallback and the unchanged 1.15x floor. Initial medians are 7.40/15.06 ms
    at 250K (2.04x) and 7.54/42.29 ms at 1M (5.61x). GPU variation is 33%/21%,
    so these local dirty-tree synthetic results are not stable release claims.
    Separate preload costs 60.71/148.60 ms; lifecycle first use is
    44.77/145.37 ms versus native 14.84/43.48 ms and still loses. The existing
    simpler-filter control also passes its gates at 1.66x/4.41x in
    `.codex/scratch/boolean-filter-control-warm-20260921`. Neither corpus
    substitutes for exact-NUMERIC real taxi fare rollups, still unfinished.
    The repeat `.codex/scratch/boolean-filter-warm-20260921-b` also passes
    exact outputs, provenance, parallel dispatch, zero fallback and the warm
    floor: 7.12/14.94 ms at 250K (2.10x), 8.88/41.88 ms at 1M (4.72x).
    GPU variation remains 29%/21%; both runs are retained without claiming a
    stable headline. Repeat preload is 57.89/150.32 ms and first use remains
    slower at 45.38/143.59 ms versus native 14.98/46.28 ms. CUDA, concurrency
    and release qualification remain open.
    Fresh upstream PostgreSQL regression/isolation schedules pass in all
    eight pristine/loaded PG18/19 runs (1448 scheduled executions) in
    `.codex/scratch/upstream-boolean-filter-pg{18,19}-20260921`; both artifact
    seals verify. This is hook/default-path compatibility on the local
    debug/assertion, no-ICU builds, not GPU execution of the upstream corpus
    or selected PostGIS/H3 SQL-object coverage in those databases.
  - [x] Add lossless NUMERIC input residency: native-endian base-10000 digits
    with per-row weight, display scale, sign and separate NULL state. Values
    are not narrowed to int64/int128 or floating point, and no common column
    scale is imposed. PostgreSQL numeric_send supplies the exact binary input;
    temporary detoast/send allocations are released. Row/digit/NULL buffers
    carry exact ledger charges with no duplicate retained host representation.
    Invalid input cannot publish a partial row. The existing query admission
    still declines NUMERIC aggregation and grouping. The native device
    accumulator milestone is recorded below; SQL result materialization
    and admission remain unfinished.
    Pure tests pass 1115; both Clippy versions and 42 doc/coverage tests pass.
    Two live tests pass on each of PG18/19, comparing numeric_send bytes to
    reconstructed GPU-buffer contents at full PostgreSQL precision/scale,
    including external/compressed TOAST, negative-scale typmods, specials,
    DML/rollback refresh, all-NULL and empty input. The initial live failure
    exposed infinity's wire display-scale field of 32; those bits are now
    preserved, not mistaken for a finite scale or rejected. Evidence is
    `.codex/scratch/numeric-residency-*-20260921*`. SQL116 adds exact byte-charge
    and lifecycle parity checks; inventory is 79 files / 393 assertions,
    with the new production source included and no threshold reduction.
    The initial PG19 broad run passed 78 files but SQL115 hit Metal's
    ImpactingInteractivity watchdog during serial integer commit; its focused
    retry passed unchanged. That exposed a redundant full partial-arena
    recount after parallel reduction. The reducer now stores each group's
    checked batch row delta in its first consumed partial, and commit scans
    groups only. Optional COUNT(*) still accumulates independently; errors
    still suppress publication. No extra allocation, launch or CPU reduction
    was added. Expanded native coverage includes 1024 groups and repeated
    chunks with/without COUNT(*): 22390 grouped checks and all 32 CTests pass.
    Final-candidate PG18/19 SQL suites pass 79/79 files each in
    `.codex/scratch/numeric-residency-pg{18,19}-20260921-b`. Module SHA-256 is
    `f7709d842e5d8ebd51c47cc031eaae6e9a25700a46b132ba896852d6e9c3bfc3` (PG18)
    and `e9fb95f87d1ce1ca24881aaec02ce58d30575dac44fb860bb2eec5c6e6f2ba08` (PG19).
    Fresh upstream pristine/loaded regression and isolation schedules pass
    all eight runs (1448 scheduled executions) and both checksum seals verify
    in `.codex/scratch/upstream-numeric-residency-pg{18,19}-20260921-b`.
    These remain local hook/default-path, debug/assertion, no-ICU proofs,
    not GPU execution of upstream tests or release/CUDA qualification.
    The fresh 123-export GPU-execution/ABI audit passes. Full NYC January 2015
    fare_amount and trip_distance residency now loads all 12,748,986 rows
    without narrowing either NUMERIC column: 489,354,918 device bytes, checked
    against both complete numeric_send payloads and NULL sidecar charges.
    Local load time is 3763.84 ms, separately from the 1698.96 ms full temporary
    projection and 776.38 ms ANALYZE. The temporary projection was dropped
    after unpin/eviction; the original corpus is unchanged. Exact readback
    proofs are the live fixtures above; this full-corpus run proves loading
    and accounting, not GPU NUMERIC aggregation or a query speedup. Script and
    raw timings are `.codex/scratch/numeric-residency-nyc-20260921.{sql,log}`.
    Existing conditional-integer rollups still pass exact outputs, provenance,
    independent oracle, verified parallel execution, zero fallback and the
    unchanged warm floor over ten paired observations: 7.75/15.01 ms at 250K
    (1.94x), 7.58/41.43 ms at 1M (5.46x), in
    `.codex/scratch/numeric-residency-control-warm-20260921`. GPU variation is
    28%/22%; these remain local synthetic diagnostics, not stable headlines.
  - [x] Implement the opt-in native exact NUMERIC SUM/count kernel foundation.
    A host-readable metadata view points to resident base-10000 row/digit/NULL
    buffers; only GPU code consumes those payloads or performs aggregation.
    Dense/global aggregation supports 1–4 decimal states, optional final
    COUNT(*), exact signed sharded accumulation, carry normalization, finite
    display scales, NaN/infinities and cancellation before final overflow
    checks. Existing descriptor layouts/exports and legacy NUMERIC decline
    behavior are unchanged. Per-launch digit work is bounded; full lifecycle
    row limits prove integer bucket arithmetic cannot overflow. Workspace
    fingerprinting covers digit extents; invalid rows/final overflow suppress
    aggregate publication and failed workspaces require RESET.
    Native tests pass 23043 checks and full CTest passes 32/32, including
    maximum PostgreSQL precision/scale, a million-row independent oracle,
    multiple states, chunk lifecycle equivalence, stale extents, malformed
    inputs, cancellation, final overflow and special-value dominance.
    Live tests on both PG18/19 feed actual resident device buffers directly
    through the native ABI and compare SUM's numeric_send bytes and non-NULL
    counts against PostgreSQL for 100017 mixed-scale rows and edge cases.
    Rust layout/bounds tests pass with 1116 total standalone tests. Evidence:
    `.codex/scratch/numeric-aggregate-*-20260921*`. Both Clippy builds,
    doc parity, safety/coverage audits (154 coverage-tool tests), and the
    fresh rebuilt-library GPU-execution/ABI audit pass (123 exports,
    593 local definitions, 36 lifecycle contracts). Release modules are
    restored and both full SQL suites pass 79/79 files, in
    `.codex/scratch/numeric-aggregate-pg{18,19}-20260921`. Installed SHA-256:
    `23396454c0baa3eecda6b4507bffe6a031643a9d45c2fd9c0c157c9f4fca3fe8` (PG18),
    `2db8bfb0847a0302990774cdc572e3bbed13cbf43fdbd05fd35c7bcd6a2f0af2` (PG19).
    This is NOT SQL planner admission or a speedup claim. NUMERIC executor
    result materialization, SUM/AVG shared-state admission, costs, real NYC
    query timings, FILTER/expressions, mixed measure types, hash/compact
    grouping and CUDA qualification remain open. PostgreSQL's own AVG first
    materializes its SUM and therefore also errors on an out-of-range final
    sum; preserve that behavior when adding PostgreSQL division finalization.
  - [x] Connect the exact NUMERIC kernel to the production Rust lifecycle and
    result-materialization components. Plans/chunks/sessions own pinned
    metadata snapshots; chunks advance row headers and NULLs without shifting
    the shared digit arena. RESET/FINALIZE receive zero-row metadata. Shape
    and workspace-pool identity include digit bounds, even when state widths
    coincide. The Rust wrapper recognizes parallel_dense_numeric and exposes
    its physical-mode counter. Result accessors validate header/extent/digits,
    canonical zero/special values and padding before transporting the reduced
    state through PostgreSQL numeric_recv. SUM returns the resulting Datum
    directly; AVG uses numeric_div and releases its temporary input Datums.
    No float/text conversion or private Numeric representation is involved.
    Live PG18/19 tests now use the production wrapper and finalizer, comparing
    one-shot versus 4093-row chunks, fresh plan metadata owners, empty tail
    chunks, RESET followed by empty finalization, and exact SUM/AVG result
    bytes/SQLSTATE. In particular, PostgreSQL's division rounding can overflow
    AVG of a maximum-precision value even when SUM fits; the same 22003 is
    preserved, not silently converted to success. The initial test failure and
    successful follow-ups are retained under
    `.codex/scratch/numeric-integration-*-20260921*`. Standalone tests pass
    1119; both Clippy versions and the coverage/safety audits pass.
    All three live NUMERIC tests pass on each supported PostgreSQL version;
    the fresh-owner/empty-tail extension also passes on both. Restored release
    modules pass both complete SQL suites, 79/79 files, in
    `.codex/scratch/numeric-integration-pg{18,19}-20260921`. Installed SHA-256:
    `24c1b108e3c22be0300ecb5c8177d2874b84a39eeb2761e7390c61068d38cecc` (PG18),
    `673858d638546511dece3af7ac9e6cd79753315f271c4a87e9f5fdd7e573484b` (PG19).
    SQL descriptor binding, logical projection/catalog admission, workspace
    memory/cost gates and measured NYC query qualification remain unfinished;
    this component milestone does not make NUMERIC SQL planner-selectable.
  - [x] Bind NUMERIC resident columns into production aggregate descriptors,
    including owned metadata, extent-derived state widths and canonical empty
    inputs. Runtime shape checks admit global or one-to-three fact-key dense
    grouping with up to four NUMERIC SUM/AVG states and shared COUNT, optionally
    ending in COUNT(*). COUNT-only NUMERIC, mixed integer states, joins, FILTER
    and numeric grouping remain declined. Output projection validates native
    NUMERIC SUM/AVG and int8 COUNT result types. A live test bypasses only the
    planner gate and runs the production catalog checks, dictionary artifacts,
    repeated residency binding, 4093-row bounded executor and tuple materializer.
    PG18/19 pass byte-exact comparisons for 100017 mixed-scale rows, all 0–3 key
    and 1–4 value-state combinations, NULLs, specials, full-exponent cancellation
    and empty global/grouped inputs. Evidence:
    `.codex/scratch/numeric-binding-*-20260922*`.
    Standalone tests pass 1122; all four live NUMERIC tests pass on both PG18/19;
    both production lint builds, PG18 pg_test lint, doc parity and coverage/
    safety audits pass. Restored release modules pass both complete SQL suites
    (79/79 each), with artifacts in
    `.codex/scratch/numeric-binding-pg{18,19}-20260922`. Installed SHA-256:
    `e5dba3d01c4c740cc39f65651de2592da6b7f18addc928eda52604b77ca2696c` (PG18),
    `2c7c28dd42eff283a1ae7cecf602a713def44130be6d0e6eec04bf6f1585ad1c` (PG19).
    This is runtime integration, NOT normal SQL admission or a speedup claim.
    Parser admission, exact workspace-budget/digit-work cost gates and measured
    NYC rollup qualification remain required before normal NUMERIC promotion.
  - [x] Enforce reserve-before-allocation budgeting for NUMERIC execution
    scratch and host results. Native workspace requirements and checked output
    footprints (all value states, counters and active bitmap) determine charges.
    Charges follow buffers through pool checkout/return, so reuse neither loses
    nor duplicates accounting. Failed reservations reclaim only idle pooled
    allocations before retrying; active results and resident inputs remain
    owned. Cleanup and allocation-failure paths release charges; copied foreign-
    PID owners cannot debit another process's reservation. Planner snapshots
    reconcile store plus executor ownership and include retained execution bytes
    in the non-reclaimable footprint and budget-decline diagnostics.
    Unit tests cover exact budget boundaries, failed reservations, ownership
    moves, simulated foreign-PID drops, every output lane's exact footprint,
    size overflow and planner budget rejection. Five NUMERIC live tests pass on
    each of PG18/19, including pooled reuse, pre-allocation denial, zero-budget
    failure, idle reclamation and active-result preservation. Evidence:
    `.codex/scratch/numeric-budget-*-20260922*`. pg_test uses a process-local
    ledger; this is not proof of cross-backend NUMERIC concurrency. Prospective
    NUMERIC workspace estimation, resident digit-work costing, parser admission
    and measured NYC SQL acceleration remain open; this milestone does not
    promote NUMERIC into normal planning or budget every other executor family.
    Verification: 1126 standalone tests, both production Clippy builds, PG18
    pg_test Clippy, doc parity and coverage/safety audits pass. Restored release
    modules pass both complete SQL suites (79/79 each), with artifacts in
    `.codex/scratch/numeric-budget-pg{18,19}-20260922`. Installed SHA-256:
    `91e75d7d8dd3dc9c06cc964f349f2cc2cddb3e24b4ea2f5751e8f0a08fe38d3a` (PG18),
    `5db057bbb479387b4705fdced12caa7f657754b70c3267715e6efa7964194005` (PG19).
    The production shared-ledger integration harness also passes on both
    versions (`numeric-budget-ledger{18,19}-20260922.log`): separate databases
    each charge 4,000,000 bytes and backend cleanup returns the ledger to zero.
    That is regression evidence for the existing real-backend ledger, not a
    substitute for future cross-backend NUMERIC execution qualification.
  - [x] Qualify uniform grouped execution budgeting on GPU and live PG18/19.
    All Rust grouped workspace and output allocations now reserve the execution
    ledger, including ordinary dense integer states that previously bypassed
    the NUMERIC/scalar/hash-only charge predicate. Dense/spatial one-shot
    execution checks the combined scratch/result footprint before allocation;
    bounded session setup includes the eventual output before any accumulation
    and accounts for already-live metadata snapshots. Read-only forecasts credit
    only charged idle pools; per-allocation reservations remain authoritative.
    New host tests cover all output-lane footprints, dense/compact layouts,
    one/four states, exact-fit and zero-space budgets, pre-device rejection,
    pooled reuse/reclamation and active-owner preservation. The live fixture
    additionally exercises actual workspace allocation, empty-input finalization
    and warm pool reuse; it was not run at the original checkpoint. Both PG18/19
    standalone suites pass 1,299 tests; production and pg_test strict lint pass
    on both.
    Documentation (21 tests), safety, scope and formatting checks pass, with
    the unchanged coverage floor. Existing normal SQL
    admission is unchanged; no fallback or performance claim is added. Evidence:
    `benchmarks/evidence/dense-memory-accounting-20261010/README.md`.
    Live qualification on 2026-10-12: the exact
    `pg_dense_integer_execution_budget_and_bounded_pool_ownership` fixture passes
    on PostgreSQL 18.4 and 19beta1 (1/1 each, no skips) with M2 Max/Metal. It
    covers real device allocations, combined preallocation denial, cold/warm
    pooled ownership, zero-row finalization/cleanup and zero-budget rejection.
    Sealed evidence: `benchmarks/evidence/uniform-budget-live-20261012/README.md`,
    complementing the separate full native 36/36 bundle. This is a process-local
    pg_test ledger proof, not cross-backend shared-ledger or cold-latency
    qualification; production SQL and static device proofs remain separate.
  - [x] Add prospective NUMERIC workspace/result budgeting and resident decimal
    work estimates. Generation-checked metadata retains exact digit counts,
    extents, maximum digits per row and selected NULL sidecars without device
    readback. A zero-row descriptor queries the execution layout without buffer
    allocation, ledger reservation, pool checkout or GPU submission. Live tests
    compare forecasts with actual scratch/result allocation deltas, including
    nullable grouping-key staging. Dense capacity multiplies individual
    PostgreSQL key estimates plus resident NULL codes: correlated keys with 11
    output groups and 110 input rows correctly forecast 1331 slots, not 11 or
    110. Dense capacity must also fit the configured dictionary limit. Overflow,
    missing/stale metadata and invalid layouts decline safely.
    Costing counts shared-state rows/digits, scratch work and bounded digit/
    chunk launches. Prospective bytes join raw/derived/retained-pool charges;
    counting fresh buffers alongside retained pools is deliberately conservative.
    Verification: 1132 standalone tests and six live NUMERIC tests on each of
    PG18/19 pass, as do both production Clippy builds, PG18 pg_test Clippy,
    coverage/safety audits and doc parity. Evidence:
    `.codex/scratch/numeric-estimate-*-20260922*`.
    Release modules pass both full SQL suites (79/79 each), with artifacts in
    `.codex/scratch/numeric-estimate-pg{18,19}-20260922`. After adding the final
    dense-capacity guard, unit/Clippy checks pass again and restored release
    modules pass SQL 116 on each major; full suites preceded that final guard.
    Final installed SHA-256:
    `10e89c833b63fc583540db58f4cb94ecb23e798ce5c9f845e9cb9d5d52adb530` (PG18),
    `c3101d67339d20e5780cb3a940c4c4ce97e190d6cd81d39d0450d5e467b5c9a0` (PG19).
    At that milestone only the estimate plumbing was closed; normal SQL
    admission and decimal cost calibration were still pending (completed below).
  - [x] Enable ordinary planner admission for exact resident NUMERIC SUM/AVG,
    shared COUNT(value), and optional COUNT(*), globally and with up to three
    dictionary keys. Preserve target ordering when coalescing states; keep
    unsupported count-only/mixed/filtered/joined decimal shapes native. Use
    exact resident row/digit/memory evidence, the grouped row floor even for
    global decimal queries, and reduction—not hash-probe—cost for dense decimal
    work. No forced-cost override is present in production benchmark runs.
    SQL 117 verifies 16 normal aggregate shapes and prepared HAVING/ORDER/LIMIT
    through changed extents, rollback, NULL and empty input on PG18/19.
    Production shared-ledger tests additionally prove NUMERIC buffer reuse and
    exact cleanup across two live owners, including forced backend exit.
    Benchmark telemetry now records NUMERIC mode counts through sample deltas,
    accumulation, cache-mode merging and report verification; counter regression,
    overflow and legacy-report decoding are tested.
    Fresh verification: 1135 core tests, 571 benchmark tests, seven live tests
    per major, 32 native CTests, both core Clippy builds, benchmark Clippy,
    coverage/safety and documentation checks pass. Both full SQL runs were
    78/80 initially because cases 03/104 exceeded the unchanged dispatch-warning
    threshold despite passing correctness; both cases passed warning-free on
    retry on each major. Do not label these uninterrupted 80/80 runs.
    Durable evidence and full reports:
    `benchmarks/evidence/numeric-20260929/README.md`.
    Final warm PG18/M2 Max medians show 9.34x for the complete 12,748,986-row
    taxi fare/distance rollup, 7.81x for the million-row three-key decimal case,
    4.43x for wide scales and 2.49x for global SUM/AVG/counts. All eight measured
    cells match exactly, verify NUMERIC dispatch and have zero executor fallback.
    The earlier taxi run was 8.41x; absolute timings varied substantially with
    shared-host load. Loading is separate (7.31 seconds for the final full month),
    so these are local warm-resident characterizations, not cold-query or
    release-headline claims. Concurrent throughput, cross-backend budget pressure,
    broader decimal shapes and CUDA qualification remain separate work.
  - [x] Fuse independent INT4 aggregate FILTER inputs into exact NUMERIC
    SUM/AVG and shared COUNT(value), globally and with up to three dictionary
    keys. Up to four normalized ranges bind through each decimal state's typed
    RHS without extra value states, masks or launches. Rejected rows still
    establish groups and contribute to unfiltered COUNT(*); rejected decimal
    specials and overflowing sums are never accumulated. Singleton ranges use
    a host-specialized equality path, with one comparison charged instead of
    two; interval comparisons and all input digit work remain conservatively
    costed using unchanged coefficients. No forced planner override is used.
    SQL 117 now verifies 28 ordinary-planner shapes, including cross-chunk
    predicate inputs, shared states, hidden HAVING, ordering and limits. The
    NUMERIC and adjacent integer FILTER contracts pass warning-free on PG18/19.
    Eight live NUMERIC tests per major cover NULLs, excluded specials/overflow,
    DML/rollback and empty inputs. Final equality-specialized code passes all
    1136 standalone tests, 32 native CTests and both production Clippy builds;
    native tests include mixed singleton/interval predicates at INT32 limits.
    The matched filtered benchmark verifies exact results, real NUMERIC
    dispatch and zero fallback: warm medians are 3.09/11.53 ms at 250K (3.73x)
    and 5.38/30.23 ms at 1M (5.62x) against parallel PostgreSQL. Loading costs
    116.85/367.80 ms separately; this is not a first-use or release headline.
    Initial native-only measurements are retained without GPU speedup credit.
    Durable reports, controls, provenance and verification logs:
    `benchmarks/evidence/numeric-filter-20260929/README.md`.
    COUNT-only decimal states, mixed integer/decimal states, decimal-input or
    multi-column predicates, NULL-accepting predicates, filtered COUNT(*) and
    query-level WHERE remain outside this milestone. Broader aggregation and
    CUDA/concurrent-throughput qualification remain open.
  - [x] Admit independent COUNT(numeric) states, including independent INT4
    FILTER predicates and composition with exact SUM/AVG, globally or with up
    to three dictionary keys. Count-only lanes read NULL status without digit
    loads, SUM arithmetic, digit buckets or SUM output; NaN/infinities count
    correctly and an unused SUM cannot introduce overflow. Count state width
    is eight bytes and workspace does not grow with digit extent. Costing
    excludes unused digit work without changing coefficients or forcing paths.
    Production SQL 117 now checks 48 normally selected shapes, and adjacent
    integer contracts 111/115 pass warning-free on PG18/19. All 1137 core,
    571 benchmark, 32 native and eight live NUMERIC tests per major pass,
    alongside both production Clippy builds and coverage/safety/doc checks.
    Matched warm-resident million-row medians are 10.88/16.72 ms for global
    counts (1.54x), 2.87/36.95 ms for three-key conditional counts (12.89x),
    and 5.82/30.78 ms for SUM plus independent counts (5.29x). All six new
    cells plus two SUM/AVG controls verify exact results, NUMERIC GPU dispatch,
    full input-row processing and zero fallback. Loading/refresh is separate
    and slower than a native query; these are shared-host local measurements,
    not cold-query, concurrency or release-headline claims. Mixed measure
    types, general predicates, joins and CUDA remain separate open work.
    Durable evidence: `benchmarks/evidence/numeric-count-20260929/README.md`.
  - [x] Fuse exact NUMERIC and int2/int4 SUM/AVG/COUNT in one normally selected
    aggregate, with independent INT4 predicates and conditional COUNT(*),
    globally or over up to three dictionary keys. Integer SUM retains int8,
    AVG retains PostgreSQL NUMERIC finalization, and only decimal SUM states
    allocate/price digit buckets. Mixed integer SUM lifecycles are bounded by
    INT64_MAX / 2^31 rows to prove every possible sum prefix fits int64;
    prospective admission and cumulative native execution enforce that limit.
    Predicate inputs bind through RHS without fake aggregate states; the
    shape-builder regression is covered while integer-only admission stays
    unchanged. SQL 117 verifies 64 normally selected shapes and mixed prepared
    HAVING/order/limits through DML, rollback, NULLs and empty input. Contracts
    111/115/117 pass warning-free on PG18/19; all 1138 core, 571 benchmark,
    32 native and eight live NUMERIC tests per major pass, along with both
    production Clippy builds and coverage/safety/documentation checks.
    Million-row warm medians show 8.07x for the mixed conditional rollup and
    2.61x globally. The complete 12,748,986-row NYC passenger/fare/distance
    rollup is exact and normally dispatched: 63.06 ms versus 301.70 ms (4.78x).
    All eight benchmark/control cells verify full GPU row processing and zero
    fallback. Loading is separate (4.25 seconds for the full month) and first
    use loses. This is shared-host per-query characterization, not a paired
    before/after result, concurrent-throughput or release-headline claim.
    Other mixed types, general expressions/predicates, query-level WHERE,
    joins, unbounded row domains and CUDA remain open.
    Durable evidence: `benchmarks/evidence/numeric-mixed-20260929/README.md`.
  - [x] Compose query-level WHERE with exact NUMERIC/mixed aggregation and
    independent aggregate FILTER predicates. Accept complete single-column
    INT4 Boolean/IN predicates normalized to one through four exact ranges,
    including IS NOT NULL, or a stored nullable Boolean mask. Reject rows
    before grouping, COUNT(*) and decimal arithmetic; preserve empty global
    and grouped results. A hidden integer predicate state obeys the existing
    four-state limit, with COUNT(*) and output bindings remapped together.
    Native multi-chunk overflow bounds still count all input rows, while
    finalized selected-row counts report only accepted rows. WHERE comparison
    work is costed without coefficient changes or forced planner selection.
    SQL 117 now verifies 136 normally selected shapes and filtered prepared
    HAVING/order/limit queries through DML, rollback, NULLs and empty input.
    Contracts 111/115/117 pass warning-free on PG18/19. All 1139 core,
    571 benchmark, 32 native and eight live NUMERIC tests per major pass,
    alongside both production Clippy builds and coverage/safety/doc checks.
    Million-row warm medians show 5.89x for the filtered three-key rollup and
    3.60x for the Boolean-filtered global aggregate. The complete 12,748,986-row
    NYC vendor-filtered fare/distance query takes 35.96 ms versus 155.44 ms
    (4.32x). All eight benchmark/control cells verify exact results, full GPU
    input-row processing and zero fallback. Full-month loading costs 4.83
    seconds separately; first use loses. These are shared-host per-query warm
    measurements, not before/after, concurrent-throughput or release claims.
    General expressions, decimal-input/multi-column/NULL-accepting predicates,
    joins, concurrent throughput and CUDA remain separate open work.
    Durable evidence: `benchmarks/evidence/numeric-where-20260929/README.md`.
  - [x] Compose qualified spatial predicates with exact NUMERIC/mixed rollups:
    point/constant Polygon or MultiPolygon Intersects, Contains/Within and
    Covers/CoveredBy can feed global or up-to-three-key SUM/AVG/COUNT, including
    independent INT4 aggregate filters and COUNT(*). At least one decimal
    measure and the existing four-state bound remain required. Geometry
    topology/SRID/orientation, catalog identity, exact PostGIS boundary
    rechecks, residency, work limits and ledger gates remain intact. Cached
    and ephemeral spatial masks use the shared bounded NUMERIC consumer with
    fresh per-chunk borrows and interrupt checks outside store guards. Costing
    retains spatial/recheck work separately from decimal/mask/launch work;
    the decimal row floor applies too. No ABI or cost-coefficient changes.
    SQL118 verifies 32 normally selected shapes against both PostGIS and an
    independent Cartesian oracle, then selected prepared HAVING/order/limit
    through geometry/value DML, rollback, all-NULL and empty input on PG18/19.
    Its DELETE-to-empty case exposed and fixed canonical zero-row geometry
    handling; mismatched typed/nonempty inputs still fail closed. Nine live
    NUMERIC tests per major include mixed spatial filters, excluded specials
    and overflow, 113-row chunks and empty global/grouped results. Core tests
    pass 1140 and benchmark tests 571; both production Clippy builds pass.
    Production SQL114/117/118 pass warning-free on both majors, all 32 native
    CTests pass, and coverage/safety/documentation checks pass. The complete
    12,748,986-row NYC JFK fare/distance/passenger rollup now dispatches exactly:
    26.58 ms versus parallel PostgreSQL's 362.93 ms warm (13.66x); the 1M prefix
    is 6.80x. All six benchmark/control cells verify exact results, full GPU
    input-row processing and zero fallback. Loading costs 6.36 seconds for the
    full rollup and refresh plus execution costs 10.59 seconds; first use loses.
    These shared-host warm-mask measurements are not indexed-query, cold,
    concurrent-throughput, before/after or whole-system release claims.
    This composes two existing producer/consumer stages, not general pipeline
    execution, row-producing spatial joins or shared-backend residency.
    Durable evidence: `benchmarks/evidence/spatial-numeric-20260929/README.md`.
  - [x] Enable normal global INT2/INT4 SUM/AVG/COUNT without SQL grouping keys,
    including four independent states, shared aggregates, independent INT4
    FILTER and conditional COUNT(*). Reuse the bounded hierarchical integer
    reduction with exact widened sums, PostgreSQL NUMERIC AVG and one active
    global group on empty input; single unfiltered counts retain their smaller
    count kernel. No ABI, cost-coefficient or forcing-setting changes. SQL119
    verifies 16 ordinarily selected shapes over 1,000,017 rows, then prepared
    HAVING/order/limit through updates, rollback, all-NULL and empty input on
    PG18/19, requiring actual dispatch in every phase. Native tests compare
    one-shot and 997-row chunks with extreme values and independent filters.
    Core tests pass 1140, benchmark tests 571 and native CTests 32; production
    SQL99/107/111/115/119 pass warning-free on both majors, as do Clippy and
    coverage/documentation checks. The SQL manifest has 82 files/399 assertions.
    Million-row warm conditional SUM/AVG takes 5.54 ms versus 28.12 ms native
    (5.08x); mixed conditional counts take 5.53 versus 27.03 ms (4.89x).
    All six benchmark/control cells match exactly with full GPU row processing
    and zero fallback. First-use loading/refresh still loses; these shared-host
    measurements are not cold-query, concurrency or whole-release claims.
    Query-level integer WHERE, integer-only spatial rollups, general expressions,
    unbound predicate inputs, INT8 SUM/AVG and joins remain separate work.
    Durable evidence: `benchmarks/evidence/global-integer-20260929/README.md`.
  - [x] Compose exact spatial predicates with integer-only INT2/INT4
    SUM/AVG/COUNT, globally or with up to three dictionary keys and four states;
    a decimal measure is no longer required. Preserve independent aggregate
    FILTER, exact PostGIS boundary rechecks, geometry/cost/budget gates and
    bounded cached/ephemeral execution. Native and SQL tests caught and fixed
    the combined-filter dispatch variant that ignored the spatial WHERE mask.
    Native tests now cover hierarchical, atomic and chunked execution with
    NULL/false/true and invalid masks, independent filters, INT32 extremes,
    all-rejected input and empty reuse. SQL120 verifies 48 normally selected
    shapes against PostGIS and Cartesian predicates, plus prepared DML/rollback,
    all-NULL and empty input on both PG18/19. SQL118/119 controls pass too.
    Core tests pass 1141, benchmark tests 571 and native CTests 32, with both
    Clippy builds and documentation/coverage checks passing. The SQL inventory
    has 83 files/401 assertions. The complete 12,748,986-row JFK passenger
    rollup matches exactly with verified GPU execution and zero fallback:
    33.11 ms versus parallel PostgreSQL's 253.05 ms warm (7.64x). Loading costs
    2.71 seconds and refresh plus execution 6.67 seconds; first use loses.
    These are shared-host per-query warm-mask measurements, not indexed-query,
    cold-query, concurrency or whole-release claims. General integer WHERE,
    unbound filter inputs, INT8 SUM/AVG, spatial joins and proximity remain open.
    Durable evidence: `benchmarks/evidence/spatial-integer-20260929/README.md`.
  - [x] Compose query-level WHERE with integer-only INT2/INT4 SUM/AVG/COUNT:
    exact single-column INT4 range algebra or stored Boolean masks, zero through
    three keys, independent aggregate FILTER and COUNT(*). Hidden predicate
    columns consume a state within the four-state bound; output/HAVING mappings
    remain independent of COUNT(*) ordering. Kernels read each measure's own
    value/NULLs while retaining the existing product predicate-value reuse.
    SQL121 verifies 160 normally selected shapes and prepared DML/rollback,
    all-NULL/empty behavior on PG18/19. Native tests cover a separate nullable
    WHERE column, disjoint ranges and hierarchical/atomic/chunked execution.
    Core tests pass 1142, benchmark tests 571, native CTests 32, both Clippy
    builds and documentation/coverage checks pass. PG18's strict full SQL run
    passes 84/84; PG19's initial 82/84 plus corrected SQL96/97 pass. The latter
    now positively tests three same-column bounds and retains cross-column
    rejection, exact reasons and no dispatch. The inventory is 84 files/403
    assertions. A 256-descriptor host limit caused a retained Metal failure;
    unchanged tests pass with repo-owned postmasters launched at 4096.
    Warm million-row range and Boolean-mask rollups are 3.41x and 5.61x faster
    than parallel PostgreSQL. The complete 12,748,986-row vendor-filtered NYC
    passenger rollup takes 24.09 versus 96.95 ms (4.02x). All eight benchmark/
    control cells match exactly with full GPU row processing and zero fallback.
    First use still loses; these shared-host measurements are not cold-query,
    concurrency, before/after or whole-release claims. General expressions,
    multi-column/NULL-accepting predicates, INT8 SUM/AVG and joins remain open.
    Durable evidence: `benchmarks/evidence/integer-where-20260929/README.md`.
  - [x] Bind independent INT4 aggregate FILTER inputs for integer-only
    SUM/AVG/COUNT without consuming hidden aggregate states. Reuse the typed
    RHS predicate view, preserve four real states and independent value/predicate
    NULLs, and compose with global/grouped, WHERE and spatial execution.
    SQL121 now verifies 256 shapes plus prepared DML/rollback/NULL/empty behavior;
    SQL120 verifies 64 spatial shapes. Both strict full PG18/19 SQL suites pass
    84/84 files, native CTests 32/32, core tests 1143, benchmark tests 571,
    coverage-tool tests 154 and documentation tests 21; both Clippy builds pass.
    Cross-column FILTER negatives retain exact decline/no-dispatch contracts.
    Eight matched benchmark/control cells have exact results and zero fallback.
    Full-month NYC conditional passenger totals take 31.05 versus 112.43 ms
    (3.62x warm); the JFK variant takes 34.65 versus 251.92 ms (7.27x warm).
    First use still loses, especially fresh spatial classification; shared-host
    warm measurements do not qualify cold queries, concurrency or a release.
    Eliminating hidden WHERE states, broader predicates/expressions, INT8 SUM/AVG,
    joins and streaming remain separate work. Durable evidence:
    `benchmarks/evidence/integer-filter-inputs-20260929/README.md`.
  - [x] Compose an exact point/constant-area spatial WHERE predicate with
    same-relation INT4 ranges for integer and decimal/mixed aggregates.
    Consume every AND conjunct, preserve three-valued scalar semantics and
    both residency dependencies, and evaluate both predicates before grouping.
    The bounded AQS4 conjunction fails closed on nested or malformed children;
    no C ABI change, extra scalar mask/launch or host row filtering is needed.
    SQL122 verifies 96 selected shapes, independent FILTER, exact spatial
    oracles, prepared DML/rollback/NULL/empty behavior and native declines.
    Both strict full PG18/19 SQL suites pass 85/85; native CTests 32/32,
    core tests 1145, benchmark tests 571, coverage-tool tests 154 and doc tests
    21 pass, as do both Clippy builds. The inventory has 406 assertions.
    All eight benchmark/control cells match exactly with zero fallback.
    Full-month JFK/vendor passenger totals take 34.67 versus 198.53 ms
    (5.73x warm); fare/distance takes 19.43 versus 203.31 ms (10.46x warm).
    Fresh loading/classification still loses. Spatial OR, multiple scalar
    columns, NULL-accepting predicates, stored-Boolean conjunctions, broader
    expressions and joins remain open; this is not whole-release evidence.
    Durable evidence: `benchmarks/evidence/spatial-scalar-20260929/README.md`.
  - [x] Generalize INT2/INT4 MIN/MAX to independent and shared SUM/AVG/COUNT
    states, zero through three dictionary keys, independent aggregate FILTER,
    WHERE masks/ranges and exact spatial restrictions. Preserve input result
    types, NULL/empty states and native HAVING/order/limits. Add signed int32
    atomic extrema with exact scratch accounting; bounded partial execution
    merges extrema without computing unused SUM arithmetic. No C ABI or
    logical-wire change, host reduction or CPU fallback is required.
    SQL123 verifies 96 selected shapes and prepared global/grouped lifecycle
    cases; SQL105 adds hidden extrema in HAVING. Former extrema negatives in
    SQL96/99/100/101/104/105 retain genuine unsupported states, exact decline
    counters and no-dispatch guards rather than dropping assertions.
    Final full PG18/19 suites pass 86/86 each, native CTests 32/32, core tests
    1146, benchmark tests 571, coverage-tool tests 154 and doc tests 21; both
    Clippy builds pass. The inventory has 408 assertions in 37 families.
    Eight benchmark/control cells match exactly with full GPU input processing
    and zero fallback. Complete-month NYC passenger MIN/MAX/SUM/AVG/COUNT takes
    35.53 versus 175.67 ms (4.94x warm); its JFK variant takes 37.58 versus
    251.96 ms (6.70x warm). Fresh load/classification still loses. NUMERIC/INT8
    extrema, extrema mixed with decimal states, general expressions and joins
    remain open. This is local Metal evidence, not whole-release qualification.
    Durable evidence: `benchmarks/evidence/integer-extrema-20260929/README.md`.
  - [x] Add exact bigint column SUM/AVG returning PostgreSQL NUMERIC, including
    sums beyond int64 and INT64_MIN. Keep raw eight-byte resident values and
    split their magnitudes into decimal digits on the GPU; reuse exact SUM/AVG
    finalization without host reduction or a new ABI layout. Compose with
    mixed decimal/int2/int4 states, independent filters, zero through three
    dictionary keys, WHERE masks/ranges and qualified geo-plus-scalar rollups.
    SQL124 checks 120 selected shapes and prepared/DML/rollback/NULL/empty
    cases. Full PG18/19 suites pass 87/87 each, core 1147, benchmark 572,
    native 32/32, coverage helpers 154, docs 21 and all Clippy checks pass.
    At 1M rows, global SUM/AVG takes 8.43 versus 15.17 ms (1.80x warm), and
    three-key rollups take 6.60 versus 28.49 ms (4.32x warm). The 250K mixed
    filtered case wins 2.62x; its 1M counterpart remains native and receives
    no GPU credit. All outputs match exactly, with independent bigint fixture
    oracles and zero fallback. First-use loading still loses; bigint
    expressions/extrema, broader pipelines and release qualification remain
    open. Durable evidence: `benchmarks/evidence/bigint-numeric-20260929/README.md`.
  - [x] Accept single-column INT4 IS NULL and NULL-or-range predicates in
    aggregate FILTER and WHERE, including exact spatial conjunctions and
    integer/decimal/bigint aggregate states. Preserve three-valued Boolean
    logic and distinguish conditional COUNT(*) from COUNT(predicate input).
    AQS5 adds explicit NULL-or-ranges semantics; a reserved native filter flag
    carries NULL acceptance without changing the ABI layout, creating another
    bitmap, filtering fact rows on the host or adding a GPU launch. Dynamic
    arrays, multi-column predicates and out-of-capacity expressions stay native.
    SQL125 covers 240 selected shapes plus prepared/DML/rollback/NULL/empty
    lifecycles. Full PG18/19 suites pass 88/88 each, core 1149, benchmark 573,
    native 32/32, coverage helpers 154, docs 21 and all Clippy checks pass.
    Eight benchmark/control cells match exactly with verified GPU modes and
    zero fallback. Nullable bigint rollup at 1M wins 3.36x warm; full-month NYC
    passenger and JFK variants win 5.28x and 7.50x. NYC has no NULL passenger
    counts; synthetic/native/SQL tests cover actual NULL inputs. First-use
    refresh/classification still loses, especially geo (6.66s versus 279ms).
    No cost thresholds were weakened. Local Metal evidence only; broader
    predicates, cold/concurrent performance and release qualification stay open.
    Durable evidence: `benchmarks/evidence/null-predicates-20260929/README.md`.
  - [x] Accelerate checked two-column INT4 `+`, `-` and `*` measures across
    SUM/AVG/COUNT/MIN/MAX, including mixed states, zero through three dictionary
    keys, supported FILTER/WHERE/geo restrictions and native HAVING/order/limits.
    Either NULL operand makes the expression NULL; overflow raises 22003 even
    for COUNT-only states. Filters reject before arithmetic. Independent
    predicates preserve the arithmetic RHS and use hidden raw-column states
    only within the existing four-state cap. AQS6 adds ADD without changing C
    ABI layout. Expression evaluation is charged without weakening gates.
    SQL126 covers 72 selected shapes, widened NUMERIC/spatial-coordinate
    oracles, prepared DML/rollback/NULL/empty cases and overflow recovery.
    PG19 passes 89/89 files and 416/416 assertions. PG18's full run passes 88/89;
    the stale SQL97 specialization-label assertion is corrected and both
    SQL97 and SQL126 pass again on the final build. Core 1153, benchmark 574,
    native 32/32 (45007 grouped checks), coverage helpers 154, docs 21 and
    Clippy checks pass. Global computed measures at 1M are 3.13x/3.37x faster
    warm in two runs, with exact results, verified GPU execution and no fallback.
    First-use refresh still loses (about 62ms versus 17ms); warm CV reaches
    29.50%. The one-key four-state benchmark remains native and its initial
    failed winner gates are retained, not reinterpreted as GPU evidence.
    Existing product/direct-column controls still win locally. Nested/literal
    and broader typed expressions, calibrated grouped admission, cold/concurrent
    performance and release qualification remain open. Evidence:
    `benchmarks/evidence/computed-integer-20260929/README.md`.
  - [ ] Integrate the SPG1 typed scalar-program foundation into resident
    aggregate producers, including PostgreSQL expression lowering, versioned
    logical/runtime bindings, exact budget/currentness proofs, cost admission
    and selected SQL parity/performance tests. The bounded forward-only
    validator and device evaluator now cover INT4/Boolean arithmetic,
    comparisons, NULL tests and lazy CASE/COALESCE/AND/OR with distinct overflow
    and division-by-zero errors. Their 108 shared semantic cases match the
    independent Rust interpreter and native PG18/19. The Metal evaluator passes
    759 host/device cases; the full native suite passes 33/33 and Rust core
    passes 1164 tests at the initial foundation milestone. The PostgreSQL-node
    lowerer now handles planner-simplified searched CASE/COALESCE, nested INT4
    operations and Boolean/NULL expressions, with built-in implementation OID
    checks, deduplicated inputs and range-table occurrence rejection. Recognized
    SPG1 measures validate program/input/result types inside the existing AQS
    envelope. Five live tests pass on each PG18/19 and core passes 1165. Simple
    CASE locals, casts/domains, mixed relation occurrences, runtime bindings,
    memory/currentness proofs, costs and selected SQL tests remain unfinished.
    This groundwork does not yet enable these SQL expressions and must not be
    described as completed general-expression support. Lowering evidence:
    `benchmarks/evidence/scalar-lowering-20260929/README.md`.
    Native dense aggregates now fuse SPG1 evaluation into dedicated kernel
    variants after WHERE/FILTER, without an intermediate result column.
    The additive versioned scalar view preserves outer ABI layouts, proves
    all control-flow paths before launch and stages metadata into exactly
    sized workspace. COUNT evaluates nullable/error-producing expressions;
    division by zero has a distinct hard error. Tests cover one-shot and
    chunked reducers, lazy branches, full 64-input/four-state bindings,
    malformed metadata/USM, failure poisoning and RESET recovery. Unsupported
    shapes cannot fall into the generic serial kernel. Planner and SQL
    admission remain disabled. Native integration evidence:
    `benchmarks/evidence/scalar-fused-20260929/README.md`.
    Final native suite passes 33/33 (48,900 grouped checks), Rust core 1166,
    and Clippy with pg_test passes on PG18/19. No performance claim yet.
    Follow-up: Rust plans/chunks/sessions now own validated scalar view, words
    and binding snapshots. Chunk slicing advances typed nested resident lanes;
    full program contents participate in lifecycle/workspace-pool identity.
    Program/input budget and currentness proofs, runtime capability,
    planner/cost admission and selected SQL tests remain unfinished.
    Owned binding/core checks pass (1170 tests), as does a live direct-facade
    test on each PG18/19 against native SQL aggregates, including chunked
    re-resolution and typed COUNT division errors. This does not test planner
    selection. Evidence: `benchmarks/evidence/scalar-ownership-20260929/README.md`.
    Executor descriptor binding now validates recognized SPG1 programs against
    exact fact-column identities/types and resident row/NULL lengths, owns
    program/view/input metadata, and binds independent raw INT4 FILTER RHS.
    The physical workspace classifier mirrors native scalar shapes; runtime
    capability and normal planner admission remain rejected until accounting
    and qualification are complete. Core binding/rejection tests pass (1174).
    Live binder-to-GPU parity passes on PG18/19, including short NULL-sidecar
    rejection and typed COUNT errors. Evidence:
    `benchmarks/evidence/scalar-binding-20260929/README.md`.
    Runtime scalar snapshots now reserve exact requested view/word/input heap
    payload before allocation across bound, plan, session and sliced-chunk
    lifetimes; scalar workspace/output buffers are also execution-ledger
    charged, including pooled buffers. Wire validation and typed CFG joins use
    fixed-size stack scratch, with measured zero heap allocations for maximal
    programs, mutated/error inputs and facade budget rejection. Core tests
    pass (1177), including exact budget boundaries and partial-failure cleanup.
    PG18/19 live binder/GPU parity and zero-budget rejection pass; teardown
    returns the ledger to baseline. These tests use the pg_test ledger, not
    the process-shared production ledger. Prospective planner peak estimates,
    artifact/currentness integration and selected SQL/cost qualification remain
    open. Evidence: `benchmarks/evidence/scalar-accounting-20261005/README.md`.
    Follow-up: residency admission now includes the four-snapshot scalar
    metadata peak, with exact charge comparison for one through four states
    and a one-byte-short budget rejection test (1179 core tests pass).
    The full scalar execution forecast remains open: native workspace/output,
    logical spec/artifact retention and their simultaneous peaks are not yet
    integrated. A new live test exercises the actual cached descriptor artifact,
    resident resolver and binder across scalar-only updates, aborted updates,
    NULLs, TRUNCATE/INSERT and INT4-to-INT8 DDL; it is not normal SQL selection.
    This test passes on PG18/19, as do both-major Clippy and SQL126 on restored
    normal builds without warnings. Follow-up evidence is in the same archive.
    Native scalar workspace/output forecasting is now integrated into the
    residency budget alongside the four-snapshot metadata peak. It requires
    exact loaded row/NULL evidence and per-key dense-slot estimates, and uses
    the executor's one-shot/bounded chunk decision plus the native layout
    calculator. Queries borrow stack metadata, read no payloads, reserve no
    bytes and leave allocation pools untouched. Core passes 1180 tests,
    including combined-budget and incomplete-evidence rejection. Logical
    spec/artifact storage, scalar costs and selected SQL qualification remain
    unfinished; capability/admission are still disabled.
    PG18/19 forecast parity matches fresh one-shot allocations and bounded
    workspaces, plus 63 multi-state layout comparisons per major. Zero-budget
    quotes leave ledger/pools unchanged. Both-major Clippy and restored normal
    SQL126 regressions pass without warnings; evidence is in the same archive.
    Dense cached/query-local artifacts now reserve scalar input/word vector
    payloads before preparation copies them, retain combined host/device
    charges for their exact lifetime, and include the retained program bytes in
    the planner forecast. Cache hits do not recharge; budget, preparation,
    build and accounting failures release reservations without publication.
    Result setup now borrows the pinned spec, removing its unnecessary clone
    and the bounded executor's duplicate spec across chunks. Other logical
    containers/encoded identities, scalar costs, normal lowering/admission and
    selected-SQL qualification remain open; scalar admission stays disabled.
    Core passes 1184 tests. PG18/19 scalar tests now cover the real one-shot
    and bounded executor through SUM/COUNT result conversion; cancellation
    recovery, Clippy and restored normal SQL126 also pass on both majors,
    without SQL warnings. Follow-up evidence is in the accounting archive.
    Aggregate extraction now invokes the typed lowerer for nested arithmetic,
    searched CASE/COALESCE and Boolean/NULL expressions while preserving the
    existing column/binary representations. Shape building shares program
    states, preserves projection order and independent filters, and budgets
    hidden WHERE carriers. Runtime capability/specialization now accepts the
    fact-only dense scalar family; hash, dimension and spatial compositions
    remain rejected. Provisional costing charges the longest lazy instruction
    path per state using the typed operation coefficient, without favorable
    selectivity assumptions. The normal planner still has an explicit scalar
    qualification gate; calibration and selected-SQL validation remain open.
    Core passes 1188 tests and PG18/19 each pass nine scalar live tests,
    including actual aggregate extraction and full descriptor-plan SUM/AVG/
    COUNT/MIN/MAX materialization over nullable, all-NULL and empty inputs.
    The numeric/spatial lifecycle regression also passes on both majors.
    Both-major Clippy and restored normal SQL126 pass without SQL warnings,
    preserving the explicit normal-decline boundary for nested expressions.
    Evidence remains in the accounting archive; these are not selected-SQL
    or calibrated-performance claims.
    Normal-admission follow-up: the explicit scalar qualification rejection is
    removed for this typed fact-only dense family. Exact loaded-evidence,
    memory, device and existing conservative cost gates still apply; no cost
    coefficient, budget or row threshold was relaxed. Artifact policy now
    includes retained scalar-program payloads in its construction-byte quote.
    SQL128 compares 48 ordinary query shapes against native PostgreSQL and
    checks actual selected dispatch, prepared-plan DML/rollback/NULL/empty
    behavior, selected COUNT and aggregate error SQLSTATEs, unsupported-shape
    rejection, zero-budget decline and DDL invalidation. On this host, its 24
    global shapes select; grouped shapes remain native. Lazy-error exclusion
    queries are parity checks, not independently asserted selected paths.
    New global/grouped scalar benchmarks use an independent widened polynomial
    oracle for shared CASE/COALESCE SUM/AVG/COUNT/MIN/MAX states. Final-build
    evidence and qualification limits are recorded in
    `benchmarks/evidence/scalar-qualification-20261005/README.md`.
    Broader cost calibration, grouped selected-SQL qualification, concurrent/
    between-chunk lifecycle coverage and remaining host-container accounting
    stay open. This is not general-expression or scalar geo/join support.
    Grouped-admission correction: extraction now fills the per-key cardinality
    estimates required by scalar memory forecasts (previously only NUMERIC
    requested them). Missing evidence still declines, Cartesian NULL-slot
    budgeting and runtime reservations remain intact, and no cost gate changed.
    SQL128 now proves normal dispatch and exact parity for all 96 matrix shapes
    on PG18/19, including 72 grouped cases across one/two/three nullable keys.
    Global and two-key prepared plans survive dictionary-changing updates,
    rollback, all-NULL and empty inputs. SQL117 also passes both normal builds
    without warnings. Core 1188, benchmark 577 and both-major Clippy pass.
    A new independent-oracle grouped COALESCE benchmark and the complex grouped
    control remain native against configured parallel PostgreSQL at 250K/1M;
    this is a functional admission fix, not a grouped performance win.
    Raw native-decline results and source provenance are retained in
    `benchmarks/evidence/scalar-grouped-20261005/README.md`.
    Broader scalar cost calibration and reduction of redundant aggregate work
    remain performance targets; do not lower cost coefficients just to select.
    COUNT-state optimization: a bounded allocation-free forward proof now
    identifies scalar results that are non-NULL on every successful path,
    assuming all inputs nullable. COUNT(*) and strict FILTER-normalized counts
    can share that required expression's COUNT lane only under identical
    filters. Evaluation/errors are retained; NULL-accepting column counts and
    differing filters stay separate. Sharing precedes the four-state cap and
    preserves projection metadata, admitting four scalar expressions plus
    COUNT(*) independent of target order. Core passes 1190 tests. SQL128 passes
    both normal PG18/19 builds, including explicit shared/separate-state plans,
    prepared lifecycle and native error/result comparison when PG19 simplifies
    a provably non-NULL COUNT argument before extraction. Existing 96 matrix
    shapes still dispatch. The unchanged grouped COALESCE workload now selects
    at 250K and 1M without coefficient, threshold or budget changes: confirmation
    warm medians 2.045/10.919 ms (5.34x) and 3.011/26.272 ms (8.73x) versus
    parallel PostgreSQL, 30 pairs per scale, exact independent oracles, actual
    dispatch and zero stock fallback. CV is about 21%; loading/refresh still
    loses. Initial measurements and controls are retained, not discarded.
    Evidence: `benchmarks/evidence/scalar-count-share-20261005/README.md`.
    Broad calibration, nullable complex programs, spatial/join composition,
    remaining host-container accounting and concurrent lifecycle work remain.
    Spatial composition follow-up: qualified resident point/polygon predicates
    now compose with typed scalar programs. Exact boundary/hole rechecks build
    the SQL mask before expression evaluation. Cached and query-local artifacts
    route scalar programs through bounded dense execution. Spatial preflight
    reserves both transient and retained program-vector payloads before cloning;
    planner memory and launch forecasts include this staged overlap/lifecycle.
    Core passes 1192 tests. Normal PG18/19 SQL129 each selects all 24 global and
    one-to-three-key matrix shapes, compared with native PostgreSQL plus
    independent integer-coordinate and widened arithmetic oracles. Additional
    tests cover Boolean COUNT, independent FILTERs, prepared DML/rollback/NULL/
    empty input, spatial error suppression, selected errors without fallback,
    and zero-budget rejection. Adjacent SQL126/128 also pass both majors.
    Scalar joins, hash execution, broader host-container accounting and
    concurrent lifecycle qualification remain open. Evidence and separate
    warm/refresh measurements: `benchmarks/evidence/scalar-geo-20261005/README.md`.
    New full-scan polygon-with-two-holes benchmarks dispatch with exact Rust
    coordinate/polynomial oracles at 1M/2M rows, 30 pairs and 10 warmups in both
    initial and confirmation runs. Confirmation warm global medians are
    6.548/82.755 ms and 10.949/123.352 ms (12.64x/11.27x); grouped medians are
    4.925/76.706 ms and 69.834/295.714 ms (15.58x/4.23x). Host variance is large
    (2M grouped native CV 90.2%); retain all samples and do not promote WINNER
    metadata. Loading/refresh still loses. Improving bounded grouped scalar
    execution at 2M and spatial preparation costs remains a performance target.
    Bounded-kernel follow-up: integer/scalar batches now reuse the row-parallel
    atomic kernel when cumulative selected rows plus input rows prove every
    INT4 SUM prefix fits int64. Batch-local counts reset and checked commits
    merge persistent SUM/COUNT/extrema; FINALIZE does not double-count. Outside
    the proof, ordered GPU partials retain cancellation-sensitive overflow.
    Native workspace queries reserve both paths, including shorter chunks of
    a maximum that cannot use atomics. No row cap, cost gate, ABI or budget
    bypass changed. An adaptive cooperative candidate regressed and was removed.
    All 33 native CTests pass (final focused aggregate rerun: 51598 checks),
    core 1192 and benchmark 578 tests pass, both-major Clippy passes, and normal
    PG18/19 SQL126/128/129 pass 26 assertion groups without warnings. The 2M
    grouped spatial median drops from 66.280 ms to 9.317 ms initially and
    8.418 ms in confirmation (7.87x versus the old GPU median). Variance remains
    high; all raw samples and the failed candidate are retained. The new 2M
    non-spatial grouped COALESCE scale also selects: 6.491 vs 64.014 ms native
    (9.86x), with exact independent oracles and no fallback. Loading/refresh
    still loses; no one-shot non-inferiority or universal speedup is claimed.
    Evidence: `benchmarks/evidence/scalar-bounded-20261006/README.md`.
    Simple CASE follow-up: INT4/Boolean test values now evaluate once on the
    bounded scalar stack, with exact built-in equality, nested lexical scopes,
    NULL semantics and lazy branch errors. Additive SPG1 DROP (opcode 5) is
    checked by both validators/evaluators; outer wire and native layouts stay
    unchanged. The final WHEN consumes its saved value directly, eliminating
    redundant DUP/DROP work. Single-arm cases have bytecode identical to their
    searched-CASE equivalents; no cost coefficient or threshold changed.
    PG18/19 each pass nine live tests and select all 144 OLAP and 48 spatial
    matrix shapes (72 more selected shapes per major). Prepared DML/rollback,
    empty/NULL inputs, errors, custom equality rejection and geo/FILTER error
    suppression pass. Core 1192, benchmark 579 and all 33 native suites pass;
    the unchanged final native evaluator passes 768 host/device cases.
    The new conditional-pricing benchmark now selects normally at 250K/1M/2M:
    3.508/6.816/14.102 ms versus parallel PostgreSQL 18.404/47.657/78.380 ms
    (5.25x/6.99x/5.56x), with independent oracles and zero fallback. GPU CV is
    15.8–27.4%; no WINNER promotion. The more complex public-table CASE probe
    remains native; diagnose its path-choice/admission boundary rather than
    treating a generic last-rejection label as a complete explanation.
    Loading/refresh still loses, and initial SQL126 verification recorded
    19.1 s / 8.5 s dispatch warnings on PG19/18. Those logs, initial native
    measurements and the provenance-aborted run are retained. Final SQL128/129
    reruns have no warnings. Evidence:
    `benchmarks/evidence/simple-case-20261006/README.md`.
    Scalar fusion follow-up: the complex CASE was admitted against the serial
    comparator but lost PostgreSQL's final parallel-path choice, not the generic
    last rejection label. A bounded compiler pass now fuses equality/branch
    and INT4 literal COALESCE operations (additive SPG1 opcodes 55/6), preserving
    branch entries, relocating forward edges and revalidating the whole CFG.
    Rust/native proof, non-NULL analysis and GPU evaluation agree; no cost
    coefficient, threshold, budget, descriptor layout or program bound changed.
    Core 1195, all 33 native suites (790 scalar cases, 51598 aggregate checks),
    both-major strict Clippy and nine focused live tests per major pass.
    Normal PG18/19 SQL128/129 pass all 18 assertion groups and select all
    144 OLAP / 48 geo matrix shapes per major. PG19 records one retained
    5.199918-second first-use dispatch warning. The complex CASE now selects
    normally at 250K: 2.381 vs 18.311 ms (7.69x warm), but 1M/2M still remain
    native with eight-worker plans and zero dispatch. Single-WHEN CASE measures
    2.450/3.494/7.098 ms versus 17.798/55.825/77.253 ms at 250K/1M/2M;
    2M grouped geo is 9.125 vs 183.054 ms. All selected rows pass independent
    oracles with zero fallback. Warm results remain noisy, are not matched
    old/new kernel comparisons, and do not establish universal non-inferiority;
    every refresh-inclusive probe still loses. Larger complex CASE selection,
    cold/refresh latency and the broader scalar roadmap remain open. Evidence:
    `benchmarks/evidence/scalar-fusion-20261006/README.md`.
    Constant membership follow-up: planner-simplified INT4/Boolean IN/NOT IN
    (`= ANY` / `<> ALL`) arrays now compose inside scalar aggregate producers.
    Computed inputs evaluate once; NULL/empty arrays, NULL members, duplicates,
    dimensions/lower bounds and lazy errors retain PostgreSQL semantics. Exact
    operator/hash/negator identities and array declarations are checked before
    decoding. Sorting/deduplication and overflow-safe consecutive-set range
    compaction use existing SPG1 instructions. Arrays remain capped at 256
    elements before compaction; sparse programs still obey the existing stack/
    instruction bounds. Dynamic arrays, other ANY/ALL operators, domains and
    mixed types remain native. Core 1197, benchmark 580, both strict Clippy
    builds and ten live tests per major pass, including hashed-IN metadata and
    malformed contract rejection. Normal SQL128/129 pass all 18 assertion groups:
    PG18 selects 209/216 OLAP shapes, PG19 208/216, and both select 64/64 geo
    shapes. New unfiltered membership shapes must dispatch; remaining selective
    shapes decline explicitly on cost. Prepared DML/rollback/NULL/empty cases
    and spatial/FILTER error suppression pass with no dispatch warnings.
    New membership OLAP medians are 3.587/5.887/9.733 ms at 250K/1M/2M versus
    native parallel 24.779/62.973/147.645 ms (6.91–15.17x warm); geo is
    5.378/7.640 ms at 1M/2M versus 87.538/176.006 ms (16.28–23.04x). All five
    scales select with independent oracles, 990 consumed output rows and zero
    fallback. GPU CV is 12.1–26.9%; loading is separate. Two small OLAP refresh
    observations win, but 2M OLAP and both geo probes lose; this is not a
    general cold-query claim or release/WINNER promotion. Fresh upstream
    pristine/loaded regression/isolation on PG18/19 pass all 1448 scheduled
    executions with verified artifact seals and exact normal module hashes.
    Complete raw upstream bundles, source snapshot and performance evidence:
    `benchmarks/evidence/scalar-membership-20261006/README.md`.
    Arithmetic fusion follow-up: checked INT4 literal-RHS and coalesced-RHS
    arithmetic now use additive SPG1 opcodes 60–69, sharing the native checked
    arithmetic primitive. A bounded fixed-point pass preserves branch entries,
    relocates/revalidates every pass, and keeps NULL/error/non-NULL-proof
    semantics. No coefficients, thresholds, bounds or descriptor layouts change.
    The existing complex CASE now selects at all three scales; formerly native
    1M/2M queries measure 4.700/8.460 ms versus parallel PG 38.644/66.751 ms
    (8.22x/7.89x warm). All 16 benchmark scales across six OLAP/geo workloads
    select, pass exact independent/native oracles and consume outputs with zero
    fallback. At 2M, geo scalar/membership measure 7.536/6.681 ms versus
    121.462/131.058 ms. GPU CV spans 9.7–59.6%; these are not matched old/new
    binaries or universal non-inferiority. Every refresh-inclusive probe loses.
    Core 1198, 33 native suites (3875 scalar cases, 58 rejected CFGs, 51598
    aggregate checks), both Clippy builds and ten live tests per major pass.
    Normal SQL128/129 pass all 18 assertion groups with 208/216 OLAP and
    64/64 geo shapes selected per major, no dispatch warnings. An initial new
    test SQL operator-spacing typo was corrected; the failed log is retained.
    No fresh full upstream/coverage run or release/WINNER promotion is claimed.
    Larger complex CASE selection is established for these registered scales;
    cold/refresh latency and the broader scalar roadmap remain open. Evidence:
    `benchmarks/evidence/scalar-arithmetic-20261006/README.md`.
    Evidence: `benchmarks/evidence/scalar-contract-20260929/README.md`.
  - [x] Extend qualified scalar aggregate FILTER beyond single-column INT4
    intervals to multi-column INT4/Boolean expressions, CASE, constant IN/NOT
    IN and filtered COUNT(*). Retain the interval fast path; otherwise compile
    predicate-before-argument in one bounded SPG1 producer, returning typed
    NULL on false/UNKNOWN without removing the group or evaluating rejected
    arguments. Predicate-only inputs share occurrence checks, dependencies,
    exact metadata budgets and costs with the argument. No ABI/kernel/cost
    coefficient/threshold change. SQL130 selects all 32 matrix shapes and
    three compositions per PG18/19; two compositions explicitly decline on
    cost. Prepared predicate-only DML, rollback, NULL/empty/reinsert, multi-chunk
    errors and spatial composition pass. Eight targeted SQL files per major
    pass 52 assertion groups with no dispatch warnings; core 1198, benchmark
    581, both strict Clippy checks, live tests 10/major, docs 21 and coverage
    helpers 104 pass. Inventory is 93 SQL files/431 assertions at unchanged
    thresholds. New spatial FILTER benchmarks select at 1M/2M and measure
    4.910/6.453 ms vs native 66.596/117.867 ms (13.56x/18.27x warm), with exact
    independent/native oracles and zero fallback. The standalone public-table
    OLAP workload remains native at 250K/1M/2M; all three below-parity native
    rows are retained, not labeled GPU results. All refresh-inclusive probes
    lose. Initial over-strict selection checks and cost diagnostics are retained.
    General WHERE/types/joins, predicate-work sharing and broader cost
    qualification remain open; no fresh upstream/native/coverage or release
    certification is claimed. Evidence:
    `benchmarks/evidence/scalar-filter-20261006/README.md`.
  - [x] Add typed DATE scalar inputs/literals, comparisons, DATE±INT4,
    INT4+DATE and DATE-DATE inside CASE/COALESCE and aggregate FILTER. Date
    differences feed integer aggregates; DATE results feed COUNT/MIN/MAX.
    Preserve strict NULLs, both infinities, exact finite bounds and SQLSTATE
    22008, including COUNT evaluation and rejected-row error suppression.
    Additive SPG1 type/opcodes and error detail leave outer layouts, budgets,
    coefficients and thresholds unchanged. Normal PG18/19 SQL131 selects all
    24 arithmetic shapes plus DATE extrema/FILTER compositions; prepared DML,
    rollback, all-NULL/empty/reinsert and multi-chunk errors pass. Native scalar
    parity now covers 168 shared oracle cases; all 33 native suites pass.
    DATE array lowering, timestamps/intervals/casts, date extraction/bucketing,
    general WHERE and scalar joins remain open. Dated OLAP/geo workloads have
    independent epoch-offset and spatial oracles. Evidence and measured limits:
    all five registered scales select with zero fallback. At 2M, OLAP measures
    6.400 vs parallel PG 41.972 ms (6.56x), geo 6.846 vs 100.762 ms (14.72x).
    Warm ratios do not include loading; every refresh-inclusive probe loses.
    Eight SQL files per major pass 60 assertion groups without dispatch warnings;
    core 1200, benchmark 582, both strict Clippy builds, docs 21 and coverage
    helpers 104 pass. Inventory is 94 SQL files/435 assertions at unchanged
    thresholds. Retained failures include missing planner/workspace DATE gates,
    corrected test assumptions and a local Metal file-limit failure resolved
    by restarting the test servers under the established 4096-file limit.
    No fresh full upstream/instrumented coverage or release certification:
    `benchmarks/evidence/scalar-date-20261006/README.md`.
  - [x] Add typed query-level WHERE to fact-only dense integer/scalar rollups:
    multi-column INT4/BOOL/DATE predicates, lazy CASE/COALESCE and planner
    implicit-AND lists. FALSE/NULL rejects rows before group activity and
    argument evaluation; typed errors retain SQLSTATE and fail publication.
    A validated Boolean carrier shares the existing four-state/metadata budget
    contract, and reuses its TRUE result instead of evaluating twice. Costing
    charges each value worker without favorable selectivity; interval/mask
    fast paths, thresholds and coefficients are unchanged. PG18/19 SQL132 each
    select 24/28 shapes with four explicit cost declines and exact native plus
    independent oracles; prepared predicate-only changes, rollback, empty input
    and later-chunk errors pass. Fifteen focused files per major pass 118
    assertion groups without dispatch warnings. Core 1201, benchmark 583,
    native 33/33, strict Clippy both majors, docs 21 and coverage helpers 104
    pass; inventory is 95 SQL files/439 assertions/40 families at 90% unchanged.
    The public-table benchmark remains native at 250K/1M/2M (ratios
    0.962/0.971/0.982x), with no GPU speedup claim. All refresh-inclusive probes
    lose. Predicate sharing across workers, broader scalar WHERE/geo/join
    composition and qualification against parallel PostgreSQL remain open.
    Evidence: `benchmarks/evidence/scalar-where-20261006/README.md`.
  - [x] Elide a redundant Boolean WHERE carrier's row worker when another value
    state exists, deriving its COUNT from selected rows during checked merge.
    Preserve the sole carrier worker, arbitrary physical slot order, optional
    COUNT(*), all-rejecting aggregate FILTERs, bounded accumulation and typed
    errors in atomic/hierarchical/ordered reducers. Workspace reservations and
    cost coefficients/thresholds are unchanged; costing removes only eliminated
    work. PG18/19 SQL128–132 pass 44 assertion groups with no dispatch warnings;
    SQL132 now selects 28/28 predicate/grouping shapes on both majors, plus
    eight multi-chunk carrier queries. Core 1202, benchmark 583, native 33/33
    (53226 grouped checks), strict Clippy both majors, docs 21 and coverage
    helpers 104 pass. Inventory: 95 SQL files/440 assertions/40 families at
    unchanged 90% thresholds. Public-table 250K/1M/2M benchmarks remain native
    (0.964/0.978/0.999x), with zero GPU dispatch; no GPU speedup is claimed.
    All refresh-inclusive probes lose. General predicate sharing, broader
    compositions and qualification against parallel PostgreSQL remain open.
    Evidence: `benchmarks/evidence/where-carrier-20261006/README.md`.
  - [x] Fix implicit WHERE qualifier ordering before expanding spatial/scalar
    composition. A selected `a/b>0 AND flag` query incorrectly divided before
    PostgreSQL's cheap FALSE/NULL guard. Use native per-tuple qualifier costs
    with stable ordering before SPG1 lowering; retain equal-cost ordering,
    nested CASE/Boolean semantics and fail-closed errors. SQL133 exercises nine
    normally selected million-row shapes plus equal-cost SQLSTATE checks on
    PG18/19, with independent/native result oracles and no stock fallback.
    Core 1203 and strict Clippy both majors pass. Final SQL121/122/125/132/133
    pass 28 assertion groups across both majors with no warnings; all existing
    28 WHERE matrix shapes still select. Coverage helpers 104 and docs 21 pass;
    inventory is 96 SQL files/442 assertions at unchanged thresholds.
    No device ABI/kernel or cost
    coefficient changes, performance claim or new spatial WHERE admission.
    Evidence: `benchmarks/evidence/where-order-20261006/README.md`.
  - [x] Compose one exact point/polygon spatial conjunct with typed INT4/BOOL/
    DATE WHERE programs in fact-only dense integer/scalar rollups. Add a
    validated nullable-Boolean spatial-mask instruction at the native qualifier
    position, so geometry rejection cannot hide an earlier scalar error.
    Preserve existing range fast paths, same-RTE identity, metadata/workspace
    accounting, final conjunction semantics and zero stock fallback; no outer
    ABI layout, cost coefficient or threshold changes. PG18/19 each select all
    24 million-row spatial/predicate/grouping combinations, with native and
    independent oracles, prepared DML/rollback/empty-input checks and late-row
    SQLSTATE parity. SQL122 and SQL129–134 pass 52 assertion groups across both
    majors without warnings. Core 1205, benchmark 584, native 33/33 (54722
    grouped checks, 3911 scalar host/device cases), strict Clippy both majors
    and benchmark crate, docs 21 and coverage helpers 104 pass. Inventory is
    97 SQL files/446 assertions/40 families at unchanged 90% thresholds.
    The public-table spatial/scalar benchmark dispatches normally at 1M/2M:
    warm medians 6.001/8.304 ms versus eight-worker PostgreSQL 38.622/59.426 ms
    (6.436/7.157x), 30 paired samples with exact oracles and zero stock fallback.
    GPU CV is 12.15/14.53%; both refresh-inclusive probes lose. No release-wide
    or matched before/after performance claim is made.
    Spatial OR/multiple spatial predicates, scalar joins/hash/NUMERIC and
    general cross-worker predicate sharing remain open.
    Evidence: `benchmarks/evidence/spatial-where-20261006/README.md`.
  - [x] Add NULL-safe INT4/BOOL/DATE `IS [NOT] DISTINCT FROM` and all six
    Boolean `IS [NOT] TRUE/FALSE/UNKNOWN` tests to typed expressions, WHERE,
    CASE, aggregate FILTER and qualified spatial conjunctions. Opcode 26 has
    canonical operands, full type/CFG proof and a non-NULL result proof;
    both surviving operands still evaluate and raise typed errors. Boolean
    tests use existing lazy branches without duplicate argument evaluation.
    Existing state, memory, program and cost limits remain unchanged.
    PG18/19 each select 48 WHERE and 24 FILTER/CASE/count shapes with native
    and independent oracles, prepared DML/rollback/NULL/empty checks and zero
    fallback. Explicit per-major COUNT tests respect PG19's native nonnullable
    COUNT-to-COUNT(*) rewrite; seven SUM/CASE cases retain operand-error checks
    on both majors. Final SQL130–135 pass 48 assertion groups; live scalar
    lowering/binding tests pass 12 per major. Core 1208, benchmark 585, native
    33/33 (3956 scalar cases/213 shared oracles, 54722 grouped checks) and strict
    Clippy both majors/benchmark pass. General types/casts, row comparisons,
    scalar joins and broad pipeline composition remain open.
    Normal public-table plans dispatch all five new benchmark cells: OLAP
    250K/1M/2M warm medians 2.269/3.292/5.954 ms versus parallel PostgreSQL
    9.553/22.429/38.971 ms (4.210/6.814/6.546x); geo 1M/2M medians
    6.080/9.094 ms versus 56.817/95.726 ms (9.345/10.526x). Thirty paired
    samples per cell match exact oracles with zero fallback; GPU CV is
    12.49–19.86%. All refresh-inclusive probes lose. These are new workloads,
    not matched before/after or release-wide claims. Coverage helpers 104 and
    docs 21 pass; inventory is 98 SQL files/451 assertions/41 semantic families
    at unchanged thresholds.
    Evidence: `benchmarks/evidence/null-safe-scalar-20261006/README.md`.
  - [x] Add typed INT4/BOOL/DATE NULLIF and row-wise GREATEST/LEAST to scalar
    aggregates, FILTER/WHERE, CASE and qualified spatial rollups. Additive
    opcodes 27–29 retain operand evaluation/errors and exact PostgreSQL NULL
    behavior, with full CFG/type/canonical-operand checks. Variadic extrema
    use bounded-stack pairwise reductions; replaced default comparators and
    shadowed equality decline. Existing costs, row/state/memory limits and
    outer ABI remain unchanged. SQL136 selects all 56 OLAP/geo shapes on each
    major against native and independent CASE/coordinate oracles, plus six
    composition shapes, generic prepared DML/rollback/all-NULL/empty inputs,
    and late division/overflow/date errors with zero fallback. SQL42/111 and
    SQL128–136 pass 94 assertion groups across PG18/19. Core 1209, live scalar
    tests 13 per major, benchmark 586, native 33/33 (4211 scalar host/device
    cases, 468 shared oracles, 90 malformed CFGs), strict Clippy both majors
    and benchmark, coverage tooling 104, docs 21 and scope audit pass.
    The initial SQL136 subquery wrapper declined on `shape_unsupported_rte`;
    the final fixture uses direct fact-table aggregates. Initial stale-manifest
    coverage and benchmark Clippy style failures are retained with corrected
    passing runs. Inventory is 99 SQL files/456 assertions/42 families without
    relaxed thresholds. General types/casts, subquery pipelines, joins and
    shared ownership remain open.
    New bounded-ratio benchmarks dispatch all five public-table cells normally:
    OLAP 250K/1M/2M warm medians 2.619/3.639/7.257 ms versus parallel PostgreSQL
    11.834/31.282/56.471 ms (4.518/8.598/7.782x); geo 1M/2M medians
    5.077/7.061 ms versus 60.242/103.093 ms (11.866/14.601x). Thirty paired
    samples per cell, exact native/independent oracles, zero fallback and
    clean provenance. GPU CV is 8.20–20.85%; every refresh-inclusive probe
    loses. This is new query coverage and local warm evidence, not a matched
    before/after optimization or release-wide headline claim.
    Evidence: `benchmarks/evidence/scalar-nullif-extrema-20261006/README.md`.
  - [x] Extend the integer rollup family to independent COUNT(int2/int4) and
    conditional COUNT(*), including shared SUM/AVG/COUNT states for identical
    expressions and predicates. Supported non-NULL-accepting INT4 predicates
    make filtered COUNT(*) exactly equivalent to filtered COUNT(predicate
    input); the predicate is retained. Count-only lanes skip value loads and
    sum arithmetic. Single unfiltered counts retain their smaller count-only
    workspace. Up to four value states (three with unfiltered COUNT(*)) use
    the unchanged ABI and existing atomic, hierarchical and bounded-chunk
    execution. Rejected rows still establish groups; counts return zero while
    sums remain NULL. SQL115 adds 24 selected differential shapes, prepared
    hidden-HAVING/DML/rollback checks and a 1,000,017-row multi-batch case.
    Focused SQL115 passes; standalone Rust tests pass 1112, benchmark tests
    568, native grouped tests 8941 checks and full native CTest 32/32.
    Both Clippy builds and doc/coverage/safety checks pass. The SQL inventory
    now contains 78 files and 392 assertions without lowering existing floors.
    Evidence is `.codex/scratch/filtered-count-*-20260921*`. General/global
    conditional aggregates, unbound predicate inputs, non-integer filtered
    counts, exact NUMERIC inputs and joins remain unfinished.
    The initial full PG19 attempt is retained: SQL96 still expected the now
    supported integer COUNT to report an unqualified kernel. Its negative
    lifecycle case now uses COUNT plus MIN, retaining exact-result, decline
    reason and no-dispatch checks for that unsupported combination.
    A second run confirmed MIN(int2) is rejected earlier by input admission;
    the final case uses MIN(int4) and its focused PG19 test passes the original
    exact serial-kernel decline contract. The other 77 PG19 files passed in
    that full run. The 123-export GPU-execution/ABI audit also passes.
    Full PG18 integration passes 78/78 files in
    `.codex/scratch/filtered-count-pg18-20260921`; PG19's 77 passing files and
    corrected focused SQL96 are retained in `filtered-count-pg19-20260921-b`
    and `filtered-count-sql96-pg19-20260921.log` under the same scratch root.
    Installed module SHA-256 values are
    `c0b4b583b2eb5b17f5145c44434e034d09a32e4b1de5cae5881158660bfe6084` (PG18)
    and `8c661164631c1602a7a34606f74c6b0f10294fe076c75dabf1ac602219bd2fd0` (PG19).
    Fresh pristine/loaded regression and isolation schedules pass all eight
    runs (1448 scheduled executions); both checksum seals verify in
    `.codex/scratch/upstream-filtered-count-pg{18,19}-20260921`. As before,
    these local debug/assertion, no-ICU schedules verify hook/default-path
    compatibility, not GPU execution or selected PostGIS/H3 objects there.
    The new `grouped_conditional_integer_counts` workload verifies all 225
    groups against PostgreSQL and an independent Rust oracle. Ten paired
    warm observations pass exact outputs, provenance, parallel device mode,
    zero stock fallback and the unchanged 1.15x floor: 7.03/14.80 ms at 250K
    (2.10x) and 7.61/41.69 ms at 1M (5.47x), retained in
    `.codex/scratch/filtered-count-warm-20260921`. GPU timing variation is
    30%/25%, so these are not stable headline measurements. Preloads of 60.61/158.43 ms
    are separate; lifecycle first use still loses at 46.19/154.25 ms versus
    native 14.80/43.43 ms. The Boolean SUM/AVG control also passes at
    2.22x/5.13x in `.codex/scratch/filtered-count-control-warm-20260921`.
    Every timed query executes aggregation; only resident inputs and grouping
    artifacts are reused. These dirty-tree local Metal synthetic diagnostics
    do not complete real NUMERIC taxi rollups, CUDA, concurrency or release
    qualification.
- [x] Generalize H3 parent COUNT rollups across constant resolutions 0–15 with
  a separate memory-bounded capacity policy and exact resident-row checks.
  SQL106 verifies all 16 resolutions against h3-pg, including NULLs, all base
  cells/pentagons, prepared queries and hidden HAVING inputs. Current Metal
  PG18/19 suites pass 69/69 files (369 declared assertions), retained in
  `.codex/scratch/h3-compact-pg18-20260921` and the matching `pg19` directory.
  Three new synthetic repeated-NYC-site benchmarks at 1M rows pass exact
  comparison, dispatch and the unchanged 1.15x warm floor: resolution 4 is
  4.04/26.50 ms (6.55x), resolution 9 is 6.76/28.69 ms (4.25x), and resolution
  15 is 6.61/28.32 ms (4.29x). Ten paired observations per query are retained in
  `.codex/scratch/h3-res{4,9,15}-compact-warm-20260921`. Preload costs of
  67.19/69.54/68.41 ms are separate; first-use still loses. These are local,
  dirty-tree Metal diagnostics, not real taxi-corpus or CUDA qualification.
  The initial resolution-4 loss (186.79 ms / 27.09 ms, 0.15x) is retained in
  `.codex/scratch/h3-res4-warm-20260921`; it exposed the compaction bottleneck
  fixed below, not a reason to drop a workload or lower its threshold.
- [x] Accelerate the complete 12,748,986-row NYC H3 rollup, previously declined
  by the conservative capacity limit despite producing only 4,467 groups.
  Hardware-bounded hash capacity now allows up to 16 Mi groups, charged against
  both maximum allocation and resident-memory budgets. Large outputs compact
  and merge-sort in parallel on-device; NULL tagging stays in private count
  lanes until aligned final publication. Row launches are bounded at one
  million, but this remains synchronous one-shot execution, not spill or a
  PostgreSQL cancellation boundary. After exact output validation, immutable
  parent artifacts retain the observed group bound for smaller subsequent
  workspaces; source-generation invalidation discards it. Every execution
  still aggregates every input row; this is not cached query results.
  SQL113 covers large repeated keys, NULLs, 117,650 actual groups, reuse,
  updates and rollback, including independent parent-bit oracles and required
  real dispatch. Full PG18/19 SQL suites pass 76/76 files (385 assertions) in
  `.codex/scratch/h3-large-pg{18,19}-20260921`. Rust tests pass 1099; both
  Clippy versions, 173 audit-verifier tests, safety contracts and 42 doc/coverage
  tests pass. All 32 native CTests pass; the hash suite passes 314 checks,
  including capacity/error nonpublication and recovery with reused scratch.
  It exercises all 16,777,216 distinct groups, including unsigned high-bit keys
  and NULL, rather than merely allocating that capacity. Logs are retained as
  `.codex/scratch/h3-large-ctest-20260921.log` and its `ctest-detail` companion.
  Two ten-pair warm runs pass exact differential/oracle checks, actual GPU
  dispatch and zero stock fallback in
  `.codex/scratch/h3-learned-nyc-warm-20260921` and its `-b` repeat. Full-scale
  medians are 49.14/48.99 ms versus PostgreSQL parallel 205.79/208.46 ms
  (4.19/4.26x); the 1M prefix remains 4.47/4.88x. Separate full-scale preload
  costs are 595/599 ms, and lifecycle observations are 698/702 ms versus native
  204/205 ms: first use still loses. The pre-learning intermediate, retained
  in `.codex/scratch/h3-large-nyc-warm-20260921`, achieved only 1.67x at full
  scale. Failed native-development attempts remain as `h3-parallel-hash-native`
  logs; no failed variant was installed into PostgreSQL. These are local
  dirty-tree Metal diagnostics, not CUDA, concurrent or release qualification.
  The existing resolution-zero control also passes its unchanged warm gates
  across all four registered scales in
  `.codex/scratch/h3-large-legacy-control-warm-20260921`: 10K remains native;
  100K/1M/10M dispatch with exact results at 6.98/5.64/7.23x and zero stock
  fallback. Ten-million-row first use still loses (452 versus 162 ms, with
  separate preload of 482 ms); this is not beyond-budget or streaming evidence.
- [ ] Introduce versioned composable GPU pipelines, costed PostgreSQL/GPU
  boundaries, and relation-occurrence identities distinct from table OIDs.
  - [x] Admit PostgreSQL-flattened derived tables, inline CTEs, views and
    single-row LATERAL report projections over qualified base operations.
    Inactive SUBQUERY entries require independent planner-slot/bitmap and
    security checks; every nested FromExpr filter is retained in native
    qualifier order. Existing cost, residency, memory and alias gates remain.
    Explicit view-owner permissions must match the current loader role;
    generic plans retain role dependencies even through negative-cache hits.
    SQL137 passes on PG18/19: 28 ordinary/geo/global/composite report shapes,
    nested guard ordering, hidden HAVING, prepared DML/rollback/NULL/empty
    input, late division errors, SET ROLE in both directions, column-grant
    revocation and RLS. Live subplans, materialized CTEs, security barriers,
    other-role definer contexts and live self aliases stay native. Core
    tests pass 1211; benchmark tests 587; strict Clippy both majors/benchmark
    and the 100-file/462-assertion/43-family coverage scope audit pass.
    The development installer now establishes an inherited server soft file
    limit of at least 4096 before installation/restart, preserves higher
    limits, and fails before mutation if it cannot raise the soft limit.
    This fixes Metal compilation failures caused by a server restarted with
    macOS's 256-file shell default; kernel limits and retries are unchanged.
    `grouped_derived_ratio` and `spatial_grouped_derived_ratio` provide normal
    public-table benchmarks with independent arithmetic/coordinate oracles.
    Thirty paired warm measurements select all five cells: OLAP 250K/1M/2M
    medians are 2.390/4.078/7.329 ms versus parallel-enabled PostgreSQL
    12.052/32.705/58.045 ms (5.043/8.020/7.920x); geo 1M/2M medians are
    5.452/7.368 ms versus 60.322/108.155 ms (11.064/14.679x). Exact outputs,
    verified parallel dense kernels and zero fallback; all preload-inclusive
    first uses and refresh-inclusive runs still lose. The CTE-inside-derived
    arrangement remains unflattened/native; its original five timing cells
    are retained without GPU credit. The benchmark uses a top-level CTE and
    derived projection, both explicitly covered by SQL137 on PG18/19.
    PG19's full suite passes 100/100 files and 462 assertions, plus the final
    expanded SQL137. PG18 passes 98/100 in its full run; its two resource-limit
    failures pass focused reruns after the server-launch fix, with unchanged
    module SHA. All 1448 upstream regression/isolation executions pass across
    pristine/preloaded PG18/19; both seals verify. This is local Metal evidence,
    not fresh single-run PG18 qualification, CUDA, hosted CI or release proof.
    This is flattened-query admission, not general pipeline execution or
    relation-occurrence-aware joins. Evidence:
    `benchmarks/evidence/pulled-up-reports-20261006/README.md`.
  - [x] Add the bounded JQS1 logical join-stage contract with distinct relation
    occurrences, strict versioned serialization, ordered typed consumer lanes
    and deduplicated physical dependencies. Self aliases retain separate key
    and output identities while sharing resident columns. The staged binder
    ensures the complete dependency set and checks generations, relfilenode,
    type OIDs, row domains and NULL sidecars under one immutable store borrow.
    Direct PG18/19 tests cover 40 join shapes plus DML/rollback, same-generation
    refresh, eviction, disabled auto-load, wrong types and retained outputs.
    Real nullable/nonnullable residency exposed a Metal argument-encoder abort
    in the native row producer. Staging its fixed 352-byte metadata in queried,
    charged workspace fixes the failing live case; native tests now exercise
    asymmetric sidecars for all four key counts and join kinds. AQS6/AOP2 and
    native layouts remain unchanged. This is not full pipeline-graph execution
    or SQL admission: planner/permission proof, costs, bounded/skew-aware work,
    CustomScan lifecycle and the three static export proofs remain open.
    Evidence: `benchmarks/evidence/resident-join-binding-20261009/README.md`.
- [ ] Implement a post-fork GPU owner, process-safe shared immutable residency,
  snapshot/ACL validation, leases, fair scheduling, owner recovery and cleanup.
- [ ] Implement streaming/overlap, partitioned operators, spill and 64-bit
  bookkeeping. Verify pooled 1/4/8-client and 10M/100M/narrow-1B-row execution,
  including working sets beyond the device budget.
- [ ] Add row-producing inner/left/semi/anti joins, composite keys, aliases,
  duplicate multiplicity, and fused join/filter/aggregate pipelines.
  - [x] Add a native resident row-mapping producer plus a budgeted Rust owner:
    one to four INT4/INT8 key pairs, SQL NULL semantics, duplicate-preserving
    inner/left and EXISTS/NOT EXISTS semi/anti joins, read-only self aliases,
    parallel bucket build, hierarchical device prefix sums and resident pairs.
    Capacity exhaustion publishes the required count without partial output;
    temporary charges unwind and successful output retains its own charge.
    Native semantic/contract tests and direct PostgreSQL oracle tests cover
    these primitives. This does **not** add normal SQL planner support or a
    speedup claim. v1 inputs are capped at INT32_MAX rows, with 64-bit output
    cardinalities/offsets; skew-aware partitioning, bounded dispatch/cancel,
    device consumers, relation-occurrence identity, SQL admission and the
    conservative static CPU-work audit's new export proofs remain open.
  - [x] Connect resident row mappings to contiguous GPU consumers with a
    versioned gather ABI and typed, charged Rust output owner. Up to sixteen
    one/four/eight-byte columns retain exact value bits and canonical NULLs,
    including LEFT padding; a GPU bounds pass rejects malformed indices before
    writing output columns. Borrowed source lengths must match the original
    join side. Direct live tests compose join -> gather -> parallel scalar
    SUM/COUNT with aggregate FILTER over both sides and compare native SQL,
    plus exact projected INT8 extremes, duplicates, empty sides and self joins.
    Intermediate columns never need CPU reconstruction. This is execution
    composition, not normal SQL admission, a fused join/aggregate kernel or a
    speedup claim. Production binding/currentness, occurrence identities,
    pipeline wire/costs, streaming/skew and static proof work remain open.
    Evidence: `benchmarks/evidence/resident-join-pipeline-20261006/README.md`.
  - [ ] Qualify the implemented bounded join driver and Rust cancellation owner.
    The additive control ABI caps launch work-items and per-row hash-chain
    steps; count/emit cursors remain resident, including duplicate-heavy joins.
    Polling occurs before work and after each completed fill/kernel. Cancelled
    output is never published; Rust callback panics are contained across FFI
    and resumed after native returns, with allocation/ledger cleanup.
    Native tests cover all four join kinds, one through four keys, NULLs,
    prefix levels, capacity retries and cancellation boundaries. Rust live
    tests add cancellation, panic, exact charges and retry to the 40-shape
    PostgreSQL oracle matrix. The initial native driver passed on Metal before
    sandbox restrictions; the final expanded GPU/live matrix still needs a
    rerun because the current sandbox exposes no GPU. No normal SQL admission,
    DeviceLimits tuning, PostgreSQL interrupt delivery, bounded gather,
    partitioning/spill or speedup claim. Five native export proof findings
    remain open; the audit has not been weakened. Current work/evidence:
    `benchmarks/evidence/resident-join-bounded-20261009/README.md`.
  - [ ] Qualify the implemented bounded gather and combined binder dispatch.
    The controlled gather uses the same limits/callback ABI and fixed workspace;
    all pair-index validation chunks finish before any column writes. Native
    tests add exact bounded launch counts, poisoned-output parity, late-chunk
    invalid indices, writable control aliases and cancellation/retry at every
    boundary for all sixteen columns. The occurrence-aware binder now composes
    controlled join and gather under its existing currentness-checked borrow;
    partial columns and pairs cannot escape on cancellation or callback panic.
    Its 40-case live oracle matrix now exercises both-stage accounting, stage
    transition/late-gather cancellation, charge cleanup, panic and retry.
    GPU/live runtime qualification remains pending; this does not add a SQL
    path, PostgreSQL interrupt delivery, calibrated DeviceLimits or spill.
    PG18/19 core suites pass 1,227 each, including ten direct native preflight
    rejection cases without a GPU; strict Clippy and the native build pass.
    The actual CPU-work audit still rejects six exports, without exemptions.
    Evidence: `benchmarks/evidence/resident-join-bounded-project-20261009/README.md`.
  - [ ] Qualify the implemented PostgreSQL-aware bounded join wrapper.
    Signal polling reads PostgreSQL's volatile flags only, respecting interrupt
    holdoff, critical sections and pending backend-death priority. The wrapper
    services interrupts only after native frames and residency borrows end;
    captured PostgreSQL errors retain their SQLSTATE/message and temporary
    owners drop before rethrow. Non-error interrupts restart with fresh binding
    and include discarded-attempt launches in diagnostics, never CPU fallback.
    Portable tests cover early/post-dispatch errors, owner/borrow cleanup,
    holdoffs, benign retries and generation-change failure. Live tests inject
    PostgreSQL signal flags at producer/gather boundaries and check native
    57014 propagation, benign retries, accounting and backend/store reuse.
    Those live cases are type-checked but pending a GPU-visible backend; external
    SIGINT/termination and real join statement-timeout qualification remain
    open. No SQL admission, limit calibration, spill or performance claim.
    Evidence: `benchmarks/evidence/resident-join-interrupts-20261009/README.md`.
  - [ ] Qualify the implemented JRS1 join-to-global-reduction consumer.
    A bounded, versioned wire maps up to four INT4 scalar SUM/COUNT programs
    to exact BOOL/INT4/DATE producer slots, preserving aliases without fake
    relation OIDs. Decode rejects malformed sections/programs before owned
    allocation; type mismatches, external predicate-mask context and invalid
    chunk limits fail before producer execution. The binder composes controlled
    join/gather with the existing bounded parallel aggregate session, keeping
    all joined rows resident. Fixed metadata encoding avoids transient heap
    program copies; copied plan/session/chunk metadata remains ledger-charged.
    Final states preserve SUM NULL semantics; interrupted/failed consumers
    release owners before PostgreSQL error propagation. The 40-shape live matrix
    adds native SUM/COUNT/FILTER/date oracles, capacity handling, type rejection
    and accounting. Core PG18/19 suites pass 1,239 each and strict Clippy passes;
    GPU/live execution remains pending. This does not qualify SQL admission,
    a fused join/reduce kernel, grouping, broad expression types or performance.
    Evidence: `benchmarks/evidence/resident-join-reduce-20261009/README.md`.
  - [ ] Qualify post-join WHERE in the resident reduction consumer. JRS1 wire
    version 2 adds an exact BOOL predicate over joined BOOL/INT4/DATE lanes;
    strict version 1 decoding remains supported. The predicate uses an internal
    scalar carrier, leaving up to three measures within the four-lane ABI.
    The existing device WHERE evaluator rejects FALSE/NULL before measures and
    their independent aggregate FILTER expressions. LEFT null-extension stays
    before WHERE; this is not residual ON support or predicate pushdown. Joined
    and selected counts are separately validated and reported. The live matrix
    adds eight PostgreSQL WHERE oracles per join shape, rejected-row division
    guards, predicate-error propagation, cleanup and retry. Portable wire tests
    cover old/new versions, exact maximum size, malformed sections and SQL type
    mismatches. GPU/live execution, SQL planner admission and performance remain
    unqualified; the current sandbox still exposes no GPU. Evidence:
    `benchmarks/evidence/resident-join-where-20261009/README.md`.
  - [ ] Qualify consumer-driven gather pruning. The reduction executor now
    gathers only declared measure/WHERE inputs, deduplicates repeated logical
    columns and preserves distinct self-join occurrences. Fixed metadata maps
    retain logical wire slots without program copies. Constant-only consumers
    preserve the verified device row domain without gather allocations or
    launches, while retaining the cancellation boundary and GPU aggregation.
    Source dependency checks and join-pair production are unchanged. Diagnostics
    report actual projection columns and bytes; added tests cover slot remapping,
    alias separation, zero-column cancellation/panic, duplicate SQL expressions,
    exact byte accounting and constant SUM/COUNT across the live join matrix.
    GPU/live execution and performance remain pending, with no SQL admission
    or measured speedup claim. Evidence:
    `benchmarks/evidence/resident-join-prune-20261009/README.md`.
  - [ ] Qualify exact join-cardinality reductions. The new bounded native count
    entry point produces a successful scalar cardinality directly from GPU
    build/probe/upward-prefix work, skipping offset propagation, pair emission,
    gather and aggregate-session dispatch. The JRS1 consumer selects it only
    for exact unfiltered literal SUM(1)/COUNT(*) states; empty SUM stays NULL
    and bigint publication is checked. Pair capacity is not a logical row limit
    when no pairs are allocated. OOM remains an error; success does not inflate
    dispatch-failure counters. Existing dependency/currentness and PostgreSQL
    interrupt boundaries remain. Native and live tests add count parity,
    cancellation/retry, scratch-only budgets, real OOM, multiple output states,
    exact counters and zero/unbounded pair-capacity inputs. PG18/19 core suites
    pass 1,249 each, including eight native no-GPU preflight rejection cases;
    strict Clippy and native build pass. GPU runtime, normal SQL admission,
    calibrated costs and performance remain unqualified. The additive export
    is included in the 130-entry ABI inventory, without a static-audit waiver;
    173 analyzer tests pass, but the actual join audit has seven unresolved
    export proofs, including the new count path.
    Evidence: `benchmarks/evidence/resident-join-count-20261009/README.md`.
  - [ ] Close the metadata-only and device-work proofs for the join exports.
    Workspace queries now have a SYCL-free translation unit, sharing their
    unchanged layout/validation helpers with execution. The independent host
    CTest passes 40,389 checks using inaccessible row buffers, including exact
    sizes through INT32_MAX, NULL sidecars, aliases, invalid descriptors and
    callback exclusion. The same matrix passes with sanitizers and against the
    rebuilt shared library; PG18/19 core suites pass 1,249 each. These are
    runtime metadata-boundary checks, not static helper-chain proofs or GPU
    qualification. All 180 analyzer tests pass, but the actual source audit
    still rejects seven exports across the execution/layout translation units.
    No audit exemption or SQL admission has been added.
    Evidence: `benchmarks/evidence/resident-join-layout-20261010/README.md`.
    The staged `metadata_effects.py` verifier now checks Clang-resolved helper
    calls, metadata pointer permissions, fixed array/loop bounds, total work
    and storage bounds, and scalar output publication. Both query bodies pass
    its typed effect check; 19 regressions include compiler-valid hostile helper
    changes and other escapes. Metadata loops now have explicit fixed
    bounds, with unchanged byte forecasts in the 40,389-check runtime matrix.
    This verifier is tested locally/in CI configuration but deliberately does
    not approve exports in the release audit yet. Bind its source/header and
    preprocessor evidence to the actual audited build before integrating it;
    the conditional-helper regression demonstrates why one AST is insufficient.
    Arithmetic and alias-contract correctness still require their own tests.
    Evidence: `benchmarks/evidence/resident-join-metadata-proof-20261010/README.md`.
    - [x] Bind the metadata proof to the object built by CMake. Both libraries
      and the host matrix now link one metadata object compiled with Clang from
      a closed, frozen source/header filesystem; the proof uses the same inputs,
      flags and mandatory diagnostic profile. Compiler/source/header/profile,
      verifier and object digests are recorded in a replayable build receipt.
      Helpers have internal linkage, so another translation unit cannot replace
      their definitions. Header edits trigger dependency rebuilds; unknown
      compiler flags, hidden startup work, stale receipts and diagnostic pragmas
      that suppress VLA/return errors fail closed. All 35 metadata tests pass,
      including actual object execution, post-proof header mutation, helper
      substitution and instrumented source/header coverage. The 40,389-check
      matrix passes against the built shared library and with sanitizers;
      PG18/19 core suites pass 1,249 each. Metadata coverage retains original
      filenames (100% query-source lines, 94.07% helper-header lines), without
      removing any source or lowering the full coverage gate. Evidence:
      `benchmarks/evidence/resident-join-metadata-build-20261010/README.md`.
    - [x] Integrate independently replayed metadata receipts with the release
      audit's actual linked-object/symbol inventory. A fresh library link now
      records export ownership/address evidence; the audit independently
      re-proves and rebuilds the metadata object, checks identical object bytes,
      and validates compiler, current inputs, artifact hashes and linker-map
      ownership. Receipts cannot select executables or supply proof booleans.
      Metadata approvals are report-only, have no GPU-output provenance, and
      cannot enter the external device-proof index. Parser/preprocessor and
      ABI errors remain failures. The complete release invocation now approves
      both workspace queries, with all 130 ABI exports accounted for. The
      existing 180 analyzer tests, 35 effect/build tests and final 11 linkage
      tests pass; the rebuilt shared-library matrix passes 40,389 checks.
      Apple ld/GNU ld/LLD map regressions include a real cross-compiled ELF map,
      without a Linux runtime or CUDA qualification claim. Evidence:
      `benchmarks/evidence/resident-join-metadata-link-20261010/README.md`.
    - [x] Close indexed ABI-output escapes in the device-work analyzer before
      expanding workspace-pointer helper proofs. A compiler-valid witness
      previously passed after saving a GPU output pointer in a host array and
      overwriting it through an argument-free helper. Host indexed assignments
      and initializers carrying output-derived state now require review, even
      on failure-only paths. Braced array initializers are no longer mistaken
      for lambda bodies. Compiler-valid negative and positive controls cover
      casts, aliases, member/nested arrays, host lambdas and device stores;
      the local recipe and no-GPU CI configuration include these regressions.
      This closes a false approval, not the remaining device-work proofs or
      runtime qualification. Evidence:
      `benchmarks/evidence/output-container-proof-20261010/README.md`.
    - [x] Establish compiler-resolved address-only body proofs for workspace
      helpers. `workspace_effects.py` checks the actual preprocessed translation
      unit, including concrete instantiations, and proves only base-derived
      pointer returns with no row reads, writes, callbacks or object lifecycle.
      The grouped helper's 11 definitions/instantiations and the join helper's
      six pass; ten regressions include hostile specializations, dynamic/TLS
      globals, pointer reconstruction and class conversions. A caller-conversion
      counterexample explicitly shows why this body proof cannot approve a
      call site. The release audit does not consume these results: compiler
      profile/build binding, call-site effects and returned-alias propagation
      remain required before integration. No device finding is waived.
      Evidence: `benchmarks/evidence/workspace-address-proof-20261010/README.md`.
    - [x] Collect compiler-resolved argument and returned-alias facts for the
      workspace-helper calls in grouped binding/launch and join execution.
      Caller and selected callee share one AST identity space; overloads,
      specializations, implicit conversions and escaping helper references
      cannot be approved by name. Metadata record parameters are explicit,
      row-pointer reads fail, and source-bound declared-type checks reject
      reference fields that look like scalar values in expression nodes.
      All 59 direct calls in the five inspected functions have destination
      bindings, including member/array destinations and two conditional aliases.
      Local value reads remain recorded caller obligations. The 24 workspace
      tests pass. These are source facts only, not full caller or device proofs;
      branch-sensitive alias propagation, initializer effects, bounds and exact
      build binding remain open before release integration. Evidence:
      `benchmarks/evidence/workspace-call-proof-20261010/README.md`.
      Follow-up: call facts now require the incoming workspace pointer value
      to remain stable throughout the complete caller. Reassignment, reference/
      address escape, writes after a call and loop-backedge changes fail closed;
      a matching declaration ID alone is no longer accepted as root provenance.
      All 59 actual-source calls still pass this stronger prerequisite, and
      28 workspace tests cover positive copies/shadowing and hostile mutations.
      Pointed-to memory effects, lifetime, local offset initializers, conditional
      aliases and build binding remain separate obligations. Evidence:
      `benchmarks/evidence/workspace-base-proof-20261010/README.md`.
      The next stricter pass now checks offset-dependent local initializers,
      scalar updates/escapes (including later writes and transitive dependencies),
      and trivial metadata record-copy construction/destruction. Hidden row
      reads/callbacks in those expressions fail. All 40 focused workspace and
      indexed-escape tests pass. It proves 45 calls in the three grouped callers
      and ordinary join execution; controlled join execution now deliberately
      declines its complete 14-call set because `level` escapes through a
      reference-capturing callback. Closure/control-predicate effects, alias
      propagation, bounds and build binding remain open; no release finding is
      waived. Evidence:
      `benchmarks/evidence/workspace-input-proof-20261010/README.md`.
      Controlled scan follow-up: the upward-prefix callback now captures only
      its immutable row bound by value. The nested GPU closure no longer needs
      the host level record. All 59 inspected calls pass the stricter workspace
      checker again without an exemption. The native library/fixtures compile,
      the host layout matrix passes 40,389 checks, and PG18/19 each pass 1,268
      core tests plus strict lint. That October 10 checkpoint could not qualify
      device execution because no GPU was visible. The October 12 full native
      Metal run subsequently passed all 36 CTests, including joins; see
      `benchmarks/evidence/native-live-20261012/README.md`. This closes the
      specific metadata reference-capture obstacle, not general callback/device
      proofs or SQL admission. Evidence:
      `benchmarks/evidence/join-scan-capture-20261010/README.md`.
    - [ ] Close the five remaining join execution/count/projection device-work
      findings and the two grouped-aggregate findings exposed by the complete
      release scan (`pgaccel_grouped_agg_execute_ex` and its wrapper). The full
      audit still fails on these seven exports; the prior join-only scan did
      not cover the grouped-aggregate failures. No device-work exemption was
      added. The October 12 full native Metal run passes all 36 CTests, replacing
      the earlier no-visible-device/exit-77 blocker, but runtime passes do not
      discharge these static proofs. Normal SQL join admission remains open.
- [ ] Add point/polygon Intersects/Contains/Covers joins with holes and
  multipolygons, spatial candidate indexes, exact rechecks, geometry proximity
  joins, and exact lat/lng-to-H3 compositions.
  - [x] Admit Covers(area, point) and CoveredBy(point, area) in normal resident
    ungrouped COUNT planning for valid constant Polygon/MultiPolygon geometry.
    Preserve distinct logical wire tags, artifact identities and EXPLAIN labels;
    reuse inclusive GPU point/area classification but call the original exact
    PostGIS function for uncertain boundaries. Both functions join the exact
    extension-owned catalog fingerprint. Reversed arguments and unsupported
    shapes remain native, with existing residency/SRID/work/cost/memory gates.
    SQL112/114 extend Cartesian and PostGIS oracles across holes, component
    islands, NULL/EMPTY points, prepared plans, DML and rollback. Invalid
    overlapping components remain native. Targeted spatial SQL19/20/92/112/114
    pass without warnings on PG18/19; 1137 core tests, 571 benchmark tests, both
    production Clippy builds, coverage/safety and documentation checks pass.
    Matched 1M/2M warm medians versus parallel PostgreSQL are 17.04x/24.74x
    for Covers and 15.41x/22.05x for CoveredBy; Intersects controls remain
    15.94x/20.61x. All six cells match exactly, dispatch and have zero fallback.
    These synthetic full-scan results reuse validated predicate masks, without
    GiST. Separate loads cost 231–445 ms; refresh plus new-mask construction
    costs 293–575 ms and still loses to native execution. This is not first-use,
    indexed, concurrent, CUDA or release qualification. Durable evidence:
    `benchmarks/evidence/covers-20260929/README.md`.
    This closes the constant-area predicate gap, not row-producing spatial
    joins, proximity joins or general geometry-pair predicate support.
  - [x] Generalize the normal resident point/constant-Polygon COUNT path beyond
    exactly one million rows and a 1,025-coordinate single-ring fixture.
    Intersects in both orientations, Contains(polygon, point), and
    Within(point, polygon) now admit holes under the existing SRID, residency,
    work, cost and memory gates. Synchronous chunks shrink with polygon
    complexity to at most 64 Mi coordinate visits / 65,536 points; preflight,
    cost and artifact accounting share that bound. Uncertain classifications
    still receive exact PostGIS rechecks. A transient missing-residency
    rejection is no longer cached as a permanent descriptor failure, allowing
    the same SQL to dispatch after explicit reloading following DML.
    SQL112 compares 1,000,019 points (including NULL/EMPTY) with two-hole
    polygons at 1,027 and 4,099 coordinates against PostGIS and a Cartesian
    oracle. It covers shell/hole boundaries, DML/rollback, reload and all-NULL
    input. Full PG18/19 suites pass 75/75 files (383 declared assertions),
    retained in `.codex/scratch/spatial-expansion-pg{18,19}-20260921`.
    Rust tests pass 1098, benchmark tests 566, native CTest 32/32, both Clippy
    versions pass, and doc parity, safety and semantic coverage checks pass.
    The three `spatial_holes_{intersects,contains,within}` benchmarks each
    compare exact results with PostgreSQL and an independent integer oracle,
    then record ten paired warm timings at 1M and 2M with real dispatch and
    zero stock fallback. Respectively, warm medians are 3.50/48.99 ms (14.00x)
    and 3.95/82.41 ms (20.85x); 3.33/49.29 ms (14.82x) and 3.89/81.77 ms
    (21.02x); 3.58/49.25 ms (13.75x) and 3.97/81.44 ms (20.51x).
    These are repeated-query cached-mask wins, not fresh-predicate wins:
    preloads cost 206–220 ms at 1M / 389–391 ms at 2M, while lifecycle
    first-use costs 714–739 ms / 1396–1436 ms and loses to native execution.
    Full evidence is `.codex/scratch/spatial_holes_*-warm-20260921`.
    The existing simple-polygon control also passes at 3.32/48.31 ms (14.56x)
    in `.codex/scratch/spatial-expansion-control-warm-20260921`.
    Fixtures are synthetic full scans without GiST indexes; this is local
    dirty-tree Metal evidence, not real-data, CUDA or release qualification.
    Failed focused attempts are retained as `spatial-topology-pg18-20260921`
    and `-b`/`-c` logs; `-d` verifies the residency-cache fix. Constant
    multipolygons are covered by the next milestone; spatial joins, Covers
    and first-use performance remain unfinished.
  - [x] Admit valid constant MultiPolygon geometry in the normal resident
    point COUNT path: Intersects in both orientations, Contains(area, point)
    and Within(point, area), under the existing SRID, cost, work and memory
    gates. The resident ring lane tags each component shell in bit 63 without
    changing ABI layouts or adding a retained GPU lane. Masked offsets are
    validated before use; components are unioned independently, with their
    own holes, rather than applying parity across all rings. EMPTY components,
    NULL/EMPTY points, uncertain boundaries and exact PostGIS rechecks retain
    their semantics. PostGIS-invalid constants remain native; general
    nonpoint multipolygon predicates, joins and Covers are not newly admitted.
    SQL114 covers four components, an island inside a hole, all orientations,
    independent Cartesian/PostGIS oracles, DML/rollback, actual dispatch and
    invalid-topology rejection. Final PG18/19 suites pass 77/77 files with
    388 declared assertions in `.codex/scratch/multipolygon-pg18-20260921-b`
    and `.codex/scratch/multipolygon-pg19-20260921`. Rust tests pass 1101;
    both Clippy versions, safety contracts and 42 doc/coverage tests pass.
    Native CTest passes 32/32, including 147 resident-spatial checks; logs
    are `.codex/scratch/multipolygon-ctest-20260921.log` and its `ctest-detail`
    companion. Parser tests reject truncated/nested/wrong-child bodies,
    oversized counts, malformed component tags and nonmonotone offsets.
    The complete pinned JFK boundary now dispatches against real taxi points.
    Two ten-pair warm runs at 1M rows record 4.539/4.189 ms versus PostgreSQL
    34.814/34.707 ms (7.67/8.29x), with exact outputs and zero stock fallback,
    in `.codex/scratch/multipolygon-nyc-warm-20260921` and
    `.codex/scratch/multipolygon-nyc-scan-warm-20260921-b`. These reuse the
    predicate mask, not fresh geometry classification. Separate preload costs
    203–210 ms; first-use lifecycle costs 907–914 ms and still loses.
    All 12,748,986 rows also dispatch correctly, but warm medians vary from
    17–31 ms versus native 904–1053 ms; high variation prevents a reliable
    full-scale speedup claim. Full-scale mask construction costs 11.2–16.1 s,
    lifecycle 13.6–20.2 s and separate preload 2.36–3.59 s. This exposes the
    next spatial performance target, not a first-query win. Indexed airport
    queries keep GiST available and remain native on cost; exact NUMERIC fare
    rollups still decline. These local dirty-tree Metal runs are exploratory,
    not CUDA, cold/concurrent or release qualification.
    The initial PG18 suite is retained in
    `.codex/scratch/multipolygon-pg18-20260921`: 74/77 passed, with dispatch
    warnings above the unchanged 5-second limit in SQL104/108 and an incorrect
    SQL114 expectation that invalid topology should dispatch. SQL114 now
    requires native rejection. SQL104/108 pass unchanged in the isolated
    rerun; concurrent builds/upstream tests during the first run do not prove
    the warnings' cause or qualify cold/concurrent execution.
- [ ] Add exact DISTINCT, high-cardinality grouping, common string predicates,
  dictionary reuse, top-k/partitioned sorting, ROW_NUMBER and explicit-ROWS
  running SUM/COUNT/AVG. Keep unqualified semantics native.
  - [ ] Bind and qualify exact WHERE masks for high-cardinality hash queries.
    The native single-key path now accepts exact SQL WHERE before key/H3 and
    measure evaluation, independently of aggregate FILTER. Rejected rows do
    not create groups or consume capacity; FILTER still preserves zero-count
    groups. Dedicated kernel variants retain unfiltered row code. Queried
    workspace reserves 1,024 extra bytes only for WHERE shapes, using 256
    device selection-counter shards independent of aggregate validity. Rust
    memory forecasts include the exact extra charge. Native metadata checks
    pass with inaccessible row/mask storage and cover both key widths, value
    states, malformed descriptors and row/capacity boundaries. GPU fixtures
    add WHERE/FILTER/NULL distinctions, hidden malformed inputs, fused H3,
    both compact sorts, million-row launch boundaries, capacity, aliasing,
    late failure and workspace retry. They compile but cannot execute here:
    both grouped GPU suites fail at device initialization. Live PG18/19
    equivalence, cost/performance qualification and SQL admission remain
    unfinished.
    Evidence: `benchmarks/evidence/hash-where-20261010/README.md`.
    The staged Rust binder now owns separate execution-charged WHERE/FILTER
    masks, tracks the WHERE column in resident dependencies, bounds selected
    counts and preserves exact COUNT(*) partition checks. All-rejected raw-key
    input can learn capacity one; predicate DML/rollback rebuilds that bound.
    H3 WHERE uses explicit fused policy so cached parent validation cannot
    evaluate rejected cells before WHERE. Cached-parent WHERE is rejected;
    mask-aware reusable transforms remain open. Host tests cover bindings,
    wrong types/shapes, dependency sets, empty ownership, selected-row bounds,
    capacity learning and two-mask work estimates. Compiled live fixtures add
    cold/warm parity, predicate-only DML/rollback, empty input, both-mask
    cancellation cleanup/retry and H3 policy enforcement. GPU/live parity,
    general expression predicates, cache reuse, performance and normal SQL
    admission remain unfinished. Evidence:
    `benchmarks/evidence/hash-where-binding-20261010/README.md`.
    Follow-up: identical validated direct Boolean WHERE/FILTER now uses one
    physical mask and no redundant per-measure predicate. Forecasts charge one
    conversion, logical dependencies remain intact, and COUNT(*) retains exact
    selected-row partition validation. Distinct and invalid predicates are not
    folded. Host checks and compiled live parity/cancellation fixtures cover
    the shared-predicate case; GPU execution/performance remain unqualified.
    2026-10-12 live follow-up: both complete ten-fixture staged hash modules
    now pass on PostgreSQL 18.4 and 19beta1 with Apple M2 Max/Metal. This covers
    independent and identical WHERE/FILTER, nullable typed counts, integer
    rollups, raw INT2/INT4/INT8 keys, budget/capacity learning, invalidation,
    cancellation cleanup and retry. The native hash suite passes 10,309,427
    checks and the general grouped suite passes. Two previously unexecuted
    fixtures required corrections (empty-input output nullability and signed
    minimum SQL cast precedence); their original failures are retained.
    These results supersede the no-device runtime status above, not the open
    expression predicates, cache reuse, performance, static-proof and normal
    SQL admission gates. Evidence:
    `benchmarks/evidence/hash-live-20261012/README.md`.
  - [ ] Qualify direct BIGINT fact-key hash aggregation. The staged Rust raw-key
    owner now binds resident INT64 values/NULLs directly, with no H3 extension,
    key copy, dictionary or host distinct scan. Query-owned metadata uses the
    generation-checked ephemeral resolver and cleanup-before-interrupt-service
    driver. Native COUNT and INT4 SUM/AVG states, exact Boolean FILTER and
    compact output validation are shared with the H3 path; signed BIGINT bits
    survive native unsigned key ordering. Initial capacity is capped by the
    configured/native group budget without restricting input rows to that
    budget; native overflow fails without partial publication. A validated
    result teaches the exact group count for smaller warm workspaces, tied to
    the current dependency generation. Failed/cancelled attempts cannot teach
    a new bound; mutation/rollback rebuild the initial budget. Host tests
    cover binding, dependencies, signed extremes and capacity limits; a live
    real-catalog fixture covers 16,388 groups, updates, rollback and cancellation.
    Another compiled live fixture covers 1,000,017 rows in a 16-group budget,
    capacity failure/retry, learned-count invalidation and empty input.
    Hash workspaces/results (including cached H3 and COUNT-only) now reserve
    the execution ledger before allocation; previously only grouped NUMERIC
    and scalar families did so. Idle pools retain charges and checkout does
    not double-charge. Side-effect-free native workspace/output forecasting
    preflights the combined working set with credit for locally reclaimable
    charged pools; atomic per-allocation reservations remain authoritative.
    Host tests cover exact footprint/budget boundaries, pooled ownership and
    overflow. A compiled live fixture checks denial before GPU dispatch, warm
    reuse and exact pool-charge release. GPU/cross-backend qualification of
    this accounting change remains open.
    Normal SQL remains on its established dictionary route. Actual GPU/live
    execution, measured performance and planner/cost admission remain open.
    Evidence: `benchmarks/evidence/raw-bigint-hash-20261010/README.md` and
    `benchmarks/evidence/hash-capacity-learning-20261010/README.md` and
    `benchmarks/evidence/hash-memory-accounting-20261010/README.md`.
    The staged native/Rust owner now also accepts SMALLINT and INTEGER keys
    from their existing INT32 resident lanes. Device key loads sign-extend
    without copying/widening the input; compact output remains canonical INT64.
    Output allocation, pooling and budget forecasts agree on eight-byte keys.
    Logical INT2/INT4 output bounds are checked before cardinality learning,
    with typed PostgreSQL key Datums. H3 remains INT64-only. Host tests cover
    all three integer key types and invalid output states; protected metadata
    and device fixtures cover both physical widths, COUNT/value states, FILTER,
    NULLs, signed endpoints, serial/parallel sorting and publication failures.
    Live narrow-key fixtures compare 16,388 groups, warm reuse, DML/rollback,
    all-NULL and empty input against PostgreSQL; execution remains unqualified.
    Normal SQL admission is unchanged. Evidence:
    `benchmarks/evidence/hash-narrow-keys-20261010/README.md`.
  - [ ] Integrate and qualify exact INT2/INT4 hash SUM/COUNT states. The staged
    native single-INT64-key path now accepts one INT32 SUM|COUNT measure,
    including exact SQL FILTER and fused H3 keys. Two-word atomic addition is
    exact under the existing UINT32_MAX row cap; both positive and negative
    bounds are compile-time checked. NULL/rejected values retain their group,
    carry zero non-NULL count and do not load the measure payload. Serial and
    parallel compact sorting retain the sum/count/key association, and one
    shared count supplies SUM validity. Dedicated sum variants leave COUNT
    row kernels free of sum loads/atomics. Protected metadata tests and GPU
    fixtures cover workspace queries, extreme values, all-NULL/rejected groups,
    late failure, reset/retry, alias/capacity rejection and H3 validation.
    Cached/fused Rust binding now resolves INT4 SUM/AVG/COUNT output subsets and
    validates compact SUM widths, matching COUNT/validity lanes and INT4 sum
    bounds before existing integer materialization. Logical SUM/AVG retains an
    internal COUNT even when it is not projected; COUNT-only stays specialized.
    Host tests exercise every subset/order, FILTER/dependencies, rejected types
    and corrupt states. Real-catalog pg_test fixtures cover exact NUMERIC AVG
    (including repeating decimals and NULLs) without a projected COUNT. A
    real-catalog pg_test fixture covers NULLs, signed extremes, cached/fused
    materialization, updates, rollback and empty input. Actual device/live
    execution and measured performance remain open; SQL admission is unchanged.
    Evidence: `benchmarks/evidence/hash-int4-sum-20261010/README.md` and
    `benchmarks/evidence/hash-int4-sum-binding-20261010/README.md` and
    `benchmarks/evidence/hash-int4-avg-20261010/README.md`.
    SMALLINT now uses the same staged Rust path through its existing losslessly
    widened INT32 resident lane. Logical type identity remains checked, and
    completed sums must satisfy the input type's own count-scaled bounds.
    Cached/fused host matrices cover both types and all output subsets/orders;
    expanded real-catalog fixtures cover SMALLINT H3 SUM/AVG/COUNT and raw
    BIGINT-key high-cardinality rollups, FILTER, NULLs, warm reuse, rollback and
    interruption. These live fixtures are compiled, not executed on this host;
    both PG18/19 standalone suites pass 1,299 tests with expanded cases, and
    production/pg_test strict lint passes on both. SQL admission and
    qualification remain open. No native/ABI change or new
    speedup is claimed. Evidence:
    `benchmarks/evidence/hash-int2-binding-20261010/README.md`.
    Native hash MIN/MAX is now staged for the same INT32 column, with any
    SUM/MIN/MAX combination plus COUNT validity. Signed 32-bit atomics, exact
    widening, NULL-state publication and both compact sorting routes carry
    the extra states. Queried workspace allocates only requested bucket and
    merge lanes; the public ABI is unchanged. Protected-input native metadata
    checks pass 2,489 cases. Runtime fixtures cover per-group-distinct extrema,
    signed endpoints, NULL/FILTER, 16,385-group parallel sorting, 1,000,017-row
    launch boundaries, pooled scratch reset, capacity failure, aliases and
    empty input. They compile, but the two grouped GPU suites fail because no
    device is visible. The staged Rust binding now accepts INT2/INT4 MIN/MAX
    alongside SUM/AVG/COUNT, requests only needed value lanes, and retains
    hidden COUNT validity. Compact validation rejects out-of-domain extrema,
    inconsistent SUM/MIN/MAX states and malformed or mismatched count lanes.
    Host matrices cover all 31 subsets and 120 full-output permutations;
    live fixtures add typed extrema and mixed output orderings to cached/fused
    H3 and high-cardinality raw BIGINT-key rollups. Live/GPU qualification and
    SQL admission remain unfinished. Evidence:
    `benchmarks/evidence/hash-native-extrema-20261010/README.md` and
    `benchmarks/evidence/hash-extrema-binding-20261010/README.md`.
  - [ ] Qualify and integrate native hash COUNT(column). The staged single-key
    kernel now counts canonical NULL sidecars for seven fixed-width physical
    types without reading value payloads, preserving zero-count all-NULL
    groups and ordinary key validation. Its workspace equals COUNT(*) for the
    same shape; the unused duplicate nonnull state is not allocated. Protected
    metadata-query tests pass without a device; GPU tests cover empty/NULL
    inputs, high-cardinality sorting, capacity failure, malformed sidecars,
    retry and fused H3 keys. Cached/fused Rust binders now resolve typed measure
    inputs with exact value/NULL lengths; logical COUNT semantics govern
    compact-output checks, allowing zero-count groups without weakening
    COUNT(*) partition checks. Host tests cover empty binding for nine SQL
    types, dependency union, invalid bundles and nullable-count invariants.
    A pg_test-only live qualification matrix now compares final PostgreSQL
    tuples for all nine types in forced cached and fused execution, including
    zero-count/NULL-key groups, NULL-to-non-NULL input transitions, warm reuse,
    value- and key-only DML, rollback of rows loaded inside a subtransaction,
    truncate and empty-to-nonempty
    reload. It requires actual device availability rather than passing a
    no-GPU skip, and asserts prepared-plan rebuilds after invalidation. These
    tests still require live execution; compilation is not parity evidence.
    Rust admission remains closed until GPU/live PG18/19 output equivalence,
    nonempty resident binding/currentness and performance qualification pass.
    This does not complete general high-
    cardinality aggregation, DISTINCT or multi-measure hash support. Evidence:
    `benchmarks/evidence/hash-count-column-20261010/README.md`.
    Rust integration evidence:
    `benchmarks/evidence/hash-count-binding-20261010/README.md`.
    Live-test implementation and compile evidence:
    `benchmarks/evidence/hash-count-live-tests-20261010/README.md`.
  - [ ] Integrate and qualify hash count FILTER for conditional H3 rollups.
    Native COUNT(*) and COUNT(column) now accept a canonical exact SQL measure
    mask with dedicated filtered launch variants. FALSE/UNKNOWN suppress the
    measure, not group creation; zero-count groups and selected row totals are
    preserved. Group-key/H3 validation runs even for rejected rows, while the
    counted column's NULL sidecar is evaluated only after FILTER accepts.
    Malformed evaluated masks/NULL bytes suppress all output publication.
    Workspace sizes and ABI are unchanged. The protected-memory metadata
    matrix passes 1,901 checks; GPU fixtures compile for seven physical types,
    both compact-sort paths, zero counts, capacity failure, retries and fused
    H3 validation, but cannot execute without a visible GPU. Rust predicate
    residency/binding, filtered output checks, live PostgreSQL parity and
    measured performance remain open; SQL admission is unchanged. Evidence:
    `benchmarks/evidence/hash-filter-20261010/README.md`.
    The staged cached/fused Rust binders now resolve direct fact Boolean
    predicates under the resident borrow and retain an owned, one-byte-per-row
    execution-charged mask through dispatch. An additive device converter
    normalizes value/NULL lanes in bounded aligned chunks; malformed evaluated
    bytes remain hard-error markers for the count kernel, never SQL UNKNOWN.
    Predicate dependencies and filtered COUNT(*) output bounds are explicit,
    while unfiltered partition checks and normal SQL admission remain unchanged.
    Added live qualification cases cover predicate-only DML, rollback, nullable
    masks and 65,537-row conversion. GPU/PostgreSQL execution, general expression
    filters, reuse/fusion and performance qualification remain open. Evidence:
    `benchmarks/evidence/hash-filter-binding-20261010/README.md`.
    Both PG18/19 pass 1,271 standalone tests and strict production/live-test
    lint. Native pre-device checks pass 1,909; the GPU suite fails because no
    device is visible. The new converter passes the unchanged device-work
    checker: the expanded 131-export linked audit has 124 passes and the same
    seven unresolved findings, with no exemption.
    Cached-parent artifacts now retain the Boolean mask with the parent lane
    under one exact nine-byte-per-row reservation and the same dependency/
    snapshot identity. Predicate columns are included in artifact construction;
    warm dispatch borrows the retained mask instead of allocating/reconverting
    it. The fused path remains query-local and execution-charged. Policy
    forecasts include both paths' mask bytes and bounded conversion launches.
    This adds one retained byte per cached row, not a peak-memory reduction.
    Live fixtures now require exact cached accounting and removal of two mask
    submissions on a 65,537-row warm query; these assertions still need GPU
    execution. General expressions, fusion, cost/performance and runtime
    qualification remain open. Evidence:
    `benchmarks/evidence/hash-filter-cache-20261010/README.md`.
    PG18/19 each pass 1,274 standalone tests plus strict production/live-test
    lint. Host tests cover exact lane/accounting checks, overflow, predicate
    dependencies and chunk-boundary cost inputs. No fresh GPU result or query
    speedup is claimed; native code and its previous seven audit findings are
    unchanged.
    Cached preparation now polls signal flags between parent/mask calls and
    before publication. Interrupted builds release unfinished lanes, the store
    borrow and the full reservation before PostgreSQL interrupt delivery;
    benign interrupts retry through fresh resident resolution. Host tests cover
    mask boundaries, pre-FFI cancellation, typed launch failure and actual
    reserve-first rollback/rebinding. Live fixtures assert original SQLSTATE
    57014/message, exact charge cleanup, every parent/mask boundary, retry and
    benign wakeups, but still require GPU-visible execution. Fused query-local
    mask polling, in-flight cancellation and full runtime qualification remain
    open. Evidence: `benchmarks/evidence/hash-preparation-cancel-20261010/README.md`.
    Fused query-local masks now poll before allocation and between/after
    conversions. H3 execution delivers interrupts outside both the ephemeral
    artifact and resident-store guards; partial masks and grouped outputs drop
    before rethrow. Benign wakeups rebind/retry, while stale dependencies still
    receive at most one artifact rebuild. Cached one-shot dispatch also gains
    outside-borrow interrupt delivery. Live fixtures cover each fused boundary,
    including after grouped execution, original SQLSTATE/message, exact charge
    cleanup, output retry and benign restart. Host tests pass, but live/GPU
    cancellation and performance qualification remain unproven; no in-flight
    asynchronous cancellation is implemented. Evidence:
    `benchmarks/evidence/hash-fused-cancel-20261010/README.md`.
- [ ] Expand raster reclassification, one/two-band expression map algebra,
  clipping and full-sample zonal statistics. Fuse tiles while preserving
  NODATA, alignment, metadata and pixel conversion semantics.
- [ ] Optimize fusion, compaction, delayed materialization, packed transfers,
  synchronization, workspace reuse and cardinality/skew-aware costs.
  - [x] Implement one reusable spatial device workspace instead of retaining
    five scratch buffers per chunk. Finish classification/compaction, validate
    and patch exact results, and service interrupts before the next chunk.
    Each subsequent dispatch revalidates the captured source generation/global
    generation/relfilenode/type/row domain under a fresh short borrow, even
    when no rows require exact recheck. Partial chunks bind exact active
    prefixes; the private final mask and publication checks remain intact.
    For 12,748,986 rows in 29,999-row chunks, accounted device scratch falls
    from 114,909,174 to 270,387 bytes and scratch allocations from 2,125 to
    five. The final mask and retained source remain relation-sized: this is
    not streaming/spill. Host checks cover prefix bounds/overflow, empty and
    partial chunks, exact accounting, source mismatch/invalidation/eviction.
    The compiled three-chunk live fixture adds final-chunk error/cancellation
    cleanup and retry after both no-recheck and patched reuse. Live/GPU
    correctness and performance qualification remain open.
    Evidence: `benchmarks/evidence/spatial-workspace-reuse-20261010/README.md`.
  - [x] Replace the relation-wide spatial exact-recheck snapshot with one
    reusable chunk-sized selection buffer. Preflight bounds its payload from
    chunk-boundary offsets without scanning geometry bytes. After compaction,
    revalidate the source generation/global generation/relfilenode/type/domain
    under a fresh short borrow and copy only uncertain rows; release that
    borrow before PostGIS calls. Invalid refills hide stale selections; final
    catalog/dependency publication and cancellation cleanup remain in force.
    Host tests cover every small selection subset, varying/NULL row lengths,
    malformed indices/offsets, partial chunks, source eviction/invalidation,
    owned lifetime and zero refill allocations. At 1,000,017 32-byte values,
    exact scratch falls from 40,000,688 to 2,621,448 bytes for 65,536-row chunks;
    two uncertain rows copy only 64 payload bytes. Resident source bytes and
    device scratch are unchanged. A compiled live fixture spans three chunks,
    sparse boundaries, changing byte layouts and NULL/empty input. Actual
    live/GPU correctness and end-to-end performance qualification remain open.
    Evidence: `benchmarks/evidence/spatial-selected-exact-20261010/README.md`.
  - [x] Remove the duplicate host group-index vector and per-tuple dense-key
    decode allocation from aggregate output. Compact output uses a contiguous
    cursor; dense output walks its validated immutable activity bitmap. Dense
    key codes use ABI-bounded stack storage. Host tests measure zero successful
    cursor/decode allocations, versus an 8,000,136-byte compact index payload
    at 1,000,017 groups on this 64-bit host and one allocation per keyed dense
    tuple. Exhaustive small bitmap tests preserve ordering, repeated peeks and
    EOF; malformed metadata and failed advances are checked. Both PG18/19
    standalone suites pass 1,296 tests; production and live-fixture strict lint
    pass. PostgreSQL Datum construction can still allocate. Live SQL/GPU and
    end-to-end performance qualification remain open; this is not a query
    speedup claim. Evidence:
    `benchmarks/evidence/group-output-cursor-20261010/README.md`.
  - [ ] Qualify the implemented hybrid raster reclassification lookup on GPU
    and live PG18/19. Up to sixteen rules keep the sequential scan; larger
    validated sorted integer rule sets check endpoints then binary-search
    neighboring sources using unchanged FLT_EQ matching. The shared device
    helper matches the former scan in 1.3M host cases, including IEEE/epsilon
    boundaries, and passes ASan/UBSan. Its host line coverage is 100%; coverage
    scope now includes the header and a 36th CTest without lowering thresholds.
    The lookup policy now selects separate linear/indexed kernels once per
    launch, not per pixel. Paired host diagnostics using the same non-inlined
    timing driver put 1/4/16-rule cases within about 2.3% of the original scan
    and improve larger sets; they are not GPU speedups. Added device tests cover
    16/17/64 rules, integer-domain extremes, epsilon boundaries, chunking,
    NODATA and invalid-rule nonpublication. The October 10 no-device blocker is
    superseded by the October 12 full native Metal run: all 36 CTests pass,
    including raster (`benchmarks/evidence/native-live-20261012/README.md`).
    Keep live PG18/19 and performance qualification open; map algebra and broader
    raster semantics are
    not enabled by this lookup change. Evidence:
    `benchmarks/evidence/raster-lookup-20261010/README.md` and
    `benchmarks/evidence/raster-launch-20261010/README.md`.
  - [x] Borrow resident text label dictionaries during dense aggregate/star
    preparation instead of deep-cloning every label. The immutable bundle
    lifetime covers preparation; final group representatives remain owned,
    including first-visible BPCHAR spellings. Four new tests cover borrowed
    versus owned code/domain parity, NULL/invalid/empty inputs, high-cardinality
    labels with sparse output, independent join dictionaries and outputs after
    source decoder destruction. Core PG18/19 each pass 1,262 tests and strict
    lint passes. The 100K-label fixture avoids 10.1 MB of temporary String
    headers/payload on the 64-bit host; the existing per-label remap and value/
    NULL downloads remain. No cross-backend sharing, new SQL admission, full
    load/query speedup or fresh GPU qualification is claimed. Evidence:
    `benchmarks/evidence/borrowed-dictionary-20261010/README.md`.
  - [x] Bound NULL-free resident-load mask scratch to one 8,192-row batch for
    primitive, text, numeric, geometry and raster columns. Preserve the existing
    per-row append loop; discard all-zero prefixes at reserve boundaries and
    backfill once when a NULL is encountered. Final mask bytes, ABI, admission
    rules and retained accounting are unchanged. Both PG18/19 standalone suites
    pass 1,258 tests, including late NULLs, exact payloads, dictionary remapping,
    malformed-input retries and overlapping backfill. At 12,748,986 rows the
    NULL-free microbenchmark mask capacity falls from 16 MiB to 8 KiB. This is
    a scratch-memory improvement, not an end-to-end speedup claim: sparse-NULL
    value-plus-mask medians are 17–26% slower in the two large runs. Full live
    loading and GPU qualification remain open. Evidence:
    `benchmarks/evidence/batched-null-staging-20261010/README.md`.
    Follow-up: normalized nullable sidecars now bypass backfill preparation,
    and late-NULL restoration clears only the old tail rather than zeroing the
    entire prefix twice. The per-row reserve microbenchmark (matching the
    additional reserve cadence in NUMERIC/raster builders, not their complete
    loading work) improves from roughly 4.5–4.6 ms to 2.4–2.5 ms per 1M synthetic
    value-plus-mask rows across six NULL patterns in two runs. Both majors pass
    1,268 core tests, including exhaustive overlapping backfill cases. The
    original batched sparse-NULL regression is NOT resolved; no end-to-end
    loading speedup is claimed. Evidence:
    `benchmarks/evidence/null-staging-perf-20261010/README.md`.
    Late-NULL backfill now computes prefix and next-batch geometric growth
    before allocating, avoiding two consecutive growth calls for one restored
    prefix. Capacity and mask bytes match the prior two-step policy in the
    host test matrix; NULL-free scratch remains bounded. The infrequent
    backfill stays out of line and the small reserve wrapper is explicitly
    inlined to preserve per-row reserve performance. New tests count two
    baseline allocations versus one, compare backfill/capacity combinations,
    and reject combined-size overflow without mutating the pending bytes.
    This removes a redundant allocation/potential copy, not final mask storage.
    Rejected experiments and host benchmark samples are retained; live loading,
    GPU qualification and the original sparse-NULL performance gate remain
    open. Evidence:
    `benchmarks/evidence/null-staging-one-growth-20261010/README.md`.
  - [x] Remove per-row temporary geometry byte copies and point-coordinate heap
    allocations during resident loading/refresh. Validate borrowed detoasted
    bytes synchronously, copy directly into retained exact storage, release
    distinct detoast allocations on success/error, and parse ordinary points
    into an inline pair. Resident layout/accounting, limits and all validation
    remain unchanged. New ownership tests cover inline/compressed/external
    TOAST, source-table release and rejected toasted 3D data on PG18/19. Core
    1207, strict Clippy both majors, docs 21 and scope/format checks pass;
    SQL112/114/118/129/134 pass 30 assertion groups across both majors.
    The unchanged 1M/2M spatial/scalar fixture eliminates 1.92M/3.84M temporary
    allocations per scan. Against the preceding source/module benchmark,
    two local candidate runs reduce load probes from 250/469 ms to
    211–214/383–390 ms and refresh-inclusive probes from 306/574 ms to
    255–263/479–490 ms. These are individual lifecycle probes, not a statistical
    general-speedup claim; first-use query probes do not improve and every
    refresh-inclusive query still loses to native. Warm results remain exact,
    normally dispatched and zero-fallback. Wider cold/concurrent qualification,
    retained-memory reduction and further refresh work remain open.
    Evidence: `benchmarks/evidence/geometry-staging-20261006/README.md`.
  - [x] Eliminate redundant high-word atomic updates from one-shot integer
    group/nonnull row counts under the existing one-million-row cap. Preserve
    exact two-word SUM, scratch layout, failure barriers and all admission
    limits. A compile-time bound and maximum-skew/boundary tests guard the
    single-word count proof. Native 32/32 (48,430 grouped checks), eight targeted
    SQL files on each major, core 1153, benchmark 575 and helpers 154 pass.
    Local 1M multi-measure medians fall from 10.740ms to 6.822/7.081ms; filtered
    medians from 10.538ms to 6.532/7.556ms. High timing variability, inconsistent
    250K multi-measure results and slower first-use probes remain explicit.
    Row fusion did not establish a benefit and was reverted; keep its evidence
    and measure-order/overflow tests. No release-wide speedup or fresh upstream
    regression qualification is claimed. Evidence:
    `benchmarks/evidence/fused-integer-20260929/README.md`.
  - [x] Publish spatial masks directly from one private device buffer instead
    of downloading every chunk and uploading a full host mask. Bind checked
    disjoint output subspans, retain exact PostGIS patching and all failure,
    cancellation, catalog and dependency publication barriers. For the full
    NYC month, remove 12,748,986 bytes of host staging, 25,497,972 bytes of mask
    transfers, 425 net device allocations and the extra final device mask.
    Persistent bytes, C ABI, logical wire and admission thresholds are unchanged.
    Core 1150, live spatial tests 7/7 on each major, ten targeted production
    SQL files on each major, native 32/32, both Clippy builds and docs 21 pass.
    A matched baseline plus two candidate runs all produce exact results,
    verified GPU dispatch and zero fallback. Full-month preparation falls
    from 4.05s to 3.18/3.35s in individual lifecycle probes; first use falls
    from 6.84s to 5.97/6.16s but still loses to native PostGIS. The 1M probe
    does not consistently improve. These are local observations, not a
    statistically qualified general speedup or full release qualification.
    Streaming, exact-snapshot costs and broader fusion remain open.
    Evidence: `benchmarks/evidence/spatial-mask-publication-20260929/README.md`.
  - [x] Bound host spatial recheck scratch to one largest chunk and transfer
    only the populated uncertainty-index/exact-result prefixes. Empty prefixes
    perform no FFI copy; complete-buffer copy APIs retain their exact-length
    contract. Checked count, range, strict ordering, failure barriers and exact
    PostGIS rechecks remain mandatory. Preflight and actual workspace accounting
    charge the full reusable allocation, not the current uncertain-row count.
    For the 12,748,986-row NYC/JFK fixture (29,999-row chunks), the two host
    scratch lanes fall from 114,740,874 to 269,991 bytes, saving 109.17 MiB.
    This excludes the unchanged exact snapshot, final host mask and device
    scratch; it is not a claim of bounded-memory streaming or spill support.
    Pure tests pass 1106, PG18 live spatial tests pass 6/6 (including prefix
    guard/suffix preservation, error/cancellation and invalidation), both
    Clippy builds pass, and doc/coverage/safety audits pass. Evidence is under
    `.codex/scratch/spatial-prefix-*-20260921*`. Installed PG18 and PG19 each
    pass 77/77 SQL files (388 declared assertions), including the million-row
    multi-chunk polygon/hole/component boundary and rollback fixtures, in
    `.codex/scratch/spatial-prefix-pg{18,19}-20260921`.
    PG18 module SHA-256 is
    `4f9db465533efa369ecee648f4a0e7499bc61c7b4b4a2dbbd1e9d718d3d3a574`;
    PG19 is `62f3bafe0ebe2dcfd2c3f50bc73094aa67f358dedcc92d77c5b6d73e5fd93834`.
    Two real-data runs after the test/build jobs finished are retained in
    `.codex/scratch/spatial-prefix-nyc-warm-20260921` and its `-b` repeat.
    Both scales have exact PostGIS differential results, matching installed
    module provenance, ten warm GPU dispatches, zero fallback and no crashes.
    At 1M rows, mask construction is 115/116 ms and first-use lifecycle is
    305/310 ms, versus the edge-index candidate's 122/123 and 317/317 ms.
    At all 12,748,986 rows, construction is 2.78/2.99 s and lifecycle is
    5.21/5.46 s, versus 3.11/3.16 and 5.59/5.70 s. Separate preload is
    215/219 ms and 2.406/2.404 s. These individual lifecycle observations
    suggest a modest additional improvement, not a statistically qualified
    first-use speedup. First use still loses to native PostGIS (35–37 ms
    and 232–239 ms in these probes). Warm cached-mask medians are 4.64/4.64 ms
    and 8.67/9.08 ms; they do not measure fresh polygon classification.
    The corpus remains non-headline local Metal evidence, not cold-cache,
    concurrency, CUDA or full release qualification.
  - [x] Replace the resident spatial helper's whole-chunk serial compaction
    with parallel tile validation/counting and ordered publication. At most
    65 counts reuse the existing charged control buffer; there is no extra
    allocation, ABI expansion or intermediate host cardinality readback.
    Internal tile boundaries align to mask words even for byte-offset output
    subspans, preventing Metal subword-store races. Invalid tri-state bytes
    and sticky evaluation failures suppress every public output write; the
    small (at most 1024 rows) serial path remains. Native spatial tests pass
    2067 checks, including all byte offsets, partial tiles, sparse/all/no
    uncertainty, ordered indices, guard bytes, nonpublication and recovery.
    Final native CTest passes 32/32 in
    `.codex/scratch/spatial-compact-ctest-final-20260921.log` and its `final-detail`
    companion. The optional `test_spatial_resident --bench-compaction`
    diagnostic retains ten post-warmup timings per size/pattern: the initial
    parallel candidate reduced 65,536-row helper means from 15.97–17.07 ms
    to 0.62–0.87 ms, with independent exact output checks. Baseline and
    candidate logs are `.codex/scratch/spatial-compact-{baseline,parallel}-20260921.log`.
    Initial aligned-buffer end-to-end runs are retained in
    `.codex/scratch/spatial-compact-nyc-warm-20260921` and its `-b` repeat;
    1M mask construction is 535/514 ms and lifecycle 732/709 ms, versus the
    preceding 722–731 ms and 907–914 ms observations. Full-month construction
    remains 9.77/9.56 s and lifecycle 12.42/12.35 s. These are individual
    lifecycle observations on a variable host, with CPU audit work overlapping
    portions of the runs, not controlled first-use certification. They
    precede byte-offset hardening; final-candidate evidence is recorded below.
    Final PG18/19 SQL suites pass 77/77 files (388 declared assertions) in
    `.codex/scratch/spatial-compact-final-pg{18,19}-20260921`. All 123 native
    exports pass the full GPU-execution/ABI audit with a freshly relinked,
    hash-recorded shared object in
    `.codex/scratch/spatial-compact-cpu-audit-final-20260921.json`. Its 173
    verifier tests, 42 doc/coverage tests and safety-contract audits pass.
    The initial audit rejected the shared success return after parallel/serial
    branches; each branch now returns explicitly after its own completed
    device work. The rule and failure threshold are unchanged. Failed and
    focused audit attempts remain as `spatial-compact-*-audit*` logs.
    Final-candidate helper means are 0.82–1.13 ms at 65,536 rows (14–20x
    faster than the serial baseline) and 1.00–1.26 ms at 30,000 rows, retained
    in `.codex/scratch/spatial-compact-native-perf-final-20260921.log`.
    After other test/build jobs finished, the exact installed PG18 candidate
    passed the two real-data cells again in
    `.codex/scratch/spatial-compact-nyc-final-warm-20260921`: 1M construction
    is 519 ms and lifecycle 720 ms; full-month construction is 8.59 s and
    lifecycle 11.15 s. Separate preload is 229 ms / 2.48 s. Exact differential
    outputs, ten actual warm dispatches and zero stock fallback hold at each
    scale. Warm medians are 5.21/97.45 ms and 8.71/249.35 ms, but native 1M
    variation is 30% and prior full-scale native observations vary widely;
    these ratios are not a reliable headline. The corpus remains explicitly
    non-headline, first-use probes are single observations, and these local
    dirty-tree Metal results do not qualify CUDA, cold cache or concurrency.
    First use still loses to PostGIS; candidate indexes, fused preparation,
    transfer/launch reduction and the broader optimization milestone stay open.
  - [x] Prepare conservative edge-block boxes for constant Polygon/MultiPolygon
    classification. Blocks cover 32 edge starts plus their following vertex;
    ring/component transitions only widen bounds. CPU input preparation builds
    the boxes without classifying query points. The GPU validates every
    coordinate against its ordinary bbox and all relevant index boxes before
    evaluation; malformed bounds suppress all output. Whole blocks strictly
    above/below the ray or left of the point are skipped, while equality and
    uncertain boundary handling retain exact PostGIS rechecks. Constant
    coordinate validation now runs across GPU work items instead of one
    serial lane; ordinary column validation remains per row.
    An additive one-row view flag appends boxes to the bbox allocation without
    changing ABI layouts or logical geometry counts. Device accounting,
    preflight and native referenced-byte budgets charge the exact extra span;
    unsupported flags and incomplete spans fail closed. Normal relation
    columns retain their unindexed layout. This is a constant-edge candidate
    index, not row-producing spatial joins or a replacement for GiST.
    Rust tests pass 1104 and both Clippy versions pass. Native spatial tests
    pass 2094 checks and all 32 CTests pass in
    `.codex/scratch/spatial-edge-index-ctest-20260921.log` and its `ctest-detail`
    companion. Tests cover ring/component/block boundaries, signed zero,
    one-ULP probes, malformed/nonfinite and conservative non-tight bounds,
    omitted crossing-edge endpoints, exact byte charging and nonpublication.
    `test_spatial_resident --bench-edge-index` records ten paired fresh
    classifications of 8192 points against a 4097-coordinate polygon, with an
    independent integer-radius oracle and no predicate-mask reuse. With
    parallel validation, unindexed/indexed means are 8.88/1.47 ms (6.05x) in
    `.codex/scratch/spatial-edge-index-native-perf-20260921-b.log`. The initial
    prototype regressed (18.05/21.69 ms with serial constant validation); its
    log remains as `spatial-edge-index-native-perf-20260921.log`. It was not
    installed before parallel validation fixed the regression. These are
    native helper diagnostics, not whole-query or release speedups.
    Installed PG18/19 suites each pass 77/77 SQL files (388 assertions) in
    `.codex/scratch/spatial-edge-index-pg{18,19}-20260921`. The full native
    GPU-execution/ABI audit passes all 123 exports against a fresh, hash-recorded
    shared object in `.codex/scratch/spatial-edge-index-cpu-audit-20260921.json`;
    safety-contract and 42 doc/coverage tests also pass. Unindexed legacy
    layout, descriptor sizes and semantic assertion floors are unchanged.
    Two real NYC airport-scan runs, after other build/test jobs finished,
    retain exact PostGIS comparisons, ten paired warm observations per scale,
    actual GPU dispatch and zero stock fallback in
    `.codex/scratch/spatial-edge-index-nyc-warm-20260921` and its `-b` repeat.
    At 1M rows, mask construction is 123/122 ms and reload-plus-construction
    lifecycle is 317/317 ms, versus the preceding candidate's 519/720 ms
    observations. At all 12,748,986 rows, construction is 3.11/3.16 s and
    lifecycle is 5.59/5.70 s, versus 8.59/11.15 s previously. Separate preload
    costs are 215/216 ms and 2.45/2.52 s. These are recorded lifecycle probes,
    not statistically qualified cold-query medians. First use still loses to
    PostGIS (37 ms at 1M and 230–235 ms full scale in those probes).
    Warm cached-mask medians remain 4.50/4.55 ms versus native 36.69/36.57 ms
    and 9.03/9.21 ms versus 233.87/234.35 ms; those are not fresh classification
    measurements or complete-corpus claims. The corpus remains non-headline,
    local dirty-tree Metal evidence; CUDA, cold cache, concurrency and broader
    preparation/transfer/fusion work remain open.
  - [x] Reduce point/Polygon construction work with exact vertical edge pruning
    and query-local PostGIS prepared-geometry reuse during uncertain-row
    rechecks. Edges strictly outside the horizontal ray's y range skip x loads
    and segment/orientation checks; equal coordinates keep the original exact
    boundary path. A lazily initialized FmgrInfo survives chunk boundaries in
    an owned child memory context, never in the reusable result artifact.
    Live tests populate that cache before injecting ordinary errors and
    cancellation, check its release, and verify subsequent successful rechecks
    without stock fallback. PG18's five spatial tests pass and PG19's targeted
    error test passes (`spatial-recheck-pgrx18-20260921-c.log` and matching
    `pgrx19` log under `.codex/scratch`). One old tiny-fixture decline assertion
    now correctly expects the row floor rather than descriptor rejection.
    Native spatial tests pass 137 checks, including segmented holes, degenerate
    edges, signed zero and one-ULP offsets around horizontal hole boundaries;
    native CTest passes 32/32. Rust tests pass 1098 and both Clippy builds pass.
    Final full SQL suites pass 75/75 files (383 assertions), retained in
    `.codex/scratch/spatial-ray-pg18-20260921-b` and
    `.codex/scratch/spatial-ray-pg19-20260921`.
    Across Intersects/Contains/Within at 1M/2M, diagnostic lifecycle probes show
    mask construction falling from 525–548 / 1024–1063 ms to 374–391 / 737–762
    ms (27–31% less), and total first use from 714–739 / 1396–1436 ms to
    563–579 / 1110–1130 ms (20–23% less). These are individual lifecycle
    observations, not statistically qualified first-use medians. First use
    remains slower than native PostgreSQL. Ten paired warm observations per
    cell retain exact output/oracle checks, real dispatch, zero fallback, and
    13.76–21.76x cached-mask speedups. Evidence is
    `.codex/scratch/spatial_holes_*-ray-warm-20260921`; cache-only intermediate
    results are retained as `*-recheck-warm-20260921` and show a small first-use
    improvement alone. These are synthetic no-index, dirty-tree local Metal
    diagnostics, not real-data, concurrency, CUDA or release qualification.
    The initial `spatial-ray-pg18-20260921` suite failed SQL110 with Metal's
    "Impacting Interactivity" error while native GPU stress tests were also
    active; the complete isolated rerun passes. The failed artifact remains.
    GPU contention/watchdog robustness and the broader concurrency milestone
    remain unresolved; isolated success does not qualify concurrent operation.
  - [x] HASH COUNT records occupied slots in existing device scratch during
    aggregation; final compaction visits actual groups instead of serially
    scanning the entire reserved hash table. Capacity failures still prevent
    publication. Native hash tests pass 390 checks, including one million
    rows with both tiny and million-group capacities and workspace-size bounds.
    All 32 native CTests pass on the updated shared library. The focused pgrx
    H3 test also passes, including selected-path source-resolution errors that
    preserve h3-pg SQLSTATE `38000`; the successful run is retained in
    `.codex/scratch/h3-compact-pgrx-20260921-b.log`.
  - [x] Reuse resident text codes during grouping-dictionary preparation and
    use bounded direct lookup for boolean and compact int32/date domains.
    Lazy remaps preserve first-seen representatives, NULL codes, unused labels,
    bpchar equivalence and filtered dimension-row behavior. They are temporary
    per-build metadata, not process-shared caches; aggregate arithmetic is
    unchanged. Reference-equivalence/boundary tests pass in the 1093-test Rust
    suite, and full PG18/19 SQL suites pass 71/71 files, retained in
    `.codex/scratch/dictionary-reuse-pg18-20260921` and the matching `pg19`
    directory. Two local sales-rollup runs retain exact 225-row comparisons,
    independent oracles, dispatch and warm gates in
    `.codex/scratch/dictionary-reuse-warm-20260921` and its `-b` repeat.
    At 1M rows, measured dictionary construction is 17.955/17.267 ms versus
    the preceding 96.226 ms probe; first-use lifecycle latency is
    195.44/201.31 ms versus 275.86 ms. At 250K, construction is 4.533/4.630 ms
    versus 24.228 ms, and lifecycle latency is 50.82/51.68 versus 73.23 ms.
    These are diagnostic lifecycle observations, not a controlled statistical
    before/after certification. Ten paired warm observations in each run
    still pass: 2.39–2.48x at 250K and 5.56–5.85x at 1M. This build-only change
    does not explain warm timing variation. Preload remains about 69/198–201 ms
    and first use still loses to native PostgreSQL: raw residency loading,
    streaming and shared ownership remain unfinished. CUDA is unqualified.
  - [x] Intern text during the raw MVCC scan instead of retaining/cloning a
    String per row. Retain one owned label per distinct value, stage row codes
    and NULL flags, and remap to the existing sorted dictionary at finalization.
    Inline borrows do not escape insertion; external/compressed values keep
    pgrx's owning decoder/cleanup. The 1095-test Rust suite passes. SQL109 adds
    inline Unicode, compressed/external TOAST, varchar/bpchar, DML and rollback
    coverage. Final PG18/19 suites pass 72/72 files (376 assertions), retained
    in `.codex/scratch/text-intern-pg{18,19}-20260921-b`.
    The initial PG18 run failed SQL109 with exhausted local buffers; the same
    fixture failed with pg_accel disabled in
    `.codex/scratch/text-intern-native-toast-pg18-20260921.log`, and passed with
    read-ahead disabled. SQL109 now sets effective_io_concurrency=0 for BOTH
    native/accelerated controls and resets it afterward. Data and buffer sizes,
    production settings and the other 71 SQL files are unchanged. Initial
    PG19 passed with default read-ahead; all failed/diagnostic runs are retained.
    Two local sales-rollup runs (`.codex/scratch/text-intern-warm-20260921` and
    its `-b` repeat) preserve exact outputs, independent oracles and dispatch.
    At 1M rows, raw reload is 107.179/106.568 ms versus the preceding
    171.232/176.519 ms; first-use lifecycle latency is 132.68/129.68 ms versus
    195.44/201.31 ms. At 250K, reload is 28.429/28.704 ms and lifecycle latency
    is 37.71/38.60 ms. Preload is separately 54.92–55.64/134.46–138.73 ms.
    Both ten-pair warm runs pass the unchanged floor (2.22–2.37x at 250K,
    4.96–5.94x at 1M). These are local dirty-tree diagnostics, not controlled
    before/after certification; first use still loses and CUDA is unqualified.
- [ ] Extend the existing harness to pinned full ClickBench (43 queries),
  TPC-H-derived (22 queries), real-coordinate NYC taxi and Landsat workloads.
  Ship OLAP, vector/H3 and raster SQL showcases; report native queries,
  first-use/cold/warm costs, concurrency, spill and memory. Target acceleration
  of half of each OLAP corpus and 2x complete-corpus geometric mean; pursue
  10x OLAP/geo showcase wins without hiding missed targets.
  - [ ] Resolve upstream asset handling before adding the full ClickBench
    corpus. Inspection on 2026-10-10 found 43 statements in upstream
    `postgresql/queries.sql`; the current three `clickbench_*` workloads are
    compact synthetic shapes, not that corpus. The upstream repository's
    `LICENSE` is CC BY-NC-SA 4.0, while this workspace declares the PostgreSQL
    license. Do not silently vendor/relicense upstream SQL, schema or data.
    Prefer separately supplied, checksum-pinned benchmark inputs; confirm
    asset distribution/licensing with the owner before vendoring. Sources:
    https://github.com/ClickHouse/ClickBench/blob/main/LICENSE and
    https://github.com/ClickHouse/ClickBench/blob/main/postgresql/queries.sql.
    Also preserve the original queries' LIMIT/tie semantics: Q18 has LIMIT
    without ORDER BY, and several top-N queries leave ties unordered. A full
    corpus parity gate must account for legally different result subsets;
    do not silently add tie-breakers or call the three-query suite complete.
  - [ ] Qualify the implemented full 22-query TPC-H-derived corpus on PG18/19
    and run its complete native/pg_accel performance matrix. The previous
    three-query subset is expanded to all eight tables with exact NUMERIC,
    referential constraints, fixed parameters, correlated subqueries,
    semi/anti/outer joins, DISTINCT and HAVING. All 22 names are mandatory in
    the system gate, with complete per-query warm pin declarations. Fixture
    and SQL checksums identify revision 2; historical subset timings are not
    comparable. All 22 produce nonempty/non-NULL answers at 100K in a secondary
    DuckDB diagnostic after correcting two synthetic-data correlations; this
    is not PostgreSQL parity or exact NUMERIC evidence. The rollback-only
    PG integration test is implemented, but local sockets and even isolated
    cluster initialization are denied by this sandbox. Keep this item open
    pending live PG18/19 results, full harness dispatch/decline evidence and
    measured corpus targets. Evidence:
    `benchmarks/evidence/tpch-full-20261010/README.md`.
  - [x] Add and execute an opt-in, checksum-pinned real-coordinate January 2015
    NYC yellow-taxi characterization. `benchmarks/corpora/nyc-yellow-2015-01/`
    records the Inria Aviz original-format CSV mirror and complete JFK boundary;
    `scripts/nyc_taxi_corpus.py` verifies/projects/imports all 12,748,986 rows
    without filtering, replication, coordinate substitution or monetary rounding.
    Source order, 243,478 zero-coordinate records and one invalid-coordinate
    record are retained. The archive differs from the current Open Data export
    (12,741,017 January records), so provenance is explicit; failed/slow API
    attempts remain in `.codex/scratch/nyc-*20260921*`. The pinned 2026 JFK
    boundary retains all four components/2,237 coordinates, not a historical
    2015-boundary claim. Full import occupies 2741 MB including indexes locally.
    The default synthetic suite remains independent of external data.
    `.codex/scratch/nyc-corpus-warm-20260921` passes all eight cells (four queries,
    1M and complete-file scales), ten warm pairs/five warmups, exact differential
    checks and an independent H3 parent-bit oracle, with no crashes or stock
    executor fallback. Airport inputs are resident-pinned even though the
    complete multipolygon declined in this initial baseline; the indexed native baseline keeps
    GiST available. Every cell captures its source manifest, plans, dispatch or
    decline evidence, and first-use/load costs where applicable. Resumed and
    single-cell runs cannot promote this exploratory corpus into a headline.
    At 1M, H3 warm medians are 17.638 ms versus native 82.384 ms (4.67x), with
    real kernel dispatch; the single lifecycle observation is 66.935 versus
    82.751 ms and separate preload is 70.223 ms, not a cold-query qualification.
    In this initial baseline, 12.75M H3 remains native
    (`generic_groups_exceed_device_maximum`), despite
    only 4,467 output groups: 205.153/205.140 ms median. JFK indexed/scan queries
    remain native (`shape_unsupported_predicate`): full-scale medians
    118.640/117.947 ms and 225.176/226.747 ms. Exact NUMERIC fare rollups remain
    native (`shape_numeric_accumulator_unavailable`): 281.050/279.406 ms.
    Unproductive full-scale spatial preload costs about 2.33 seconds and is not
    hidden. The subsequent large-H3 and constant-MultiPolygon milestones above
    resolve those admission gaps; exact NUMERIC aggregation remains open.
    This is local dirty-tree Metal characterization, not a complete
    NYC-year/OLAP corpus, cold/concurrent/CUDA or release qualification.
    Verification: 568 benchmark unit tests, ten hermetic importer tests plus
    one live rollback-only import test on each PG18/PG19, Clippy, doc parity and
    coverage checks pass. The broader corpus milestone remains open.
- [ ] Run upstream PostgreSQL regression/isolation and PostGIS/H3 differential
  tests at milestones, including concurrency, DML/DDL, rollback, permissions,
  RLS exclusion, prepared plans, errors, cancellation and owner failure.
  - [x] Refresh upstream compatibility after scalar FILTER/DATE/WHERE,
    PostgreSQL qualifier ordering, spatial WHERE, geometry staging and
    NULL-safe scalar expansion. Exact normal PG18.4/PG19beta1 modules pass all
    pristine/loaded regression and isolation schedules (1448 executions),
    with both complete artifact seals verified. No expected-output changes
    or new exclusions. These local debug/assertion builds omit ICU; preloaded
    upstream databases do not install pg_accel SQL objects, so this proves
    hook/default-path compatibility, not GPU execution of the upstream corpus.
    Separately, final installed-extension SQL suites pass 98/98 files and
    451/451 semantic assertions on each major, with no warnings/skips. Both
    select all 48 NULL-safe WHERE and 24 FILTER/CASE/count shapes. The initial
    PG18 full run (97/98) is retained: SQL111 incorrectly required native
    execution for newly supported scalar FILTER predicates. Five such cases
    now require native/CASE-oracle parity and GPU dispatch; genuine decline
    checks remain. Its reviewed guard count grows from three to five, without
    lowering the 90% threshold. Isolated CREATE/DROP/rollback/prepared-plan
    catalog checks pass both majors and remove only their owned test databases.
    Coverage tooling 104, docs 21 and scope audit pass. Normal module hashes
    remain unchanged; no production fix, timeout increase or performance claim.
    Evidence: `benchmarks/evidence/nullsafe-compatibility-20261006/README.md`.
  - [x] Refresh local upstream compatibility after scalar programs, simple CASE,
    instruction fusion and constant-list membership. Exact normal PG18/19
    modules pass pristine/loaded regression and isolation: 700 plus 748
    scheduled executions, with both artifact seals verified. No expected-output
    changes or new exclusions. Complete raw bundles, module hashes and source
    snapshot are in `benchmarks/evidence/scalar-membership-20261006/README.md`.
    The same modules separately pass selected OLAP/geo SQL128/129, including
    prepared DML/rollback and error semantics. These local dirty-tree results
    do not close future milestones, owner-failure work or hosted release gates.
  - [x] Refresh upstream compatibility after the NUMERIC/predicate/arithmetic
    expansion and atomic-count optimization. This exposed a real PG18 failure:
    preloading without CREATE EXTENSION could select aggregates whose resident
    loader then failed for missing extension-owned SQL objects (`select_parallel`
    and `temp`). Upper-path admission now requires PostgreSQL's database-local,
    invalidation-aware extension catalog and declines during extension scripts.
    No executor fallback or PostgreSQL expected-output changes were introduced.
    Fresh PG18.4 and PG19beta1 pristine/loaded regression and isolation runs all
    pass (1448 scheduled executions), with all three original/fixed artifact
    seals verified. A new isolated-database lifecycle command tests CREATE/DROP,
    rollback, generic prepared plans, spoofed helper names and resumed normal
    GPU dispatch on both majors; it is wired into CI, not yet hosted-qualified.
    Core 1154, both Clippy builds and five tool-contract tests pass. The full
    installed-extension SQL matrix is a separate gate: both full runs finish
    89/90, with SQL117 NUMERIC dispatch warnings of 6.708 seconds on PG18 and
    5.340 seconds on PG19. The unchanged fixture and final catalog lifecycle
    checks pass on both majors in serial follow-ups without warnings; this
    does not erase the full-run failures or establish their cause. Broader
    concurrency, selected PostGIS/H3 and release work stay open.
    Evidence: `benchmarks/evidence/catalog-compatibility-20260929/README.md`.
  - [x] Verify the accumulated OLAP/H3 expansion and text-loader changes against
    PostgreSQL's own suites: PG18.4 passes 231 regression and 119 isolation
    tests, and PG19beta1 passes 245 regression and 129 isolation tests, each
    both pristine and with pg_accel preloaded/enabled. All eight runs pass
    (1448 scheduled test executions). Artifacts and source/module hashes are
    retained in `.codex/scratch/upstream-expansion-pg{18,19}-20260921`; both
    complete SHA256SUMS seals and the captured source manifest validate.
    The installed module hashes are
    `f7ee559491cc777ce90b486adaeac317da44ed95a5636f52284c321d1cc4c626`
    (PG18) and
    `ec8e0951359b093e45a624a6a86f0cf4a88c82780df3c2dc78f787b96fbb7739`
    (PG19). These dirty-tree local debug/assertion builds omit ICU. The loaded
    suites exercise hooks/default-path compatibility without installing
    extension SQL objects in the upstream databases; they do not demonstrate
    GPU execution of the upstream corpus. Selected-path SQL/PostGIS/H3 tests
    remain separate evidence, and future engine changes need fresh milestone
    runs. This does not qualify CUDA, other hosts or unfinished owner recovery.
  - [x] Refresh the same upstream gate after multiple measures, independent
    FILTER, polygon-hole/recheck and large-H3 changes. All eight PG18.4 and
    PG19beta1 pristine/loaded regression/isolation runs pass again (1448 test
    executions), with complete artifact seals verified in
    `.codex/scratch/upstream-large-h3-pg{18,19}-20260921`. Installed module hashes
    are `b8c51d750f8895d691dede878f8377b07235d6c1de5af4856c008b31fd582013`
    (PG18) and
    `beaf6f70670b8cba9dc2d612ff9f2f85cedc5115f3e812e7157c85e407eb045f`
    (PG19). These retain the same local debug/assertion, no-ICU and hook/default
    path limitations above; GPU semantics are separately covered by the
    76-file selected-path suites, not inferred from the upstream gate.
  - [x] Refresh upstream compatibility after constant-MultiPolygon support.
    All eight PG18.4/PG19beta1 pristine/loaded regression/isolation runs pass
    (1448 scheduled executions); complete artifact seals validate in
    `.codex/scratch/upstream-multipolygon-pg{18,19}-20260921`. Installed module
    hashes are
    `d020ff9392e3911acceffafff06daade5db836d9f16cc31811ab80e1450011a5`
    (PG18) and
    `8976af088106052df344fbec7674ea2c51b85e317571ef577ab7a08074f178ab`
    (PG19). The same local debug/assertion, no-ICU and hook/default-path
    limitations apply; selected GPU semantics are covered separately by the
    77-file suites, not inferred from upstream compatibility.
  - [x] Refresh the same upstream gate after parallel spatial compaction and
    byte-offset output hardening. All eight pristine/loaded PG18.4/PG19beta1
    regression/isolation runs pass (1448 scheduled executions), and both
    complete SHA256SUMS seals verify in
    `.codex/scratch/upstream-spatial-compact-final-pg{18,19}-20260921`. Installed
    module hashes are
    `ac2f888cd827209221643b22a40fd73d18a34d9143802498ecf543334d81890c`
    (PG18) and
    `af4edcb8a6401233656ccc856d4c539e982a2e77db4cb37143b18b28bb208f17`
    (PG19). The same local debug/assertion, no-ICU, hook/default-path and
    non-CUDA limitations apply; this does not qualify GPU execution of the
    upstream corpus or concurrent owner recovery.
  - [x] Refresh upstream compatibility after constant edge indexing and
    parallel constant-coordinate validation. All eight pristine/loaded
    PG18.4/PG19beta1 regression/isolation runs pass (1448 scheduled executions),
    with complete seals verified in
    `.codex/scratch/upstream-spatial-edge-index-pg{18,19}-20260921`. Installed
    module hashes are
    `287a6b5ca4f0b896ca3ad44a2db6868d024b8a24dae577a62d55ad70a2163b6b`
    (PG18) and
    `ed606b521c75ca40b3bd662199e1b925bf49552900c81ffb7d7e675d488c091b`
    (PG19). These preserve the same local debug/assertion, no-ICU,
    hook/default-path limitations above; selected GPU behavior remains a
    separate gate, and CUDA/concurrent owner recovery remain unqualified.
- [ ] Qualify CUDA once before release on disposable NVIDIA L4 capacity with
  a $50 total ceiling including storage/overhead, a precomputed cutoff and
  automatic teardown. Prepare locally first; no recurring paid CI. Leave CUDA
  unqualified if that budget cannot establish correctness. This supersedes
  the historical hardware deferral below, not its correctness requirements.

## Current Implementation Baseline

- [x] The latest frozen implementation candidate is
  `07c50129aa22984209b7cd070a6b5227e43abb3d` (tree
  `d7109076727f7804660b193a8614a462ff19d764`). Its PG18 three-layer coverage,
  enriched Metal stress, 17-cell native parity, PG18/PG19 upstream PostgreSQL
  suites, and same-host selected-path control are sealed below. The guarded
  nineteen-cell Metal run also proves all warm floors, but remains a failed
  release gate because this unprivileged host could not complete the required
  H3 cold-cache purges. Hosted CI, privileged cold-cache evidence, an
  independent fresh-machine install, and publication evidence remain separate
  gates. Later documentation-only commits do not change which implementation
  tree these artifacts validate.
- [x] PostgreSQL 18 and 19 strict workspace Clippy/check gates pass.
- [x] The current Rust extension and benchmark harness unit suites pass, along
  with the live plan/integration suite.
- [x] Native Metal CTest passes 32/32 executables. The focused H3 suite passes
  1,135/1,135 plus 23/23 no-device cases.
- [x] Installed-extension PostgreSQL 18 and 19 SQL each pass 60/60 files and
  the fixed 323/323 semantic assertion inventory on clean candidate `e465a24`.
  The release module SHA-256 values are
  `0eb0eaec2830de31b13ec4bc964dd6aab93cce9daad7e374cb4ceef4d0b189e5`
  for PG18 and
  `58efe8b92266eda727df8d5baf1d300d6978c63778616bc36bb3ec39d3db9d08`
  for PG19. Strict per-file evidence is retained in
  `.codex/scratch/sql-exact-e465a24-pg18-20260812-a` and
  `.codex/scratch/sql-exact-e465a24-pg19-20260812-a`. The semantic matrix
  covers 22 families, and all released selected families have declared NULL,
  prepared-plan, DML, DDL, dispatch, and shape evidence.
- [x] Production residency-ledger integration, packaging tests, dependency
  policy/RustSec audit, documentation parity, coverage-scope audit, Metal stress
  artifact tests, and the object-bound CPU-cheat audit pass.
- [x] The sealed predecessor warm matrix at
  `.codex/scratch/final-warm-benchmark-3a0bcd73-PREPARED-20260811T072357Z`
  passes correctness and path validation for 37/37 cells: 20/20 GPU winners at
  or above 1.15x, 17/17 exact native declines, and zero stock fallback. Its
  1,362-entry `SHA256SUMS` seal hashes to
  `5c9f50243bbe0141fa23e8bd0dd5a84a577f95a1646bcc927553fb77c1ced70c`.
- [x] The focused predecessor 30-pair artifact at
  `.codex/scratch/supplemental-warm-diagnostics-3a0bcd73-PREPARED-20260811T074540Z`
  passes 11/11 selected winner diagnostics in both full and post-first views.
  Full median speedups range from 3.758x to 21.171x; every paired test has
  `p < 0.003` and Cohen's `d > 0.865`. Its 781-entry seal hashes to
  `4a838b7c3dae5f393ef648c2b47d0f47ae88e8b60779dc1b039ea26565b92d58`.
- [x] Native-decline parity passes on exact candidate `07c5012`. The sealed
  artifact at
  `.codex/scratch/native-parity-p0-07c5012-pg18-20260820-c` retains 30 exactly
  balanced pairs and four raw executions per arm for each of all 17 registered
  cells. All 17 pass the median and p95 bounds plus exact one-sided paired
  non-inferiority; all 17 preserve comparable PostgreSQL-native plans and zero
  device dispatch. The largest median regression was 1.268%, the largest p95
  regression was 2.214%, and the largest exact non-inferiority p-value was
  0.000022. Expected and installed module SHA-256 values both equal
  `21cc218f68789bdf54c9cc6bce82417f13228798837b810bf356bb9158a335a3`.
  The terminal `SHA256SUMS` file hashes to
  `1b69e3a48a279979ad718d379d62ed9efb4501c8e447520dd1d6facae60fc1c4`;
  its complete manifest verifies without error.

## Non-Negotiable Invariants

- A selected plan must execute a real GPU-resident pipeline, return an exact
  PostgreSQL result, consume device-produced output, record coherent same-backend
  counters, use zero stock fallback, and clear its registered 1.15x warm floor.
- A declined plan must remain PostgreSQL-native, expose its exact structural or
  cost reason, dispatch zero GPU work, and pass the paired native-parity contract.
- GPU failure after production selection is an error. No benchmark label, test
  GUC, cached output, registration entry, or planner choice alone proves GPU work.
- Evidence must bind an exact clean source/tree and binary, retain raw paired
  samples, record first-use work, and use immutable-style manifests and seals.
  Never weaken a threshold or omit a slow sample to make a gate pass.

## P0: Native-Decline Overhead

The focused native artifact reproduced real overhead. Representative full-series
enabled-minus-disabled results include:

| Cell | Median delta | p95 delta |
|---|---:|---:|
| `grouped_agg_int4` 10K | +5.49% | +28.79% |
| `ssbm_resident_int4_star` 10M | +9.38% | -12.72% |
| `hash_join` 10K | +10.98% | -8.49% |
| `hashjoin_10k_1m` 10K | +22.42% | -2.45% |
| `hashjoin_10k_1m` 100K | +14.65% | +11.04% |
| `reduce_f64_minmax` 100K | +10.44% | -4.92% |
| `ssbm_resident_int8_star` 10K | +11.88% | +22.78% |

- [x] Add substage timing for query fingerprint construction, decline-cache
  lookup, dependency revalidation, native-cost reconstruction, and rejection
  recording. Standard benchmarks must keep profiling off.
- [x] Build the statement source/search-path/security fingerprint once, lazily
  for a top-level aggregate candidate, instead of recopying it in each
  recognizer. Bounds-check prepared/EXECUTE source mismatches without wrapping
  `standard_planner` or intercepting PostgreSQL error metadata.
- [x] Stop cloning complete decline-cache entries on lookup and avoid repeated
  residency-store scans while preserving exact collision checks, catalog epoch,
  dependency stamps, policy GUCs, and error-safe nested planner cleanup.
- [x] Remove avoidable tracing/map/counter work from unprofiled rejection paths.
- [x] Re-run all 17 registered native-decline cells with 30 balanced pairs. Pass
  median allowance `max(0.25 ms, 2%)`, p95 allowance 5%, and exact paired
  non-inferiority at `alpha=0.05` in 17/17 cells. Target planner upper-group
  means are at most 25 us for simple declines and 75 us for join/reduce declines.
  The profiling-off release measurement passes all formal gates in
  `.codex/scratch/native-parity-p0-bfa756f-pg18-20260819-b`: 17/17 descriptive
  bounds, 17/17 exact non-inferiority tests, and 17/17 final parity verdicts.
  `native_parity.json` hashes to
  `a395450a097949e23aeb477a0a9d871c0b2d09802a39815c04870494f6d9e9ac`.
  The preceding `-a` attempt contains only cells 1-6 and stopped before cell 7
  when unrelated `corespotlightd` exceeded the 50% foreign-CPU guard. It remains
  partial diagnostic evidence and must not be resumed, combined, sealed, or
  relabeled as the complete `-b` result.

## P0: Benchmark Lifecycle Accounting

The broad ten-pair run placed a forced artifact rebuild in measured sample one.
In the focused repeat that first accelerated sample was 3.44x to 105.59x slower
than the post-first median. With 30 pairs it no longer changes the winner verdict,
but it makes a ten-sample warm p95 describe lifecycle work rather than steady state.

- [x] Keep refresh/rebuild/dispatch as a separately sealed lifecycle probe, then
  measure ten artifact-hit warm pairs independently. Reports and validators must
  require both; the rebuild sample must never be discarded or relabeled.
- [x] Record `Built`, `Rebuilt`, and `Hit` outcomes, construction bytes/time,
  generation/dependency identity, dispatch counters, and output consumption for
  the lifecycle and steady-state series.
- [x] Preserve a combined end-to-end view for cost-model calibration while using
  the steady-state series for the warm latency ratchet.

## Ordered Architecture And Performance Roadmap

These nine items capture the architectural follow-up from the pgrust executor
comparison. Their order is intentional. It does not make every later item a
Metal v1.0 release gate: the existing Definition Of Done remains authoritative,
but any item that changes a planner-selectable lane must pass the same exact
correctness, dispatch, fallback, lifecycle, and performance contracts before
that lane is advertised.

- [x] **1. Close both existing P0 blockers before architecture expansion.** Pass
  every Native-Decline Overhead gate and separate lifecycle construction from
  steady-state measurement as specified above. New kernels or SQL surface must
  not hide, relabel, waive, or postpone either blocker.
- [x] **2. Make physical kernel mode part of admission and evidence.** Define a
  stable execution classification such as `parallel_hash`,
  `parallel_dense_count`, `parallel_dense_integer`, and `serial_generic`; carry
  it through descriptor capability validation, costing, `EXPLAIN`, counters,
  and benchmark artifacts. Normal planning must reject `serial_generic` unless
  that exact shape has independent winning evidence. Tests must prove that the
  reported mode matches the native branch that actually dispatched.
- [x] **3. Add descriptor-keyed specialization.** Build a cache keyed by the
  complete semantic descriptor/spec identity and generate branch-free released
  combinations of filter, membership, grouping, and measures. Start with
  precompiled or build-generated template variants; attempt runtime GPU query
  JIT only after compile latency, cache lifetime, backend duplication, archive
  provenance, cancellation, and cold/warm behavior are measured. AdaptiveCpp
  compiling a static generic kernel is not query-shape specialization.
- [x] **4. Build hierarchical reduction kernels.** For ungrouped and
  low-cardinality aggregates, use multiple per-work-item accumulators,
  workgroup-local reduction, and one bounded global merge instead of a
  contended global atomic for every input row. Preserve exact PostgreSQL NULL,
  overflow, accumulator-width, result-type, and cancellation semantics, and
  independently qualify each reduction shape.
- [x] **5. Cost fused execution versus reusable derived artifacts.** Provide an
  ephemeral path that fuses H3, spatial, and dimension transforms into their
  consuming aggregate, plus the existing dependency-stamped cached-artifact
  path. Admission must choose using construction bytes/time, expected reuse,
  cache-hit state, invalidation risk, launch count, and memory budget. Report
  the selected policy and observed artifact outcome in `EXPLAIN` and evidence.
- [x] **6. Remove hot-path allocation, launch, wait, and copy seams.** Complete
  the exact-shape workspace pool and reuse compatible output storage; poison
  and evict failed state. Reduce bounded accumulate calls, chain safe queue
  dependencies, pack bounded output into fewer transfers, and synchronize once
  at the final boundary where the cancellation contract permits. Keep launch
  count, allocation bytes, output bytes, and wait time separately observable.
- [x] **7. Improve resident representation and cold ingestion.** Evaluate compact
  NULL bitmaps, dictionary or bit-packed low-cardinality columns, block min/max
  metadata, and direct encoded-domain filtering/aggregation. Batch heap-page
  deformation and evaluate double-buffered staging into GPU-readable storage.
  Account raw, encoded, construction, transient, and retained-exact bytes; no
  lossy representation may weaken PostgreSQL semantics or recheck obligations.
- [x] **8. Quantify and address backend-local duplication.** Add N-backend tests
  for resident bytes, repeated loads, derived artifacts, AdaptiveCpp JIT/archive
  warmup, queue contention, throughput, latency, eviction, and invalidation.
  Based on measured cost, evaluate a background-worker GPU owner or another
  process-safe shared residency service without passing raw device pointers
  across PostgreSQL processes. Preserve cluster accounting and fail-closed
  cleanup under backend exit, fork, cancellation, and owner failure.
- [x] **9. Add broad end-to-end benchmark evidence.** Retain the exact selected-
  lane ratchets, then add SSBM, TPC-H, and ClickBench-style suites that report
  native declines as honestly as GPU selections. Compare with the best observed
  PostgreSQL-parallel plan and keep cold load, artifact construction, warm reuse,
  concurrency, memory footprint, and full output consumption separate. Do not
  publish a system-level speedup headline until the query coverage, storage
  policy, hardware, configuration, and scoring method make it reproducible.

## P1: Selected-Path Performance

No selected regression reproduced under the focused repeat: all 11 repeated
winners improved their speedup over the broad run. The following work is still
valuable for latency and headroom, but must not bypass correctness or lifecycle
evidence.

- [x] Profile `DescriptorAggPlan::new`, artifact ensure/lookup, derived-input
  binding, grouped-workspace allocation, output allocation, kernel execution,
  and tuple materialization separately.
- [x] Evaluate an exact-shape backend-local grouped workspace pool. Poisoned
  workspaces must be evicted; cancellation, fork/backend exit, generation changes,
  and device accounting must remain fail-closed and leak-free.
- [x] Return artifact evidence and byte accounting directly from ensure/rebuild so
  the executor does not repeat a lookup solely for reporting.
- [x] Reduce the 10M dense hash-aggregate lifecycle from 40 accumulate calls plus
  finalize. Test a dedicated 1M-row dense-session chunk boundary (target at most
  11 calls) before considering queued multi-range submission. Preserve interrupt
  latency and exact workspace limits.
- [x] Add dense-session launch count to cost estimation and wire completed
  aggregate batches into global batch counters; EXPLAIN and counters must agree.
- [x] Acceptance for these changes: one kernel/query where applicable, zero
  fallback, exact results, selected floor at least 1.15x, normalized median no
  worse than 5%, bounded cancellation, and clean fork/resource tests. The
  contamination-free same-host predecessor/current/current/predecessor control
  retains 60 samples per module per cell and passes all 11 comparisons: current
  speedups range from 2.624x to 13.944x and the worst PostgreSQL-normalized
  current/predecessor median factor is 0.9426. The immutable source artifact is
  `.codex/scratch/same-host-selected-abba-07c5012-vs-3a0bcd7-pg18-20260820-e`
  (`result.json` SHA-256
  `4fa5ced51307af60dd732e01e7efeea754d74d1a7c0b9c8292aaf4ee0c754163`,
  `SHA256SUMS` SHA-256
  `410e2c7032d580f56fd77e108d2b99d514bb205cf20731ef36c8adfb2dba3fbb`).
  Its frozen validator correctly remains failed on 44 numeric row/batch
  telemetry checks unavailable in the exact predecessor. The separately sealed
  compatibility addendum reproduces that output byte-for-byte, accepts only
  those legacy counters after proving selection, captured dispatch, nonzero
  GPU-kernel execution, 30 accelerated queries, exact diffs, and zero fallback
  in all 22 predecessor arm-cells, while retaining strict numeric telemetry for
  all 22 current arm-cells. The addendum `result.json` SHA-256 is
  `3998bac5f76fc36c6588153649a17b09e561af18bf143f0d16a166a57b0a79c7`;
  its manifest hashes to
  `db1e85f53a293f6908ac6318b2ab38e35161a8034b377b673ac7fec65b4df35e`.
  Exact Metal stress separately passes bounded cancellation, 8-by-20 fork
  stress, resource balance, and clean-log audit.
- [x] Add a dedicated release benchmark for the newly selected counted-dimension
  global `COUNT(*)` lane. `weighted_global_count_int4@1000000` freezes duplicate
  dimension keys, an unmatched fact key, and NULL with an independent no-join
  oracle. Its exact current-only gate returns 1,500,000, selects
  `descriptor_ungrouped_aggregate` / `dense_count_membership` /
  `parallel_dense_count`, executes one kernel/query with 10,000,000 dispatched
  and processed rows, zero fallback, a separately measured construction, and
  10/10 steady artifact hits with no build/rebuild. It measures 4.26x versus
  PostgreSQL parallel in
  `.codex/scratch/weighted-count-ship-gate-exact-1ba0b28-pg18-20260820-a`
  (`report.json` SHA-256
  `b7523353e0892b89840858ea1235bc7e8ad8cbfa9c0a305a5b06abaa8f0edee5`,
  seal SHA-256
  `0b8ae8bffc8691d84a9f305631f4939cfadbed0e3eb202c355267b6fc27f2a13`).
  The contamination-qualified A/B/B/A control retains 30 measured pairs per
  arm and 60 samples per module: current is 3.872 ms versus PostgreSQL 21.196 ms
  (5.473x), predecessor is 2328.532 ms versus PostgreSQL 21.216 ms (0.00911x),
  and the PostgreSQL-normalized factor is 0.001665 (600.75x improvement). The
  sealed source control is
  `.codex/scratch/weighted-count-same-host-abba-1ba0b28-vs-3a0bcd7-pg18-20260820-b`
  (manifest SHA-256
  `5847faaf0810a5ca1a8c788621930bdd09cf501d5c82defbc80dddc50396bcbf`).
  Its frozen validator intentionally remains failed only because supplementary
  exact queries ran after harness fixture cleanup. The separately sealed
  addendum reproduces that result byte-for-byte, accepts exactly those 24
  missing-table checks, and replaces them with fresh predecessor/current live
  proofs: accelerated, PostgreSQL, and independent-oracle results are all
  1,500,000; current dispatches one kernel, one batch, and 1,000,000 rows with
  zero fallback, while predecessor proves its five-kernel legacy path. Addendum
  `result.json` SHA-256 is
  `776575566a05c5b41f6644b625d4b201e5006d8c829b10f4fbe54a798f34eba3`;
  its seal hashes to
  `100126dc6d21b144dc7d1563670c9989672edb9cd35ca886a89ad46ff4451325`.

## P1: Measured Losing Lanes

These implementations remain internal or explicit native declines until they are
redesigned and independently requalified.

- [x] Multiple same-column range intersection: replace the former serial generic
  path (10,100.35 ms versus PostgreSQL 19.31 ms at 1M) with an exact
  `dense_integer_multiply_range` specialization. The planner now fuses exactly
  two int4 bounds over the product lhs, and the parallel dense-integer row
  kernel tests the inclusive interval from the same lhs load used by
  multiplication without a derived mask. Nullable predicate, RHS, and group
  semantics, both hierarchical and 256-group atomic execution, int4 expression
  overflow with untouched output, malformed sidecars, physical-mode reporting,
  prepared DML/DDL, and adjacent native declines are covered by Rust, native
  C++, PG18 SQL95-SQL97, and the semantic matrix. A
  5-warmup/10-pair-per-scale characterization measured 2.32 ms versus
  PostgreSQL 10.12 ms (4.36x) at 250K and 3.50 ms versus 28.12 ms (8.05x) at
  1M, with 20/20 artifact hits, verified `parallel_dense_integer`, and zero
  fallback in
  `.codex/scratch/range-intersection-fastpath-envelope-characterization`
  (`artifact_index.json` SHA-256
  `4324d14e7dfbc7af12d85fc0e7e6c7a71513b213cca2290518023b06465d7918`).
  Preserve `shape_multiple_range_predicates` for a third bound and broader
  SSBM shapes. The characterization ran while unrelated CPU-heavy processes
  were active and has provenance warnings, so rerun clean exact-candidate
  release evidence before publication.
- [x] Grouped `COUNT(bool_column)`: replace the former serial generic path
  (3,508.11 ms versus PostgreSQL 18.87 ms) with a hierarchical dense bool-count
  specialization that tracks selected and non-NULL rows independently. The
  exact 1M-row distinct-key/measure shape now reports verified
  `parallel_dense_count`, preserves all-NULL group activity, and keeps global,
  same-column, filtered, joined, and multi-measure variants native. PG18 SQL94,
  SQL95 DML/DDL/prepared lifecycle, SQL96 adjacent decline, native C++, and Rust
  contracts pass. A 5-warmup/10-pair-per-scale characterization measured 1.66
  ms versus PostgreSQL 9.59 ms (5.77x) at 250K and 2.92 ms versus 21.13 ms
  (7.23x) at 1M, with 20/20 artifact hits and zero fallback in
  `.codex/scratch/bool-count-fastpath-envelope-characterization`; rerun the
  clean exact release artifact after the unrelated CPU-saturating process is
  removed.

## P1: Profitable Unqualified Surface

- [x] Expressions and predicates: arithmetic, `CASE`, multiple `AND` ranges,
  `IN`, `IS NULL`, and bounded `FILTER`/`HAVING`, with exact NULL, overflow,
  divide-by-zero, cast, NaN, and collation behavior.
  - The 1.0 evaluation is complete: only the two-bound int4 product interval
    and exact aggregate-local int4 FILTER lanes below are admitted. The broader
    expression surface stays PostgreSQL-native. SQL14/15, SQL30-34, SQL40-42,
    SQL80, SQL87, SQL96-98, planner-shape tests, and the semantic declaration
    matrix cover result parity, edge semantics, selected envelopes, and native
    structural boundaries. Post-1.0 expansion is tracked in
    <https://github.com/yocontra/pg_accel/issues/1> and is not shipped support.
  - [x] Release the first bounded aggregate-local `FILTER` lane: one direct
    nullable int4 `SUM(value) FILTER (WHERE value >= lo AND value <= hi)` plus
    one unfiltered `COUNT(*)`, grouped by one distinct int4 fact column. The
    descriptor, cost model, specialization cache, Rust physical-mode backstop,
    and hierarchical/atomic Metal kernels keep group activity and COUNT
    independent from filtered SUM state. One-sided, wrong-column, filtered
    COUNT, joined, fact-filtered, HAVING, and broader measure variants remain
    native. Rust, native C++, PG18 SQL95/SQL98 DML/DDL/prepared lifecycle, and
    the 25-family/325-assertion semantic declaration matrix pass. A
    5-warmup/10-pair-per-scale characterization measured 3.11 ms versus
    PostgreSQL 9.28 ms (2.98x) at 250K and 3.35 ms versus 21.53 ms (6.42x) at
    1M, with 20/20 artifact hits, verified `parallel_dense_integer`, exact
    diffs, and zero fallback in
    `.codex/scratch/aggregate-filter-functional-pg18-20260811`;
    `artifact_index.json` SHA-256 is
    `c15a88f297e663204036d8d857b800dbbbac9e4bbd5dc4877a6c024e4e1802ee`.
    The host still had the unrelated CPU-saturating process, so rerun a clean
    exact-candidate ship-gate cell before publication.
- [x] Types and aggregates: bool/int2/general int8, float4/float8, date/time,
  integer `AVG`, and safe `SUM` combinations. Preserve PostgreSQL accumulator,
  result, and overflow semantics; never approximate `NUMERIC` with f64.
  - The 1.0 type decision is complete: the exact typed COUNT and widened
    int2/int4 SUM/NUMERIC-AVG envelopes below ship; adjacent global, filtered,
    joined, non-boolean-grouped, modifier, broader-measure, int8/numeric/
    interval aggregate shapes remain native. SQL94 and SQL99-104 plus the
    planner/runtime/ABI contracts prove the admitted types, special values,
    result OIDs, null sidecars, widening, and PostgreSQL NUMERIC finalization.
    Broader combinations are tracked in
    <https://github.com/yocontra/pg_accel/issues/1> and are not shipped support.
  - [x] Release the first exact `int2` aggregate lane: one nullable boolean
    fact-column group key and `COUNT` over one distinct nullable `int2` fact
    column, with no filter, join, HAVING, or additional measure. The planner,
    specialization cache, Rust physical-mode backstop, and C++ descriptor gate
    admit only the widened resident `int32` representation used for PostgreSQL
    `int2`; the kernel reads only the null sidecar and retains all-NULL groups
    with count zero. Full-domain endpoints, invalid null bytes, adjacent native
    shapes, PG18/PG19 SQL99 plus prepared DML/DDL lifecycle, and the
    27-family/331-assertion semantic matrix pass. The complete PG18 and PG19
    validation matrices pass: 1,539 core plus 55 correctness unit tests, 62 SQL
    contracts, and 580 planner-shape/stress tests on each version; all 32 native
    GPU CTests and the fail-closed safety audits pass as well. A diagnostic
    5-warmup/10-pair characterization measured 1.58 ms versus PostgreSQL 8.26
    ms (5.23x) at 250K and 2.29 ms versus 19.26 ms (8.42x) at 1M, with 20/20
    artifact hits, verified `parallel_dense_count`, exact diffs, and zero
    fallback in
    `.codex/scratch/grouped-count-int2-functional-pg18-20260811`;
    `artifact_index.json` SHA-256 is
    `6ca9987bb3ae1c6a5b1e69989a4f3db2f0e81d93ee2e14a42c3a9aa621f57844`.
    The unrelated CPU-saturating process and diagnostic provenance warnings
    make these functional measurements non-publishable; rerun the clean exact
    ship-gate cell before release.
  - [x] Release the first exact general `int8` aggregate lane: one nullable
    boolean fact-column group key and `COUNT` over one distinct nullable `int8`
    fact column, with no filter, join, HAVING, or additional measure. The
    planner, specialization cache, Rust physical-mode backstop, C++ descriptor
    gate, and host ABI contract admit only the exact `INT64`/8-byte count shape;
    the kernel reads only the validated null sidecar, so `INT64_MIN` and
    `INT64_MAX` never enter device arithmetic and all-NULL groups retain count
    zero. Endpoint fixtures, invalid null bytes, adjacent native shapes,
    PG18/PG19 SQL100 plus prepared DML/DDL lifecycle, and the
    29-family/337-assertion semantic matrix pass. The complete PG18 and PG19
    validation matrices pass: 1,540 core plus 55 correctness unit tests, 63 SQL
    contracts, and 582 planner-shape/stress tests on each version; all 32 native
    GPU CTests and 137 fail-closed coverage/safety tests pass as well. A
    diagnostic 5-warmup/10-pair characterization measured 1.40 ms versus
    PostgreSQL 8.48 ms (6.05x) at 250K and 2.28 ms versus 18.96 ms (8.30x) at
    1M, with 20/20 artifact hits, verified `parallel_dense_count`, exact diffs,
    and zero fallback in
    `.codex/scratch/grouped-count-int8-functional-pg18-20260811`;
    `artifact_index.json` SHA-256 is
    `82ec1de4597f6c27b82a22fdff288a9c8e6a30996e4331937b3f54d2e43ba422`.
    The unrelated CPU-saturating process and installed/local module-provenance
    mismatch make these functional measurements non-publishable; rerun the
    clean exact ship-gate cell before release.
  - [x] Release the first exact `date` aggregate lane: one nullable boolean
    fact-column group key and `COUNT` over one distinct nullable PostgreSQL
    `date` fact column, with no filter, join, HAVING, or additional measure.
    The planner, specialization cache, Rust physical-mode backstop, C++
    descriptor gate, and host ABI contract admit only the exact `DATE`/4-byte
    count shape. The kernel reads only the validated null sidecar, so finite
    dates plus PostgreSQL `-infinity` and `infinity` sentinels never enter
    device interpretation or arithmetic, while all-NULL groups retain count
    zero. Infinity fixtures, invalid null bytes, adjacent native shapes,
    PG18/PG19 SQL101 plus prepared DML/DDL lifecycle, and the
    31-family/343-assertion semantic matrix pass. The complete PG18 and PG19
    validation matrices pass: 1,541 core plus 55 correctness unit tests, 64 SQL
    contracts, and 584 planner-shape/stress tests on each version; all 32 native
    GPU CTests and 137 fail-closed coverage/safety tests pass as well. A
    diagnostic 5-warmup/10-pair characterization measured 1.56 ms versus
    PostgreSQL 8.73 ms (5.58x) at 250K and 2.60 ms versus 19.86 ms (7.64x) at
    1M, with 20/20 artifact hits, verified `parallel_dense_count`, exact diffs,
    and zero fallback in
    `.codex/scratch/grouped-count-date-functional-pg18-20260811-b`;
    `artifact_index.json` SHA-256 is
    `814d9ebd05cb6881a0e5b0d3dd88a3c40659c030fab83aff17c7af22b659e3d4`.
    The unrelated CPU-saturating process and installed/local module-provenance
    mismatch make these functional measurements non-publishable; rerun the
    clean exact ship-gate cell before release.
  - [x] Release the first exact timestamp aggregate lanes: one nullable boolean
    fact-column group key and `COUNT` over one distinct nullable PostgreSQL
    `timestamp` or `timestamptz` fact column, with no filter, join, HAVING, or
    additional measure. The two logical OIDs retain distinct descriptor/cache
    identities while sharing the validated `TIMESTAMP`/8-byte physical count
    ABI. The kernel reads only the validated null sidecar, so finite values,
    timezone interpretation, and PostgreSQL `-infinity`/`infinity` sentinels
    never enter device interpretation or arithmetic, while all-NULL groups
    retain count zero. Temporal infinity fixtures, invalid null bytes, adjacent
    native shapes, PG18/PG19 SQL102 prepared DML/DDL lifecycle, and the
    33-family/349-assertion semantic matrix pass. The complete PG18 and PG19
    validation matrices pass: 1,542 core plus 55 correctness unit tests, 65 SQL
    contracts, and 586 planner-shape/stress tests on each version; all 32 native
    GPU CTests and 137 fail-closed coverage/safety tests pass as well. Diagnostic
    5-warmup/10-pair characterizations measured 1.48 ms versus PostgreSQL 8.28
    ms (5.60x) at 250K and 2.68 ms versus 18.82 ms (7.03x) at 1M for
    `timestamp`, and 1.58 ms versus 8.72 ms (5.51x) at 250K and 2.36 ms versus
    18.95 ms (8.02x) at 1M for `timestamptz`. Both retained 20/20 artifact hits,
    verified `parallel_dense_count`, exact diffs, and zero fallback in
    `.codex/scratch/grouped-count-timestamp-functional-pg18-20260811` and
    `.codex/scratch/grouped-count-timestamptz-functional-pg18-20260811`;
    their `artifact_index.json` SHA-256 values are
    `266a033d69a69948437ae0e80633a4aab89844320b16f54e3ef02c6f885179ec`
    and
    `6b5fd6caf26a41ed65b1f9f4e7520685ba72f77ef4dd9aa22375a146bc49893a`.
    The unrelated CPU-saturating process and installed/local module-provenance
    mismatch make these functional measurements non-publishable; rerun both
    clean exact ship-gate cells before release.
  - [x] Release the first exact floating-point aggregate lanes: one nullable
    boolean fact-column group key and `COUNT` over one distinct nullable
    PostgreSQL `float4` or `float8` fact column, with no filter, join, `HAVING`,
    or additional measure. The two logical/physical widths retain distinct
    descriptor/cache specializations, `dense_float4_count_plain` and
    `dense_float8_count_plain`, while sharing the validated null-sidecar-only
    `parallel_dense_count` kernel. Count-only floating descriptors use an exact
    int64 count state and do not pay the FP64-arithmetic cost multiplier. The
    kernel never interprets the measure payload, so PostgreSQL `NaN`, positive
    and negative infinity, and both signed zero representations affect COUNT
    only through their NULL status; all-NULL groups retain count zero. Special-
    value fixtures, invalid null bytes, adjacent native shapes, PG18/PG19 SQL103
    prepared DML/DDL lifecycle, and the 35-family/355-assertion semantic matrix
    pass. The complete PG18 and PG19 validation matrices pass: 1,545 core plus
    55 correctness unit tests, 66 SQL contracts, and 588 planner-shape/stress
    tests on each version; all 32 native GPU CTests and 137 fail-closed
    coverage/safety tests pass as well. The C++ source-tree build dependency
    contract now emits every kernel source/header path, preventing Cargo from
    linking a stale static kernel archive after an in-place native edit.
    Diagnostic 5-warmup/10-pair characterizations measured 1.52 ms versus
    PostgreSQL 8.10 ms (5.32x) at 250K and 2.21 ms versus 18.56 ms (8.41x) at
    1M for `float4`, and 1.56 ms versus 8.53 ms (5.46x) at 250K and 2.36 ms
    versus 18.76 ms (7.94x) at 1M for `float8`. Both retained 20/20 artifact
    hits, verified `parallel_dense_count`, exact diffs, zero fallback, and an
    exact installed/expected module SHA-256 of
    `2d6574990fe06be6e9e1a4be32bc3672a9d080d4a933b71a3c3d0e87ebc7337d`
    in
    `.codex/scratch/grouped-count-float4-functional-pg18-20260811` and
    `.codex/scratch/grouped-count-float8-functional-pg18-20260811`;
    their `artifact_index.json` SHA-256 values are
    `96cfe549d9e63465c146263ca3a74706b784c60c11e7a87876f618bf26d5422f`
    and
    `4250552126d2e0eb3150adfe6095fc46d344aee317fc99d09fbbfb26a144a999`.
    The unrelated Bun process remained at roughly 99% CPU throughout, so these
    functional measurements are non-publishable; rerun both clean exact
    ship-gate cells on an eligible host before release.
  - [x] Release the first exact widened-integer SUM/AVG lanes: one nullable
    boolean fact-column group key, one distinct nullable `int2` or `int4` fact
    measure projected as `SUM(value)` and `AVG(value)`, and one `COUNT(*)`, with
    no filter, join, `HAVING`, or additional measure. Both specializations use
    the exact int64 SUM and non-NULL-count state produced by
    `parallel_dense_integer`; SUM materializes as PostgreSQL `int8`, while AVG
    performs PostgreSQL NUMERIC division on the backend thread. No integer AVG
    is approximated through floating point. Full-domain endpoints, all-NULL
    groups, independent arithmetic oracles, result OIDs, zero fallback,
    descriptor specialization, prepared DML/DDL lifecycle, and AVG-only /
    missing-COUNT native declines are covered by PG18/PG19 SQL104 and the
    37-family/361-assertion semantic matrix. The complete PG18 and PG19
    validation matrices pass: 1,547 core plus 55 correctness unit tests, 67 SQL
    contracts, and 591 planner-shape/stress tests on each version; all 32 native
    GPU CTests, 137 coverage-audit tests, and 171 adversarial CPU-cheat tests
    pass as well. Diagnostic 5-warmup/10-pair characterizations measured 3.40
    ms versus PostgreSQL 11.32 ms (3.33x) at 250K and 10.03 ms versus 23.82 ms
    (2.37x) at 1M for `int2`, and 3.58 ms versus 10.68 ms (2.98x) at 250K and
    10.69 ms versus 30.58 ms (2.86x) at 1M for `int4`. Both retained 20/20
    artifact hits, verified `parallel_dense_integer`, exact diffs, and zero
    fallback in
    `.codex/scratch/grouped-int2-sum-avg-functional-pg18-20260818` and
    `.codex/scratch/grouped-int4-sum-avg-functional-pg18-20260818`; their
    `artifact_index.json` SHA-256 values are
    `b71cdecdb858507c929fac8b76d1f37aa9acb45aab2a40a4e6e03b8d09973225`
    and
    `315761bc38945ab9041410f7f12d411e6eec9d3d0b66bb8fe14235c275d697f1`.
    The measurements ran while an unrelated long-lived Bun process saturated a
    CPU, so they are functional evidence only; rerun both exact 1M ship-gate
    cells on an eligible host before publication.
- [x] Membership and joins: composite keys, broader exact int8 shapes,
  catalog-proved collation-safe text, and reducing semi/anti membership with exact
  NULL, `NOT IN`, duplicate, and multiplicity semantics. Row-returning joins stay
  native until a bounded winning design exists.
  - The 1.0 evaluation keeps only the qualified single-key/count and resident
    star-membership descriptors. Composite, broader int8, text, semi/anti,
    `NOT IN`, and row-returning variants remain native; planner tests cover
    collation identity and semi/anti rejection, while SQL21/55/56/91/95/96
    cover NULL, duplicate, multiplicity, selected reuse, and native output.
    Expansion is accepted as a post-1.0 deferral in
    <https://github.com/yocontra/pg_accel/issues/1>.
- [x] H3: expose resident `h3_latlng_to_cell` only as a fused reducing group
  producer, then compose it with parent rollup, filters, measures, and joins.
  - The 1.0 evaluation admits only fused `h3_cell_to_parent` grouped COUNT.
    `h3_latlng_to_cell`, its composed grouped forms, and variable-output SRFs
    remain native with explicit planner declines and exact h3-pg parity in
    SQL12/85/91/95/96 and the H3 protection tests. Expansion is tracked in
    <https://github.com/yocontra/pg_accel/issues/1>.
- [x] Spatial: expand beyond the released 1M-row, 1,025-coordinate
  `ST_Intersects(point, one-ring polygon)` count only after differential recheck,
  cancellation, crash-band, and performance qualification. Next candidates are
  `Contains`, `Within`, and point-point `DWithin`.
  - The candidates were evaluated and remain PostgreSQL-native for 1.0.
    SQL19/20/80-82/85/87 prove PostGIS parity across `Contains`, `Within`, and
    `DWithin`; the selected ST_Intersects envelope separately has exact
    lifecycle, recheck, cancellation, crash-band, and performance evidence.
    Expansion is tracked in <https://github.com/yocontra/pg_accel/issues/1>.
- [x] Raster: expand beyond the released resident three-argument integer
  `ST_Reclass` pixel envelope. NDVI, slope, clip, summaries, and map algebra need
  reconstruction-byte accounting, malformed/NULL/nodata tests, and winning evidence.
  - The 1.0 evaluation admits only the exact three-argument integer ST_Reclass
    envelope. SQL85 proves native Clip, MapAlgebra, summary consumption, and
    output parity; SQL93/95 prove selected Reclass lifecycle and exact WKB;
    malformed, NULL, nodata, and reconstruction accounting are fail-closed.
    Broader raster work is tracked in
    <https://github.com/yocontra/pg_accel/issues/1>.
- [x] Sort/window: evaluate only cardinality-reducing top-N, rank-filter, or
  window-to-aggregate forms. Full-output scans, projections, sorts, windows, and
  row-returning joins remain intentionally native.
  - No bounded candidate met the 1.0 resident/evidence bar. SQL13/18/43/60/61
    and SQL96 prove exact PostgreSQL ordering, NULL, duplicate, peer, frame,
    top-k, and window behavior with structural native declines and zero device
    dispatch. Future cardinality-reducing designs are tracked in
    <https://github.com/yocontra/pg_accel/issues/1>.

## P1: Residual Safety And Coverage

- [x] Run PostgreSQL's upstream regression and isolation suites for every
  supported major against both a pristine server and the exact candidate loaded
  in every test session. Require zero pg_accel-caused result diffs, crashes,
  hangs, leaked hooks, or changed planner/executor semantics; archive the exact
  PostgreSQL SHA, build flags, schedules, expected/actual diffs, server logs, and
  platform-qualified exclusions as durable release evidence. Add the gate to CI
  so hook compatibility is continuously tested rather than inferred from the
  extension's own SQL tests. Context:
  <https://malisper.me/pgrust-passes-100-of-postgresqls-regression-tests/>.
  Current clean candidate `07c5012` (tree `d710907`) passes pristine
  regression, pristine isolation, loaded regression, and loaded isolation on
  both PG18.4 and PG19beta1. The loaded module SHA-256 values are
  `21cc218f68789bdf54c9cc6bce82417f13228798837b810bf356bb9158a335a3`
  for PG18 and
  `d92d34067aa6454187186bc9d7ebff08242e2fd4253f1862739b2cba38c42157`
  for PG19. Clean-tree/module-bound evidence is retained in
  `.codex/scratch/upstream-postgresql-exact-07c5012-20260820-a`; the PG18 and
  PG19 `SHA256SUMS` SHA-256 values are
  `a13614e9a00f942b630b1dede249c107c26e0d9ec03f39ae145d79105ce18390`
  and
  `bc4bcbd5887f00c17fdce8d425d365a6e0b013cfc3a54ef27dd19bcf27dba2a6`.
  Any later source or module change does not inherit those results: the next
  frozen release candidate must rerun all four schedules on both supported
  majors before publication.
- [x] Extend failure injection across multi-session residency/invalidation,
  executor reset/drop, planner private data, allocation/free, copy/wait,
  cancellation, output materialization, PostGIS calls, and derived-artifact
  publication. Require exactly-once cleanup, balanced ledger, and backend reuse.
- [x] Property/fuzz private-data codecs, descriptors and PostgreSQL lists,
  geometry/raster/H3 packed inputs, byte/cardinality overflow, pointer aliasing,
  and C ABI layouts. Malformed input must fail before allocation or dereference.
- [x] Add risk-weighted coverage for every unsafe FFI, lifetime, cleanup,
  invalidation, and cancellation branch in addition to global percentage gates.
- [x] Keep historical crash-prone grouped/hash-join cardinalities structurally
  gated until redesigned kernels pass exact crash-band, cancellation, and memory
  stress. Do not present guarded code as reachable support.
- [x] Close remaining declaration gaps in `coverage/sql-semantic-matrix.json`,
  prioritizing declined aggregate modifiers, base/row-returning paths, H3 scalar
  and SRF shapes, sort/top-k/window, and neighboring raster/spatial declines.
- [x] Add live PG19 package/install/SQL evidence. The PostgreSQL 19beta1
  release build installed and loaded module SHA-256
  `58efe8b92266eda727df8d5baf1d300d6978c63778616bc36bb3ec39d3db9d08`;
  extension smoke, 60/60 external SQL files, and both loaded upstream schedules
  pass. PG19 lint, check, and test compilation also pass as separate gates.

## Release And Publication Gates

- [x] Produce a fresh exact-candidate coverage and enriched Metal stress bundle
  covering mixed workloads, fork, cancellation, concurrency, memory pressure,
  per-kernel JIT/archive cold/warm evidence, clean logs, and resource balance.
  Current clean candidate `07c5012` passes the three-layer PG18 gate at 90.17%
  Rust source coverage (49,166/54,528; 210/210 required mappings), 90.01%
  C++/SYCL source coverage (16,827/18,695; 32/32 native Metal tests), and 100%
  SQL semantic coverage (361/361 assertions across 67/67 files). Evidence is in
  `.codex/scratch/coverage-exact-07c5012-pg18-20260820-a`;
  `gate-summary.json` SHA-256 is
  `7e88beb53b2904937faaffc96f61788b7cbb52713acc1b7cceaf459053580dff`
  and `provenance.json` SHA-256 is
  `0fad7357d5a6985c6296012f30b63e08bc0c85cb137fc95fec89aaa42883081f`.
  The same exact candidate's full Metal stress bundle is at
  `.codex/scratch/metal-stress-exact-07c5012-pg18-20260820-a`.
  It passes strict SQL, 32/32 native tests, OOM, cancellation, archive cold/warm,
  8-by-20 fork stress, six crash probes, resource/log audit, and artifact-index
  verification across 222 indexed files. Its `artifact_index.json` SHA-256 is
  `1f91fcd4d9bfbc23d2b413e2398c43ebee61e94109335ce5d2bfd35b1c97a0ee`;
  candidate-provenance SHA-256 is
  `c731b42fbacb015fdcd1de6fc107d2c5f2c0446bbba45c0c7d56bc459d9e9e30`.
  Benchmark timings inside the stress bundle are correctness/stability
  characterization only because unrelated CPU load made the host ineligible
  for performance claims.
- [ ] Pass hosted release CI on macOS arm64 Metal and Linux x86_64 no-GPU, with
  durable artifacts from the exact candidate. Exact-candidate run
  `31550632369` reached GitHub Actions, but every hosted job was rejected before
  its first step because the account has a failed payment or exceeded spending
  limit. Restore Actions billing, then rerun that SHA; the zero-step failure is
  not project execution evidence.
- [ ] Verify public source-build, package, install, and `CREATE EXTENSION`
  instructions from a clean checkout on a fresh Apple Silicon machine.
- [x] Accept the 1B-row scale gate as optional, environment-deferred
  certification. The release makes no 1B claim, no smaller fixture is
  represented as 1B evidence, and the real-hardware/storage gate is durably
  tracked in <https://github.com/yocontra/pg_accel/issues/2>.
- [ ] Finish the public release checklist: replace placeholders with durable
  SHAs, CI URLs, artifacts, explicit accepted deferrals, or named sign-off; pass
  `just release-checklist-audit` and `just release-verify` honestly.
- [ ] Publish `v1.0.0-rc1`, monitor that exact candidate for one week, then
  publish `v1.0.0` with checksums, release notes, packages, benchmark evidence,
  limitations, and owner/reviewer sign-off.
- [x] Accept the privileged OS page-cache arm as optional certification and
  track it in <https://github.com/yocontra/pg_accel/issues/3>. The mandatory
  unprivileged warm matrix and project-owned JIT/archive cold-start evidence
  remain separate; no OS-cold claim is made. Preserve the failed exact
  nineteen-cell Metal ship-gate result where the required OS page-cache purge
  could not complete for every H3 cold iteration. The clean,
  continuously guarded current-candidate run at
  `.codex/scratch/metal-ship-gate-exact-07c5012-pg18-20260820-b` has no crashes,
  exact correctness, real dispatch, zero fallback, and all 19 warm speedups
  above their 1.15x floors (minimum 3.174x), but the built-in gate correctly
  fails `h3_cell_to_parent` with `expected_winner_missing_cache_evidence`
  because `/usr/sbin/purge` returned `Operation not permitted` for every cold
  sample. Its 104-entry terminal manifest hashes to
  `38668111af0cb7a8f42523ca4b48dec83b7ff17e714a2869dc873245c094d731`;
  `report.json` hashes to
  `59fadfab58213e54a3d80f21fad50ea32e51777b9a242592e0fcaba05e14556c`.
  Preserve the failed result; rerun on a privileged or otherwise qualified host
  without weakening the completed-purge requirement or inferring cold evidence
  from warm-only timings.

## OWNER-DEFERRED: CUDA, NVIDIA, And PG-Strom

The owner deferred this work until a CUDA device is available. It does not block
the Metal-only build, but no CUDA/NVIDIA/PG-Strom claim is permitted beforehand.

- [x] Owner-defer building the pinned AdaptiveCpp revision with CUDA and the
  Rust/C/C++ ABI proof to <https://github.com/yocontra/pg_accel/issues/4>.
- [x] Owner-defer CUDA correctness, FP64, cold/warm, fork, cancellation, memory-pressure,
  crash-band, packaging, and `just cuda-stress` gates with durable artifacts.
  Tracked in <https://github.com/yocontra/pg_accel/issues/4>.
- [x] Owner-defer CUDA device-counter lowering and Metal-equivalent coverage to
  <https://github.com/yocontra/pg_accel/issues/4>.
- [x] Owner-defer independent CUDA admission calibration; Metal limits are not
  copied. Tracked in <https://github.com/yocontra/pg_accel/issues/4>.
- [x] Owner-defer PG-Strom installation on the same PostgreSQL/CUDA host and like-for-like
  configuration, correctness, plan, and timing evidence.
  Tracked in <https://github.com/yocontra/pg_accel/issues/4>.
- [x] Owner-defer CUDA CI and release artifacts to
  <https://github.com/yocontra/pg_accel/issues/4>; NVIDIA support remains
  unadvertised until that issue closes.

## Definition Of Done

The Metal release is ready only when the exact candidate passes all selected and
native performance contracts, fresh safety/coverage/stress gates, hosted CI,
fresh-machine installability, non-deferred release-checklist rows, and named
sign-off with no known critical/high defect. Operator expansion and CUDA may
remain open, but neither may be represented as shipped support.
