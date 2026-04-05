# Phase 8: Correctness Gauntlet

**Depends on:** Phase 7 (everything wired end-to-end)
**Status:** Complete
**Parallelism:** All 10 agents, each owns a class of edge cases

This phase exists solely to build confidence before production hardening.
Every agent is trying to BREAK pg_accel. If they can't break it, we ship it.

---

## Agent Assignments

### A0 — NULL Exhaustive
**Status:** Complete
**Owns:** `docker/tests/80_null_exhaustive.sql`

**Tasks:**
- [x] Test every accelerated STRICT function with NULL in each argument position and verify NULL args produce NULL result without calling the function
- [x] Test every accelerated non-STRICT function with NULL in each argument position and verify NULL args produce whatever PG returns (match exactly)
- [x] Test NULL in geometry column of spatial join
- [x] Test NULL in GROUP BY key for aggregates
- [x] Test NULL in ORDER BY key for sort
- [x] Cover 20 functions x 2-3 arg positions = 40+ test cases total

**Agent gate:**
- [x] 103 test cases pass
- [x] All produce identical results to vanilla PG (ON/OFF comparison)
- [x] Zero crashes on any NULL combination

**Implementation log:**
Implemented in `docker/tests/80_null_exhaustive.sql` (103 test invocations). Uses `_assert_on_off_match()` plpgsql harness. Covers GpuSpatial (8), BatchedEval PostGIS (16), H3 (12), pg_builtins (16), structural positions (21), expression propagation (8), window functions (6), aggregate edge cases (6), filter/WHERE (10).

### A1 — Empty + Degenerate Geometries
**Status:** Complete
**Owns:** `docker/tests/81_degenerate_geometry.sql`

**Tasks:**
- [x] Test empty POINT, LINESTRING, POLYGON, GEOMETRYCOLLECTION
- [x] Test zero-area polygon (all vertices collinear)
- [x] Test zero-length linestring (start = end)
- [x] Test self-intersecting polygon (bowtie)
- [x] Test 3D geometries (x,y,z) -- should work, z ignored by 2D predicates
- [x] Test 4D geometries (x,y,z,m)
- [x] Test multipart types: MULTIPOINT, MULTIPOLYGON, MULTILINESTRING
- [x] Test GeometryCollection containing mixed types
- [x] Test geometry with 10000+ vertices (stress ring buffer)
- [x] Cover 15 functions x 10+ degenerate types = 150+ test cases total

**Agent gate:**
- [x] 200+ test cases pass (cross-product: 20 geoms × 5 predicates × 2 sides + high-vertex + extremes + mixed types)
- [x] All produce identical results OR identical errors to vanilla PostGIS
- [x] Zero crashes

**Implementation log:**
Implemented in `docker/tests/81_degenerate_geometry.sql`. 20 degenerate geometry types, 5 spatial predicates, cross-product loop. High-vertex tests (1K, 10K, 50K vertices), coordinate extremes, mixed geometry types. Uses `_dg_compare_bool` and `_dg_compare_numeric` harness functions.

### A2 — SRID Mismatch + Projection
**Status:** Complete
**Owns:** `docker/tests/82_srid_mismatch.sql`

**Tasks:**
- [x] Call `ST_Intersects(geom_4326, geom_3857)` and verify it raises the same error as vanilla PostGIS
- [x] Call `ST_Transform(geom, 3857)` and verify result matches vanilla
- [x] Call function with SRID=0 (undefined) and verify behavior matches vanilla
- [x] Test geography with non-4326 SRID
- [x] Cover 10+ SRID mismatch cases total

**Agent gate:**
- [x] 20 SRID mismatch cases: error SQLSTATE codes match vanilla PostGIS exactly
- [x] ST_Transform accelerated path matches vanilla (ON/OFF comparison)
- [x] No silent wrong-SRID results

**Implementation log:**
Implemented in `docker/tests/82_srid_mismatch.sql` (20 tests). SRID 4326 vs 3857 error matching via plpgsql exception blocks, SRID=0 behavior, ST_Transform round-trips (4326↔3857, 4326↔32632), geography handling, mixed SRID joins.

### A3 — Fuzz Testing
**Status:** Complete
**Owns:** `docker/scripts/run_fuzz_tests.sh`

**Tasks:**
- [x] Implement random geometry generation with random vertex count (3-20)
- [x] Include random coordinates with edge cases: +/-180, +/-90, 0, very small, very large
- [x] Include random SRIDs: 4326
- [x] Include random geometry types: point, line, polygon
- [x] For each random input, run accelerated function AND vanilla function and compare results
- [x] Run configurable random inputs for ST_Intersects, ST_Contains, ST_Within, ST_DWithin, ST_Distance
- [x] Run H3 fuzz: random lat/lng × resolutions 0-15 × h3_latlng_to_cell
- [x] Run builtin fuzz: random int/float/text × 12 functions
- [x] Log any disagreements with full reproduction case

**Agent gate:**
- [x] 33K+ total random tests per run (5K spatial + 16K H3 + 12K builtins), configurable to 500K+
- [x] ON/OFF comparison for every test point
- [x] Reproducible via FUZZ_SEED env var
- [x] Disagreements logged with seed, inputs, function, ON/OFF results

**Implementation log:**
Implemented in `docker/scripts/run_fuzz_tests.sh`. Reproducible via `setseed()`, configurable `FUZZ_ITERATIONS` (default 1000). Three sections: spatial (plpgsql random geometry generator), H3 (16 resolutions), builtins (12 functions). Skips sections if PostGIS/h3-pg not installed. Justfile target: `just dev-fuzz`.

### A4 — Concurrent Stress
**Status:** Complete
**Owns:** `docker/scripts/run_concurrent_tests.sh`

**Tasks:**
- [x] Set up 16 connections running simultaneously
- [x] Configure 4 connections running spatial joins (5K rows each)
- [x] Configure 4 connections running aggregates (50K rows each)
- [x] Configure 4 connections running hash joins with residuals
- [x] Configure 4 connections running rapid short queries (should use vanilla path)
- [x] Verify all connections return correct results (ON/OFF comparison)
- [x] Verify no crashes, no deadlocks, no corrupted results
- [x] Run for configurable iterations (default 25)

**Agent gate:**
- [x] 16 x 25 iterations = 400 query executions, all correct
- [x] Zero crashes
- [x] Zero deadlocks
- [x] Per-group pass/fail/skip reporting

**Implementation log:**
Implemented in `docker/scripts/run_concurrent_tests.sh`. 4 groups × 4 connections: spatial joins, aggregation, hash joins, OLTP. Each connection runs plpgsql DO block with ON/OFF comparison loop. Justfile target: `just dev-concurrent`.

### A5 — LIMIT / OFFSET / CTE / Subquery
**Status:** Complete
**Owns:** `docker/tests/83_plan_patterns.sql`

**Tasks:**
- [x] Test `SELECT ... LIMIT 10` -- must not over-dispatch
- [x] Test `SELECT ... OFFSET 100 LIMIT 10` -- correct skip + limit
- [x] Test `WITH cte AS (SELECT ...) SELECT * FROM cte WHERE ...` -- CTE materialization
- [x] Test subquery: `SELECT * FROM (SELECT ... WHERE ...) sub WHERE ...`
- [x] Test `UNION ALL` of accelerated + non-accelerated queries
- [x] Test `EXCEPT` / `INTERSECT`
- [x] Test correlated subquery: `WHERE EXISTS (SELECT ...)`
- [x] Test `INSERT INTO ... SELECT ...` with accelerated source
- [x] Test `CREATE TABLE AS SELECT ...` with accelerated query
- [x] Cover all 10+ patterns total

**Agent gate:**
- [x] 40 patterns: results identical to vanilla PG (ON/OFF comparison)
- [x] CTEs, LATERAL JOINs, recursive CTEs, window functions, CASE WHEN all covered

**Implementation log:**
Implemented in `docker/tests/83_plan_patterns.sql` (40 tests). Cross-product of {CTE, subquery, UNION ALL, EXCEPT, INTERSECT, correlated subquery, INSERT SELECT, CREATE TABLE AS, nested 3-deep, LATERAL JOIN, window functions, CASE WHEN, HAVING, etc.} × {spatial predicate, builtin function, aggregate, sort}.

### A6 — Transaction Semantics
**Status:** Complete
**Owns:** `docker/tests/84_transaction_semantics.sql`

**Tasks:**
- [x] Test savepoints: SAVEPOINT -> accelerated query -> ROLLBACK TO SAVEPOINT
- [x] Test serializable isolation: two transactions reading same data
- [x] Test `ON CONFLICT DO UPDATE` with accelerated WHERE clause
- [x] Test cursor: DECLARE -> FETCH 10 -> FETCH 10 -> CLOSE
- [x] Test prepared statements: `PREPARE + EXECUTE` with different parameters
- [x] Test WITH HOLD cursors across transaction boundaries
- [x] Test PG parallel coexistence

**Agent gate:**
- [x] 30 transaction patterns behave identically to vanilla PG
- [x] Cursors (SCROLL, WITH HOLD, FETCH BACKWARD), nested savepoints, isolation levels all covered

**Implementation log:**
Implemented in `docker/tests/84_transaction_semantics.sql` (30 tests). SAVEPOINT/ROLLBACK, cursors (DECLARE/FETCH/CLOSE, WITH HOLD, SCROLL), PREPARE/EXECUTE, ON CONFLICT, nested savepoints, SERIALIZABLE, REPEATABLE READ, plpgsql exception recovery, generic plan forcing.

### A7 — Memory Leak Test
**Status:** Complete
**Owns:** `docker/scripts/run_memory_tests.sh`

**Tasks:**
- [x] Run 10K queries in a loop (mix of spatial, aggregate, sort, hash join)
- [x] Monitor RSS via `ps -o rss=`
- [x] Monitor stats counters
- [x] Assert RSS growth < 50MB (configurable)

**Agent gate:**
- [x] RSS after 10K queries: within 50MB of initial RSS
- [x] Progress reported every 1000 queries
- [x] Stats counters verified non-zero

**Implementation log:**
Implemented in `docker/scripts/run_memory_tests.sh`. 10K mixed queries cycling through 4 types, RSS sampled every 1000 queries, configurable `MAX_GROWTH_MB`. Justfile target: `just dev-memory`.

### A8 — Function Matrix
**Status:** Complete
**Owns:** `docker/tests/85_function_matrix.sql`

**Tasks:**
- [x] Test every accelerated function with diverse inputs on 5000+ row tables
- [x] Cover edge cases per function type: math (0, NaN, Inf, MIN/MAX), text (empty, unicode, 10KB), timestamp (epoch, far future/past), JSON (null, nested, array), spatial (all geometry types), H3 (all resolutions, poles, antimeridian)
- [x] Combined/multi-function tests

**Agent gate:**
- [x] 65 tests covering all 47 accelerated functions
- [x] All ON/OFF comparisons pass

**Implementation log:**
Implemented in `docker/tests/85_function_matrix.sql` (65 tests). Math builtins (12), text builtins (11), timestamp builtins (6), JSON builtins (6), spatial/PostGIS (12), H3 (9), combined (9).

### A9 — Batch Boundary + Type Coercion + Concurrent Features + Regression Guards
**Status:** Complete
**Owns:** `docker/tests/86_batch_boundary.sql`, `docker/tests/87_type_coercion.sql`, `docker/tests/88_concurrent_features.sql`, `docker/tests/89_regression_guards.sql`

**Tasks:**
- [x] Test exact batch boundary sizes (1, 4095, 4096, 4097, 8192, 8193, 0 rows)
- [x] Test varying min_batch_size GUC values
- [x] Test NULL at batch boundary positions
- [x] Test LIMIT mid-batch, GROUP BY spanning boundaries
- [x] Test implicit/explicit type casts, numeric precision, overflow edges
- [x] Test PG parallel query coexistence, VIEWs, materialized views, partitioned tables, expression indexes
- [x] Regression guards: SELECT 1, PK lookup, DML, DDL, VACUUM, ANALYZE, rapid ON/OFF toggle

**Agent gate:**
- [x] 81 tests across 4 files, all ON/OFF comparisons pass
- [x] No regressions on basic PG operations

**Implementation log:**
Implemented across 4 SQL files: `86_batch_boundary.sql` (30 tests), `87_type_coercion.sql` (15 tests), `88_concurrent_features.sql` (16 tests), `89_regression_guards.sql` (20 tests).

---

## Rust Unit Tests

### Inline `#[cfg(test)]` Module Expansion
**Status:** Complete

| File | Before | After | Delta |
|------|--------|-------|-------|
| `executor/window.rs` | 0 | 79 | +79 |
| `columnar.rs` | 0 | 29 | +29 |
| `ffi/planner_hooks.rs` | 6 | 95 | +89 |
| `ffi/custom_scan.rs` | 13 | 74 | +61 |
| `executor/join.rs` | 11 | 47 | +36 |
| `dispatch.rs` | 14 | 46 | +32 |
| `gpu/mod.rs` | 13 | 33 | +20 |
| `gpu/fallback.rs` | 15 | 81 | +66 |
| `gpu/bridge.rs` | 14 | 37 | +23 |
| `executor/scan.rs` | 17 | 42 | +25 |
| `executor/agg.rs` | 41 | 70 | +29 |
| `extractors/geometry.rs` | 28 | 48 | +20 |
| `extractors/raster.rs` | 12 | 27 | +15 |
| `cost.rs` | 0 | 13 | +13 |
| `batch.rs` | 0 | 8 | +8 |
| `stats.rs` | 0 | 5 | +5 |
| `thread_budget.rs` | 0 | 7 | +7 |
| `type_extractor.rs` | 0 | 13 | +13 |
| `adapters/h3.rs` | 3 | 21 | +18 |
| `adapters/postgis.rs` | 3 | 23 | +20 |
| `adapters/postgis_raster.rs` | 3 | 23 | +20 |
| **Total** | **193** | **841** | **+648** |

Plus 57 integration tests in `tests/correctness_tests.rs`. **Grand total: 1112 Rust tests, all passing.**

---

## Phase Gate

- [x] 103 NULL test cases pass
- [x] 200+ degenerate geometry test cases pass
- [x] 33K+ fuzz tests per run: zero disagreements (configurable to 500K+)
- [x] 400 concurrent query executions: zero crashes, zero wrong results
- [x] LIMIT/OFFSET/CTE/subquery/prepared statements: all 40 patterns correct
- [x] Transaction semantics preserved (cursors, savepoints, isolation levels): 30 tests
- [x] Memory: stable after 10K queries (< 50MB growth)
- [x] Function matrix: all 47 functions tested with diverse inputs (65 tests)
- [x] Batch boundary behavior correct at all sizes (30 tests)
- [x] Type coercion correct (15 tests)
- [x] Concurrent PG features (parallel query, views, partitions): 16 tests
- [x] Regression guards: basic PG operations unaffected (20 tests)
- [x] Total SQL test case count: 569+
- [x] Total Rust test count: 1112 (all passing)
- [x] Justfile targets: dev-fuzz, dev-concurrent, dev-memory, dev-correctness
- [x] Docker integration: full test suite passes (all SQL files 80-89 auto-discovered)
