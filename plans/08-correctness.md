# Phase 8: Correctness Gauntlet

**Depends on:** Phase 7 (everything wired end-to-end)
**Parallelism:** All 10 agents, each owns a class of edge cases

This phase exists solely to build confidence before production hardening.
Every agent is trying to BREAK pg_accel. If they can't break it, we ship it.

---

## Agent Assignments

### A0 — NULL Exhaustive
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/nulls.rs`

**Tasks:**
- [ ] Test every accelerated STRICT function with NULL in each argument position and verify NULL args produce NULL result without calling the function
- [ ] Test every accelerated non-STRICT function with NULL in each argument position and verify NULL args produce whatever PG returns (match exactly)
- [ ] Test NULL in geometry column of spatial join
- [ ] Test NULL in GROUP BY key for aggregates
- [ ] Test NULL in ORDER BY key for sort
- [ ] Cover 20 functions x 2-3 arg positions = 40+ test cases total

**Agent gate:**
- [ ] 40+ test cases pass
- [ ] All produce identical results to vanilla PG
- [ ] Zero crashes on any NULL combination

**Implementation log:**
_(no deviations)_

### A1 — Empty + Degenerate Geometries
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/degenerate.rs`

**Tasks:**
- [ ] Test empty POINT, LINESTRING, POLYGON, GEOMETRYCOLLECTION
- [ ] Test zero-area polygon (all vertices collinear)
- [ ] Test zero-length linestring (start = end)
- [ ] Test self-intersecting polygon (bowtie)
- [ ] Test 3D geometries (x,y,z) -- should work, z ignored by 2D predicates
- [ ] Test 4D geometries (x,y,z,m)
- [ ] Test multipart types: MULTIPOINT, MULTIPOLYGON, MULTILINESTRING
- [ ] Test GeometryCollection containing mixed types
- [ ] Test geometry with 10000+ vertices (stress ring buffer)
- [ ] Cover 15 functions x 10+ degenerate types = 150+ test cases total

**Agent gate:**
- [ ] 150+ test cases pass
- [ ] All produce identical results OR identical errors to vanilla PostGIS
- [ ] Zero crashes

**Implementation log:**
_(no deviations)_

### A2 — SRID Mismatch + Projection
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/srid.rs`

**Tasks:**
- [ ] Call `ST_Intersects(geom_4326, geom_3857)` and verify it raises the same error as vanilla PostGIS
- [ ] Call `ST_Transform(geom, 3857)` and verify result matches vanilla
- [ ] Call function with SRID=0 (undefined) and verify behavior matches vanilla
- [ ] Test geography with non-4326 SRID
- [ ] Cover 10+ SRID mismatch cases total

**Agent gate:**
- [ ] 10+ SRID mismatch cases: error messages match vanilla PostGIS exactly
- [ ] ST_Transform accelerated path matches vanilla within tolerance
- [ ] No silent wrong-SRID results

**Implementation log:**
_(no deviations)_

### A3 — Fuzz Testing
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/fuzz.rs`

**Tasks:**
- [ ] Implement random geometry generation with random vertex count (1-10000)
- [ ] Include random coordinates with edge cases: +/-180, +/-90, 0, very small, very large
- [ ] Include random SRIDs: 4326, 3857, 0, 32632
- [ ] Include random geometry types: point, line, polygon, multi-*
- [ ] For each random input, run accelerated function AND vanilla function and compare results
- [ ] Run 100K random inputs for ST_Intersects
- [ ] Run 100K random inputs for ST_Contains
- [ ] Run 100K random inputs for ST_DWithin
- [ ] Run 100K random inputs for ST_Distance (geography)
- [ ] Run 100K random inputs for h3_lat_lng_to_cell
- [ ] Log any disagreements with full reproduction case

**Agent gate:**
- [ ] 500K total random tests (100K x 5 functions)
- [ ] ZERO disagreements between accelerated and vanilla
- [ ] Test completes in < 30 minutes
- [ ] Disagreements (if any) logged with full reproduction case

**Implementation log:**
_(no deviations)_

### A4 — Concurrent Stress
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/concurrent.rs`

**Tasks:**
- [ ] Set up 16 connections running simultaneously
- [ ] Configure 4 connections running spatial joins (10K rows each)
- [ ] Configure 4 connections running aggregates (100K rows each)
- [ ] Configure 4 connections running hash joins with residuals
- [ ] Configure 4 connections running rapid short queries (< 100 rows, should use vanilla path)
- [ ] Verify all connections return correct results
- [ ] Verify no crashes, no deadlocks, no corrupted results
- [ ] Verify thread budget never exceeds `max_workers_total`
- [ ] Verify stats counters are consistent (no lost increments)
- [ ] Run for 100 iterations

**Agent gate:**
- [ ] 16 x 100 iterations = 1600 query executions, all correct
- [ ] Zero crashes
- [ ] Zero deadlocks (timeout = 60s per query, none hit)
- [ ] Thread budget counter never exceeds configured max
- [ ] After all connections close: thread budget counter = 0

**Implementation log:**
_(no deviations)_

### A5 — LIMIT / OFFSET / CTE / Subquery
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/plan_patterns.rs`

**Tasks:**
- [ ] Test `SELECT ... LIMIT 10` -- must not over-dispatch
- [ ] Test `SELECT ... OFFSET 100 LIMIT 10` -- correct skip + limit
- [ ] Test `WITH cte AS (SELECT ...) SELECT * FROM cte WHERE ...` -- CTE materialization
- [ ] Test subquery: `SELECT * FROM (SELECT ... WHERE ...) sub WHERE ...`
- [ ] Test `UNION ALL` of accelerated + non-accelerated queries
- [ ] Test `EXCEPT` / `INTERSECT`
- [ ] Test correlated subquery: `WHERE EXISTS (SELECT ... WHERE a.geom && b.geom)`
- [ ] Test `INSERT INTO ... SELECT ...` with accelerated source
- [ ] Test `CREATE TABLE AS SELECT ...` with accelerated query
- [ ] Cover all 10+ patterns total

**Agent gate:**
- [ ] All 10+ patterns: results identical to vanilla PG
- [ ] LIMIT: pg_accel_stats shows rows_dispatched approx batch_size (not full table)
- [ ] CTE materialization: correct even when CTE scanned multiple times

**Implementation log:**
_(no deviations)_

### A6 — Transaction Semantics
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/transactions.rs`

**Tasks:**
- [ ] Test rollback mid-query (cancel during spatial join)
- [ ] Test savepoints: SAVEPOINT -> accelerated query -> ROLLBACK TO SAVEPOINT
- [ ] Test serializable isolation: two transactions reading same data
- [ ] Test `ON CONFLICT DO UPDATE` with accelerated WHERE clause
- [ ] Test cursor: DECLARE -> FETCH 10 -> FETCH 10 -> CLOSE (batching respects cursor semantics)
- [ ] Test prepared statements: `PREPARE + EXECUTE` with different parameters
- [ ] Test WITH HOLD cursors across transaction boundaries
- [ ] Test PG parallel coexistence: query with both PG parallel workers AND pg_accel active (verify thread budget accounts for PG's workers correctly)

**Agent gate:**
- [ ] All transaction patterns behave identically to vanilla PG
- [ ] Rollback: clean, no leaked thread budget
- [ ] Serializable: no false serialization failures caused by our threading

**Implementation log:**
_(no deviations)_

### A7 — Memory Leak Test
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/memory.rs`

**Tasks:**
- [ ] Run 100K queries in a loop (mix of spatial, aggregate, sort)
- [ ] Monitor RSS via `/proc/self/status` or `mach_task_info` (macOS)
- [ ] Monitor rayon thread pool (thread count stable)
- [ ] Monitor GPU memory pool (`pgaccel_pool_bytes_used`)
- [ ] Monitor thread budget counter (returns to 0 after each query)

**Agent gate:**
- [ ] RSS after 100K queries: within 50MB of RSS after 100 queries
- [ ] Rayon pool: same thread count throughout
- [ ] GPU pool: bytes_used returns to baseline after pool_reset
- [ ] Thread budget: 0 at rest between queries
- [ ] No "out of memory" errors

**Implementation log:**
_(no deviations)_

### A8 — Extension Version Matrix
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/versions.rs`

**Tasks:**
- [ ] Test with PostGIS 3.3
- [ ] Test with PostGIS 3.4
- [ ] Test with PostGIS 3.5
- [ ] Test with h3-pg 4.0
- [ ] Test with h3-pg 4.1
- [ ] Verify `pg_proc` pattern matching finds correct functions in each version
- [ ] Verify function signatures that changed between versions are detected and handled
- [ ] Verify missing functions are silently skipped (logged at DEBUG)

**Agent gate:**
- [ ] All supported PostGIS versions: functions discovered correctly
- [ ] If a function signature changed: function skipped + logged (not crash)
- [ ] At least PostGIS 3.4 + 3.5 tested (most common)

**Implementation log:**
_(no deviations)_

### A9 — PG Version Matrix
**Status:** Not Started
**Owns:** `pg_accel/tests/correctness/pg_versions.rs`

**Tasks:**
- [ ] Run core test suite on PG 15
- [ ] Run core test suite on PG 16
- [ ] Run core test suite on PG 17
- [ ] Run core test suite on PG 18
- [ ] Verify Custom Scan registration works on each version
- [ ] Verify planner hooks work on each version
- [ ] Verify shared memory init works on each version
- [ ] Verify all executor nodes produce correct results on each version
- [ ] Verify EXPLAIN ANALYZE format is correct on each version
- [ ] Handle any version-specific behavior in compat.rs

**Agent gate:**
- [ ] All 4 PG versions: core tests pass
- [ ] Any version-specific behavior: handled in compat.rs
- [ ] All versions within 15% performance of each other (or difference explained)

**Implementation log:**
_(no deviations)_

### A9b — PostGIS + h3-pg Upstream Test Suites
**Status:** Not Started
**Owns:** `pg_accel/tests/upstream/`

**Tasks:**
- [ ] Start PG with `shared_preload_libraries = 'pg_accel'`
- [ ] `CREATE EXTENSION pg_accel;` in the test database
- [ ] Run PostGIS `make check` from PostGIS build tree (~138 core regression tests, ~82 topology, ~86 raster, ~20 loader/dumper)
- [ ] Run PostGIS `make garden` separately (~80K generated SQL statements, crash detection, no diff)
- [ ] Triage any PostGIS test failures: classify each as "plan-shape difference" (Custom Scan changes plan but result is correct) or "actual bug" (wrong result)
- [ ] Handle expected false failures: tests with `EXPLAIN` output in expected results, tests depending on specific plan choices, tests assuming specific row ordering without `ORDER BY`
- [ ] Run h3-pg regression tests via standard `pg_regress` (~20 tests) with pg_accel loaded
- [ ] Verify h3-pg tests covering both GiST and SP-GiST operator classes pass (batched recheck must handle both index types correctly)

**Agent gate:**
- [ ] PostGIS `make garden` with pg_accel loaded: zero crashes across 80K+ SQL statements
- [ ] PostGIS `make check`: all failures triaged; zero actual correctness bugs
- [ ] Plan-shape differences documented (expected output files updated or skip-listed)
- [ ] h3-pg regression tests: all pass with pg_accel loaded
- [ ] No new errors in PG log (no PANIC, no FATAL, no unexpected WARNING)

**Implementation log:**
_(no deviations)_

---

## Phase Gate

- [ ] 40+ NULL test cases pass
- [ ] 150+ degenerate geometry test cases pass
- [ ] 500K fuzz tests: zero disagreements
- [ ] 1600 concurrent query executions: zero crashes, zero wrong results
- [ ] Thread budget never exceeded under concurrency
- [ ] LIMIT/OFFSET/CTE/subquery/prepared statements: all patterns correct
- [ ] Transaction semantics preserved (including cursors and savepoints)
- [ ] PG parallel coexistence: correct when both PG workers and rayon active
- [ ] Memory: stable after 100K queries (< 50MB growth)
- [ ] PostGIS 3.4 + 3.5 tested
- [ ] PostGIS test suite: zero crashes (garden), zero correctness bugs (make check)
- [ ] h3-pg test suite: all pass with pg_accel loaded (GiST + SP-GiST operator classes)
- [ ] PG 15, 16, 17, 18 all pass
- [ ] Total test case count: 1000+
- [ ] Docker integration: full test suite passes on real PG (all phases cumulative)
- [ ] Docker integration: 24-hour soak test subset (1 hour) on Docker -- zero crashes
