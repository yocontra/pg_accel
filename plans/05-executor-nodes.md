# Phase 5: Executor Nodes

**Depends on:** Phase 3 (planner FFI working)
**Parallelism:** Runs in parallel with Phase 6 (both start after Phases 3+4 complete).
6 agents (A0–A4b). Max 5–6 concurrent agents across Phases 5+6 combined.

This phase implements the actual execution logic behind the Custom Scan nodes.
After Phase 3 gave us the planner hooks to inject our nodes into query plans,
this phase makes those nodes actually DO something.

**Implementation note:** The Phase 3 spike gives us a passthrough Custom Scan.
This phase replaces the passthrough with real batch dispatch. The key callback
is `ExecCustomScan` — instead of calling `ExecProcNode(child)` for each tuple,
we accumulate tuples into `BatchAccumulator` (from Phase 2) and dispatch them
through the engine. The existing `dispatch.rs` and `dispatch_fallback.rs` are
already functional for BatchedEval strategy.

---

## Agent Assignments

### A0 — GpuAccelScan: Core Execution
**Status:** Complete
**Owns:** `pg_accel/src/engine/executor/scan.rs`

**Tasks:**
- [x] Implement `BeginCustomScan` callback:
  - [x] Read strategy + config from `custom_private`
  - [x] Acquire thread budget from shared memory
  - [x] Initialize batch buffer (4096 rows default)
  - [x] Resolve fmgr_info for accelerated functions in WHERE clause
  - [x] Open child scan (the underlying SeqScan/IndexScan)
- [x] Implement `ExecCustomScan` callback (called repeatedly by PG executor):
  - [x] Pull tuples from child scan into batch buffer
  - [x] When buffer full (or child exhausted):
    - [x] Batch-deserialize columns needed by cheapest predicate
    - [x] Evaluate predicates via appropriate strategy:
      - GpuSpatial predicates: GPU three-layer pipeline (batch all candidates)
      - GpuH3 predicates: GPU h3 kernel (batch all lat/lng pairs)
      - BatchedEval predicates: main thread, one at a time
    - [x] For multi-predicate WHERE: evaluate cheapest first, only deserialize expensive columns for rows that passed cheap predicates
    - [x] Return passing rows one at a time to parent node
  - [x] When LIMIT reached: stop pulling, release resources
- [x] Implement GiST index scan batched recheck for PostGIS (`&&` operator -> GiST -> candidates): accumulate all candidate geometry pairs, send to GPU three-layer pipeline; Layer 1 bbox is redundant (GiST already did it), so skip to Layer 2 geometric fast-path (this is the single biggest win for spatial queries with indexes)
- [x] Implement GiST index scan batched recheck for h3-pg (`@>` / `<@` / `&&` on h3index -> GiST -> candidates): accumulate candidate cell IDs, batch-evaluate exact containment/overlap via GPU h3 kernel (h3 cell operations are pure integer math -- GPU processes millions of cell comparisons in one kernel launch)
- [x] Implement SP-GiST index scan batched recheck for h3-pg (h3index provides an SP-GiST operator class leveraging the hierarchical cell structure -- parent/child relationships map naturally to SP-GiST's space-partitioning tree): same batched recheck as GiST path; SP-GiST traversal on main thread (PG internal), candidates accumulated, exact recheck via GPU h3 kernel in bulk; SP-GiST may produce fewer candidates than GiST for hierarchy queries (`@>` containment) due to tighter partitioning, but the recheck path is identical -- accumulate cell IDs, batch GPU h3 kernel
- [x] Implement generic GiST / SP-GiST batched recheck (any registered recheck function): accumulate candidates, batch-evaluate recheck via BatchedEval with late materialization (still a win for wide tables -- skip non-recheck columns)
- [x] Ensure in all GiST/SP-GiST cases that PG's tree traversal happens on the main thread (PG internal), with batching happening at the recheck stage after candidates are collected
- [x] Implement `EndCustomScan` callback:
  - [x] Release thread budget
  - [x] Update stats counters
  - [x] Close child scan

**Agent gate:**
- [x] `SELECT * FROM big WHERE expensive_func(x) LIMIT 10` -> 10 correct rows via GpuAccelScan
- [x] `SELECT COUNT(*) FROM big WHERE expensive_func(x)` -> correct count
- [x] GiST recheck: `SELECT * FROM points WHERE ST_Contains(polygon, geom)` with GiST index -> GpuAccelScan wraps IndexScan, batched GPU recheck, identical results to vanilla
- [x] GiST recheck: `SELECT * FROM h3_data WHERE cell @> target_cell` with h3 GiST index -> batched h3 kernel recheck, identical results
- [x] SP-GiST recheck: `SELECT * FROM h3_data WHERE cell @> target_cell` with h3 SP-GiST index -> batched h3 kernel recheck, identical results to GiST path and vanilla
- [x] Results match vanilla PG (`pg_accel.enabled = off`) exactly
- [x] EXPLAIN ANALYZE shows batch count, dispatch time, recheck stats
- [x] LIMIT doesn't over-dispatch (stats show ~batch_size rows, not all rows)

**Implementation log:**
Implemented in `src/engine/executor/scan.rs` and `src/engine/ffi/custom_scan.rs`. BeginCustomScan/ExecCustomScan/EndCustomScan callbacks, batch accumulation, GiST recheck detection, multi-strategy dispatch.

### A1 — GpuAccelScan: Vectorized Deserialization
**Status:** Complete
**Owns:** `pg_accel/src/engine/executor/deser.rs`

**Tasks:**
- [x] Implement column-at-a-time deserialization for batches instead of per-tuple all-column deserialization:
  ```
  Standard PG: for each tuple -> deserialize all columns -> evaluate WHERE
  Our approach: for all 4096 tuples -> deserialize col1 only -> filter on col1
               -> for survivors -> deserialize col2 -> filter on col2
               -> for survivors -> deserialize expensive cols (geometry, jsonb)
  ```
- [x] Implement late materialization: only touch expensive columns for rows that passed cheap filters
- [x] Implement column cost heuristic for predicate ordering:
  - int/float/bool/timestamp: cheap (fixed-size, inline datum)
  - text: medium (varlena header + length)
  - geometry/jsonb: expensive (complex deserialization)
- [x] Ensure correct results regardless of predicate evaluation order
- [x] Handle NULLs in any column position correctly

**Agent gate:**
- [x] Benchmark: wide table (10 cols), cheap int filter (95% selectivity) + expensive geometry predicate -> vectorized path faster than naive "deserialize everything" path
- [x] Correct results regardless of predicate evaluation order
- [x] NULL handling: NULLs in any column position handled correctly

**Implementation log:**
Implemented in `src/engine/executor/deser.rs`. Column cost classification (cheap/medium/expensive), plan_column_order for predicate ordering, late materialization.

### A2 — GpuAccelJoin: Nested Loop Batched
**Status:** Complete
**Owns:** `pg_accel/src/engine/executor/join.rs` (nested loop portion)

**Tasks:**
- [x] Implement batched nested loop join:
  - [x] Accumulate N outer tuples (batch)
  - [x] For each outer tuple, probe inner side on main thread (index access is PG internal, cannot be called from rayon threads -- uses PG buffer manager, syscache, etc.)
  - [x] Collect all (outer, inner) matched pairs
  - [x] Batch-evaluate residual conditions on matched pairs:
    - GpuSpatial residuals (e.g., ST_Intersects): GPU three-layer pipeline
    - BatchedEval residuals: main thread, batched
  - [x] Return passing joined tuples to parent
- [x] Implement spatial join path (the primary use case):
  - Outer: accumulate points/geometries
  - Inner: index scan on spatial index (main thread, one at a time)
  - Residual: `ST_Intersects(outer.geom, inner.geom)` -> GPU three-layer or batched eval
  - The win is in residual eval (step 4), not index probe (step 2)
- [x] Handle empty join result (no crash, returns 0 rows correctly)
- [x] Handle NULL join keys correctly (NULL != NULL)

**Agent gate:**
- [x] Spatial join 10K x 1K: result set identical to vanilla PG NestLoop
- [x] Hash join residual: `a JOIN b ON a.key = b.key AND a.ts < b.ts` 100K rows -> identical
- [x] EXPLAIN ANALYZE shows batches + thread count
- [x] Empty join result: no crash, returns 0 rows correctly
- [x] NULL join keys: handled correctly (NULL != NULL)

**Implementation log:**
Implemented in `src/engine/executor/join.rs`. Batched nested loop, spatial join path with index scan on main thread, GPU three-layer residual evaluation.

### A3 — GpuAccelJoin: Hash Join Batched Probe
**Status:** Complete
**Owns:** `pg_accel/src/engine/executor/join.rs` (hash join portion, same file distinct section)

**Tasks:**
- [x] Implement batched hash join probe:
  - [x] Build side runs normally (PG builds hash table)
  - [x] Probe side: accumulate outer tuples in batch
  - [x] Probe PG's hash table on main thread (PG internal structure, not thread-safe)
  - [x] Collect all (outer, inner) matched pairs from probing
  - [x] Batch-evaluate residual conditions on matches:
    - GpuSpatial residuals: GPU three-layer pipeline
    - BatchedEval residuals: main thread, batched
- [x] Optimize for joins with expensive residual conditions beyond the equality key
- [x] Handle hash collision correctly (multiple matches per bucket)

**Agent gate:**
- [x] `a JOIN b ON a.key = b.key AND a.ts < b.ts AND complex_func(a.data, b.data)` -> identical to vanilla
- [x] Join with no matches: correct empty result
- [x] Join with all matches: correct full cross product
- [x] Hash collision handling: correct (multiple matches per bucket)

**Implementation log:**
Implemented in `src/engine/executor/join.rs` (hash join section). Build side runs normally, probe side batches, residual conditions dispatched via GPU or batched CPU.

### A4 — GpuAccelAgg
**Status:** Complete
**Owns:** `pg_accel/src/engine/executor/agg.rs`

**Tasks:**
- [x] Replace Agg node when transition functions are in our registry and rows > threshold
- [x] Call transition functions on main thread (BatchedEval)
- [x] Implement fused scan+filter+agg: when child is GpuAccelScan with selective filter, skip deserializing aggregate columns for filtered-out rows
- [x] Implement GPU reduce offload: for simple numeric aggregates (SUM/MIN/MAX/COUNT) on large datasets, offload to GPU reduce kernel (Phase 4 A9) if available
- [x] Implement predicate pushdown: evaluate WHERE before aggregation, reducing rows touching the transition function

**Agent gate:**
- [x] `GROUP BY dept, SUM(salary), AVG(salary), COUNT(*)` on 1M rows -> identical to vanilla
- [x] `SUM(val) FROM generate_series(1,10M)` -> identical (no float drift)
- [x] Combined scan+filter+agg: `SUM(val) WHERE selective_predicate` on 10M -> identical, faster

**Implementation log:**
Implemented in `src/engine/executor/agg.rs`. GROUP BY support via GroupKeyInfo, GPU reduce offload for SUM/AVG/MIN/MAX, hash-based grouping.

### A4b — GpuAccelSort
**Status:** Complete
**Owns:** `pg_accel/src/engine/executor/sort.rs`

**NOTE:** This agent assignment shares the A4 slot. In practice, assign to a 6th
agent or have A4 complete Agg first, then Sort, or split across Phase 5 and 7.

**Tasks:**
- [x] Replace Sort for > 100K tuples
- [x] For numeric sort keys: use GPU sort kernel (Phase 4 A8) when GPU available
- [x] For expression sort keys: extract sort keys in batch, then parallel sort
- [x] Implement top-k optimization for `ORDER BY ... LIMIT k` where k << N: partition across rayon threads, each finds local top-k, merge (O(N) vs O(N log N))

**Agent gate:**
- [x] `ORDER BY complex_expr DESC LIMIT 10` on 5M rows -> correct top-10, faster
- [x] `ORDER BY float_col` on 1M rows -> identical to vanilla PG
- [x] Window functions with GpuAccelSort child: correct results

**Implementation log:**
Implemented in `src/engine/executor/sort.rs`. GPU sort dispatch for numeric keys, top-k optimization, expression sort key extraction. Window functions in `src/engine/executor/window.rs` (ROW_NUMBER, RANK, DENSE_RANK, SUM, COUNT, LAG, LEAD).

---

## Phase Gate

- [x] GpuAccelScan: 10+ query patterns return identical results to vanilla PG
- [x] GpuAccelScan GiST recheck: PostGIS spatial index queries correct via batched GPU recheck
- [x] GpuAccelScan GiST recheck: h3-pg cell index queries correct via batched GPU h3 kernel
- [x] GpuAccelScan SP-GiST recheck: h3-pg SP-GiST index queries correct via batched GPU h3 kernel
- [x] GpuAccelJoin: nested loop + hash join both correct on 5+ test cases each
- [x] GpuAccelAgg: GROUP BY + SUM/AVG/COUNT correct on 1M rows
- [x] GpuAccelSort: ORDER BY correct, top-k correct
- [x] Combined scan+filter+agg path works and is faster for selective filters
- [x] LIMIT/OFFSET handled correctly by all nodes
- [x] NULL handling correct in all nodes
- [x] EXPLAIN ANALYZE instrumentation shows all fields for all node types
- [x] Thread budget acquired/released correctly in all nodes (counter verified)
- [x] No regressions for small queries (< min_batch_size uses vanilla PG path)
- [x] cargo pgrx test pg17 -- all executor tests pass
- [x] Docker integration: GpuAccelScan on spatial + analytic queries, results match vanilla
- [x] Docker integration: GiST/SP-GiST recheck queries on PostGIS + h3-pg, results match vanilla
- [x] Docker integration: JOIN + AGG + SORT nodes produce correct results on real data
- [x] Docker integration: all prior phase tests still pass (no regressions)
