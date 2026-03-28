# Phase 7: Integration (Adapters + GPU Wiring)

**Depends on:** Phase 5 (executor nodes) + Phase 6 (GPU kernels)
**Parallelism:** All 10 agents

This phase wires everything together. Adapters connect PG extension functions to
our dispatch engine. The three-layer GPU pipeline connects to executor nodes.
After this phase, unmodified SQL queries are automatically accelerated.

---

## Agent Assignments

### A0 — Geometry Extractor (PostGIS)
**Status:** Not Started
**Owns:** `pg_accel/src/adapters/extractors/geometry.rs`

**Tasks:**
- [ ] Read PostGIS GSERIALIZED format
- [ ] Implement fixed-offset POINT read in pure Rust (no liblwgeom dependency for simple types)
- [ ] For complex types: extract bbox from GSERIALIZED header (HasBBox flag → BOX2DF at known offset)
- [ ] Implement `extract_bbox()` → (xmin, ymin, xmax, ymax) as f32 (PostGIS stores BOX2DF as float32)
- [ ] Implement `extract_point()` → (x, y) as f64 for CPU path, f32 for GPU path
- [ ] For complex geometries: extract vertex array as flat f32 buffer for GPU kernels
- [ ] Handle NULL geometry (return None)
- [ ] Handle empty geometry (return None, not crash)

**Agent gate:**
- [ ] Extract POINT datum → (x,y) → pack back → `ST_Equals(original, packed)` is true
- [ ] Extract bbox of POLYGON → matches `Box2D(geom)::box2d` from PostGIS
- [ ] 1000 random geometries: all extract without crash
- [ ] NULL geometry: handled (returns None)
- [ ] Empty geometry: handled (returns None, not crash)

**Implementation log:**
_(no deviations)_

### A1 — Geography Extractor (PostGIS)
**Status:** Not Started
**Owns:** `pg_accel/src/adapters/extractors/geography.rs`

**Tasks:**
- [ ] Read same GSERIALIZED format as geometry but with different type OID (geography uses spherical coordinates and spherical bounding box)
- [ ] Reuse geometry extractor logic with geography OID registration
- [ ] Implement spherical bbox extraction

**Agent gate:**
- [ ] Extract geography POINT → (lon, lat) correct
- [ ] Geography bbox extraction matches `ST_Envelope(geog::geometry)` bounds
- [ ] All geography types handled (or gracefully return None for unsupported)

**Implementation log:**
_(no deviations)_

### A2 — PostGIS Adapter (Complete)
**Status:** Not Started
**Owns:** `pg_accel/src/adapters/postgis.rs`

**Tasks:**
- [ ] Complete the adapter with all function entries, each specifying function pattern (name, arg types) and strategy (`GpuSpatial` or `BatchedEval`)
- [ ] Register functions with `GpuSpatial` strategy (GPU fast-path + CPU recheck):
  - `ST_Intersects(geometry, geometry)`
  - `ST_Contains(geometry, geometry)`
  - `ST_Within(geometry, geometry)`
  - `ST_DWithin(geometry, geometry, float8)`
  - `ST_Distance(geography, geography)`
- [ ] Register functions with `BatchedEval` strategy (main-thread, benefits from late materialization + predicate reordering in Custom Scan node):
  - `ST_Buffer`, `ST_Transform`, `ST_Area`, `ST_Centroid`, `ST_Length`
  - `ST_AsMVTGeom`, `ST_Union`, `ST_Simplify`
  - `ST_Crosses`, `ST_Overlaps`, `ST_Touches`
  - `ST_X`, `ST_Y`, `ST_SRID`, `ST_GeometryType`

**Agent gate:**
- [ ] `#[pg_test]` per GpuSpatial function: 1000-row table, ON == OFF bit-identical results
- [ ] `#[pg_test]` per BatchedEval function: 1000-row table, ON == OFF
- [ ] Total: 20+ functions tested, all matching vanilla PostGIS
- [ ] GPU path used for GpuSpatial functions (verify via pg_accel_stats gpu_rows_processed > 0)

**Implementation log:**
_(no deviations)_

### A3 — h3-pg Adapter (Complete)
**Status:** Not Started
**Owns:** `pg_accel/src/adapters/h3.rs`

**Tasks:**
- [ ] Register functions with `GpuH3` strategy (GPU kernel -- pure integer/trig math):
  - `h3_lat_lng_to_cell(point, int)` → GPU bulk conversion (the killer workload -- millions of points indexed in one kernel launch)
  - `h3_grid_distance(h3index, h3index)` → GPU bulk pairwise distance (integer math)
  - `h3_cell_to_parent(h3index, int)` → GPU bulk (bit shift, nearly free)
  - `h3_get_resolution(h3index)` → GPU bulk (bit mask, nearly free)
- [ ] Register functions with `BatchedEval` strategy (return complex types requiring palloc):
  - `h3_cell_to_lat_lng` → returns point (palloc)
  - `h3_cell_to_boundary` → returns polygon geometry (palloc)
  - `h3_grid_disk` → returns array of cells (palloc)
  - `h3_compact_cells` → returns array (palloc)
  - `h3_cells_to_multi_polygon` → returns geometry (palloc)
- [ ] Implement GiST / SP-GiST index recheck acceleration: h3-pg provides both GiST and SP-GiST operator classes for `h3index`; SP-GiST leverages h3's hierarchical cell structure -- parent/child relationships map naturally to SP-GiST's space-partitioning tree, giving tighter candidate sets for hierarchy queries
- [ ] Support queries like `WHERE cell @> h3_lat_lng_to_cell(point, 7)` (cell containment) and `WHERE cell && h3_grid_disk(center, 3)` (cell overlap with k-ring)
- [ ] Batch the recheck via GPU h3 kernels in our Custom Scan: GiST/SP-GiST traversal runs on main thread, but exact containment/overlap checking on accumulated candidates runs on GPU; the recheck path is identical regardless of which index type produced the candidates

**Agent gate:**
- [ ] `#[pg_test]` per function: 1000 rows, ON == OFF
- [ ] GPU: `h3_lat_lng_to_cell` on 1M random points → identical to sequential, >=5x faster
- [ ] GPU: `h3_grid_distance` on 100K pairs → identical to sequential
- [ ] GiST recheck: `SELECT * FROM h3_data WHERE cell @> target` with GiST index → identical to vanilla
- [ ] SP-GiST recheck: `SELECT * FROM h3_data WHERE cell @> target` with SP-GiST index → identical to vanilla
- [ ] GiST/SP-GiST recheck: batched recheck visible in EXPLAIN ANALYZE stats
- [ ] NULL handling: `h3_lat_lng_to_cell(NULL, 7)` → NULL
- [ ] High resolution (res 15) on Metal: invalid cells fall back to CPU, correct results

**Implementation log:**
_(no deviations)_

### A3b — PostGIS Raster Adapter
**Status:** Not Started
**Owns:** `pg_accel/src/adapters/postgis_raster.rs`, `pg_accel/src/adapters/extractors/raster.rs`

**Tasks:**
- [ ] Implement raster type extractor: read PostGIS raster serialization format in Rust
- [ ] Parse header: endianness, version, nBands, scaleX/Y, ipX/Y, width, height, srid
- [ ] Parse band header: pixtype, nodata value, isOffline flag
- [ ] Parse pixel data: flat array of width x height values (int8/16/32, float32/64)
- [ ] Extract to flat pixel arrays for GPU kernels (only parse the binary format, not link `libraster`)
- [ ] Register functions with GPU strategy (`GpuRaster`):
  - `ST_MapAlgebra(raster, text)` → GPU map algebra kernel
  - `ST_MapAlgebra(raster, raster, text)` → GPU two-raster map algebra
  - `ST_Clip(raster, geometry)` → GPU clip kernel (reuses point_in_ring)
  - `ST_Reclass(raster, reclassarg)` → GPU reclass kernel
- [ ] Register functions with `BatchedEval` strategy:
  - `ST_Value(raster, geometry)` -- single-pixel lookup, cheap
  - `ST_Union(raster, raster)` -- complex merge, palloc-heavy
  - `ST_Resample(raster, ...)` -- resampling touches libraster
  - `ST_SummaryStats(raster)` -- could use GpuReduce in future

**Agent gate:**
- [ ] Raster type extractor: round-trip 100 rasters (extract pixels → repack → ST_SameAlignment check)
- [ ] `ST_MapAlgebra(rast, '[rast] * 2')` on 1024x1024 raster: ON == OFF pixel-identical
- [ ] `ST_MapAlgebra(rast1, rast2, '[rast1] + [rast2]')`: ON == OFF pixel-identical
- [ ] `ST_Clip(rast, polygon)`: ON == OFF pixel-identical
- [ ] `ST_Reclass`: ON == OFF
- [ ] NODATA pixels: correctly propagated (not computed)
- [ ] NULL raster input: returns NULL without crash
- [ ] GPU stats: `pg_accel_stats()` shows gpu_rows_processed > 0 for raster ops (count pixels as "rows" for stats purposes)

**Implementation log:**
_(no deviations)_

### A4 — pg_builtins Adapter (Complete)
**Status:** Not Started
**Owns:** `pg_accel/src/adapters/pg_builtins.rs`

**Tasks:**
- [ ] Register all functions with `BatchedEval` strategy (main thread, Custom Scan batching):
  - Math: `abs(int4)`, `abs(int8)`, `abs(float8)`, `sqrt(float8)`, `log(float8)`
  - Aggregate transitions: `int8_sum`, `float8_accum`, `int4_sum`
  - Text: `length(text)`, `lower(text)`, `upper(text)`, `btrim(text)`
  - Timestamp: `date_part`, `age`, `date_trunc`
  - JSON: `jsonb_extract_path_text`, `jsonb_typeof`
  - Network: `inet_contains`, `inet_subnet`
- [ ] These benefit from the Custom Scan node's late materialization and predicate reordering, not from parallelizing the function calls themselves

**Agent gate:**
- [ ] 10+ functions tested: ON == OFF on 10K rows each
- [ ] Aggregate path: `SELECT SUM(val) FROM generate_series(1,100000)::float8` → identical
- [ ] Text functions: correct with UTF-8, empty strings, NULLs

**Implementation log:**
_(no deviations)_

### A5 — Three-Layer Wiring (Rust Side)
**Status:** Not Started
**Owns:** `pg_accel/src/gpu/three_layer.rs`

**Tasks:**
- [ ] Connect executor nodes to GPU kernel library for GpuSpatial functions
- [ ] Implement the spatial predicate execution function:
  ```rust
  pub fn execute_spatial_predicate(
      geoms_a: &[ExtractedGeometry],
      geoms_b: &[ExtractedGeometry],
      predicate: SpatialPredicate,
      thread_pool: &ThreadPool,
  ) -> Vec<PredicateResult> {
      // 1. Extract bboxes to flat f32 arrays
      // 2. Call pgaccel_spatial_intersects (GPU layers 1+2)
      // 3. For UNCERTAIN pairs: dispatch to CPU via rayon (layer 3)
      //    Call the real PostGIS function via fmgr_info
      // 4. Merge results
  }
  ```
- [ ] Layer 3 CPU recheck uses the existing batch dispatch engine from Phase 2 -- same fmgr_info resolution, same rayon pool, same thread budget
- [ ] Implement automatic fallback to CPU-only rayon path when no GPU is available

**Agent gate:**
- [ ] Spatial join 10K x 1K with GPU: result identical to vanilla PostGIS
- [ ] `pg_accel_stats()` shows gpu_rows_processed > 0 and gpu_uncertain_count > 0
- [ ] GPU path disabled (`gpu_enabled = off`): still correct via CPU-only rayon path
- [ ] No GPU available: automatic fallback to CPU path, correct results

**Implementation log:**
_(no deviations)_

### A6 — End-to-End Query Tests
**Status:** Not Started
**Owns:** `pg_accel/tests/e2e/`

**Tasks:**
- [ ] Implement full SQL query tests -- the queries a real user would run
- [ ] Test spatial join:
  ```sql
  SELECT a.name, b.name FROM points a, polygons b
  WHERE ST_Contains(b.geom, a.geom);
  ```
- [ ] Test proximity search:
  ```sql
  SELECT * FROM restaurants
  WHERE ST_DWithin(location, ST_MakePoint(-73.98, 40.75)::geography, 500);
  ```
- [ ] Test H3 bulk indexing (GPU kernel):
  ```sql
  SELECT h3_lat_lng_to_cell(point, 7) as cell, COUNT(*)
  FROM events GROUP BY cell ORDER BY count DESC LIMIT 10;
  ```
- [ ] Test H3 GiST index containment (batched recheck via GPU h3 kernel):
  ```sql
  SELECT * FROM h3_data WHERE cell @> h3_lat_lng_to_cell(ST_MakePoint(-73.98, 40.75), 7);
  ```
- [ ] Test H3 SP-GiST index containment (batched recheck via GPU h3 kernel):
  ```sql
  SELECT * FROM h3_spgist_data WHERE cell @> h3_lat_lng_to_cell(ST_MakePoint(-73.98, 40.75), 7);
  ```
- [ ] Test H3 GiST k-ring overlap:
  ```sql
  SELECT * FROM h3_data WHERE cell && h3_grid_disk(h3_lat_lng_to_cell(ST_MakePoint(-73.98, 40.75), 7), 3);
  ```
- [ ] Test analytical aggregate:
  ```sql
  SELECT dept, COUNT(*), AVG(salary), MAX(bonus)
  FROM employees WHERE hire_date > '2020-01-01' GROUP BY dept;
  ```
- [ ] Test complex join:
  ```sql
  SELECT a.*, b.* FROM events a JOIN events b
  ON a.session_id = b.session_id AND a.ts < b.ts
  AND b.ts - a.ts < interval '1 hour';
  ```
- [ ] Test raster map algebra (NDVI calculation from 2-band imagery):
  ```sql
  SELECT ST_MapAlgebra(nir_band, red_band,
    '([rast1] - [rast2]) / ([rast1] + [rast2])')
  FROM satellite_tiles WHERE tile_id = 42;
  ```
- [ ] Test raster clip to area of interest:
  ```sql
  SELECT ST_Clip(rast, aoi.geom)
  FROM elevation_tiles, areas_of_interest aoi
  WHERE ST_Intersects(rast::geometry, aoi.geom);
  ```
- [ ] Test raster reclass (elevation to slope categories):
  ```sql
  SELECT ST_Reclass(rast, 1, '0-500:1, 500-1500:2, 1500-9000:3', '8BUI')
  FROM elevation_tiles;
  ```
- [ ] Run every query with ON and OFF, compare results

**Agent gate:**
- [ ] 28+ real-world query patterns tested (including 3+ raster, 3+ h3 index queries)
- [ ] All return identical results (ON == OFF)
- [ ] At least 5 queries use GpuAccelScan (verify via EXPLAIN)
- [ ] At least 2 queries use GpuAccelJoin
- [ ] At least 2 queries use GpuAccelAgg

**Implementation log:**
_(no deviations)_

### A7 — Cost Model Tuning
**Status:** Not Started
**Owns:** `pg_accel/src/engine/cost.rs` (updates)

**Tasks:**
- [ ] Run 20+ query patterns with EXPLAIN ANALYZE now that all nodes work
- [ ] Compare estimated cost vs actual execution time
- [ ] Adjust thresholds: min_batch_size, GPU break-even, parallelism factor
- [ ] Answer key question: at what row count does each node type become beneficial? Build a table: node_type x row_count → should_use?
- [ ] Ensure PG's own parallel plan wins when it should (don't inject our node when PG parallel is already optimal for simple cases)

**Agent gate:**
- [ ] Cost model correctly predicts "use GpuAccelScan" for 10K+ rows with expensive predicate
- [ ] Cost model correctly predicts "don't use" for 100 rows
- [ ] Cost model correctly predicts "don't use" for simple `int_col > 100` (PG parallel sufficient)
- [ ] No query regresses more than 10% vs vanilla PG

**Implementation log:**
_(no deviations)_

### A8 — Benchmark Workloads (Complete)
**Status:** Not Started
**Owns:** `pg_accel_bench/src/workloads/`

**Tasks:**
- [ ] Complete all 15 workloads with real data generators and three-way comparison (PG single-thread, PG parallel with 4 workers, pg_accel):
  1. `spatial_join` -- point x polygon ST_Contains
  2. `proximity` -- ST_DWithin radius search
  3. `h3_bulk` -- h3_lat_lng_to_cell on 1M points
  4. `aggregate` -- GROUP BY + SUM/AVG/COUNT with selective WHERE
  5. `index_recheck` -- GiST index scan with recheck
  6. `join_residual` -- hash join with timestamp residual
  7. `topk_sort` -- ORDER BY expression LIMIT
  8. `fts_rank` -- full-text search + ts_rank
  9. `jsonb_filter` -- JSONB path queries
  10. `range_overlap` -- range type overlap join
  11. `network_query` -- inet containment
  12. `bulk_transform` -- ST_Transform on large table
  13. `raster_map_algebra` -- ST_MapAlgebra NDVI on 1024x1024 tiles
  14. `raster_clip` -- ST_Clip raster tiles to polygon AOI
  15. `raster_reclass` -- ST_Reclass elevation to categories

**Agent gate:**
- [ ] All 15 workloads run without error
- [ ] Deterministic data: same seed → same results across runs
- [ ] Three-way comparison produces timing table
- [ ] p-values computed (two-sample t-test, 5+ iterations)

**Implementation log:**
_(no deviations)_

### A9 — Initial Benchmarks
**Status:** Not Started
**Owns:** benchmark results

**Tasks:**
- [ ] Run all 15 workloads on Apple Silicon (M-series Mac)
- [ ] Profile with Instruments to identify bottlenecks
- [ ] Identify which functions dominate, batch overhead, rayon contention, GPU transfer cost
- [ ] Document where pg_accel wins, where PG parallel wins, where it's a wash

**Agent gate:**
- [ ] >=2x vs PG parallel on at least 4 of 12 workloads
- [ ] Spatial join with GPU on CUDA (fp64): target >=5x vs PG parallel
- [ ] Spatial join with GPU on Metal (fp32): target >=3x vs PG parallel (more CPU rechecks)
- [ ] No workload regresses > 10% vs PG parallel
- [ ] Results reproducible (< 15% variance across 5 runs)
- [ ] Bottleneck analysis documented

**Implementation log:**
_(no deviations)_

---

## Phase Gate

- [ ] PostGIS vector adapter: 20+ functions produce identical results to vanilla
- [ ] PostGIS raster adapter: ST_MapAlgebra, ST_Clip, ST_Reclass pixel-identical to vanilla
- [ ] h3 adapter: all functions produce identical results
- [ ] h3 GPU pipeline: h3_lat_lng_to_cell bulk GPU correct with stats showing usage
- [ ] h3 GiST batched recheck: cell containment queries correct via GPU h3 kernel
- [ ] h3 SP-GiST batched recheck: cell containment queries correct via GPU h3 kernel
- [ ] pg_builtins adapter: 10+ functions produce identical results
- [ ] Three-layer GPU pipeline: spatial join correct with GPU stats showing usage
- [ ] Raster GPU pipeline: map algebra correct with GPU stats showing pixel count
- [ ] GPU disabled: all queries still correct via CPU path
- [ ] 28+ end-to-end SQL queries all match vanilla PG (including raster + h3 index)
- [ ] Cost model doesn't inject nodes for small queries
- [ ] All 15 benchmark workloads run
- [ ] >=2x speedup vs PG parallel on >=4 workloads on Apple Silicon
- [ ] Spatial join with Metal GPU: >=3x vs PG parallel
- [ ] Raster map algebra with GPU: >=10x vs PG sequential
- [ ] No workload regresses > 10%
- [ ] cargo pgrx test pg17 -- all tests pass
- [ ] Docker integration: all 28+ e2e queries pass (ON == OFF) on real PG with real data
- [ ] Docker integration: GPU stats visible in pg_accel_stats() after spatial/h3/raster queries
- [ ] Docker integration: all prior phase tests still pass (no regressions)
