//! Per-row cost constants and dispatch overhead budgets.

/// Safety margin for GPU vs CPU cost comparison.
///
/// The GPU path must estimate at least this fraction cheaper than CPU
/// before being chosen. 0.7 means GPU total cost must be ≤70% of CPU
/// total cost, preventing marginal cases where estimation noise could
/// make a query slower on GPU.
///
/// Derivation: PG cost estimates carry ±30% noise for row counts under
/// 100K (observed via EXPLAIN ANALYZE variance across 50 queries on
/// M2 Max). A 30% margin ensures we never choose GPU when the true
/// cost difference is within the noise band.
pub const GPU_COST_SAFETY_MARGIN: f64 = 0.7;

/// Per-datum extraction cost for columnar transposition. When building
/// a columnar batch for GPU dispatch, each referenced column incurs
/// this per-row cost for slot_getattr + datum copy.
///
/// Derivation: Measured slot_getattr + datum copy loop at ~1ns/datum on
/// M2 Max (perf counter). PG's cpu_tuple_cost = 0.01 covers ~10
/// operations, so one datum extraction ≈ 0.001 in PG cost units.
pub const PER_DATUM_EXTRACT_COST: f64 = 0.001;

/// Fixed overhead for launching a GPU kernel, in arbitrary cost units.
///
/// This accounts for queue submission, buffer allocation, and device
/// synchronisation latency. Batching must save more than this to be
/// worthwhile on a GPU path.
///
/// Derivation: Metal command buffer submit + fence sync measured at
/// ~80-120µs on M2 Max. PG's seq_page_cost = 1.0 ≈ 1ms of I/O, so
/// 100µs ≈ 0.1 in PG cost units. We use 5.0 (50x) as a conservative
/// fixed penalty to strongly discourage GPU for small batches.
pub const GPU_LAUNCH_OVERHEAD: f64 = 5.0;

/// Fixed overhead for PreAgg (fused star-join aggregation), in cost units.
///
/// PreAgg is a CPU-only executor node: it walks the fact table heap
/// directly via `heap_getnext`, probes in-memory hash tables, and
/// accumulates aggregates — no GPU kernel is launched. The overhead
/// covers scan descriptor setup, dimension materialization, and hash
/// table construction, but NOT GPU queue submission or device sync.
///
/// Derivation: Measured PreAgg setup (open scan + build N hash tables
/// for N=2 dimensions, 2K rows each) at ~200-400µs on M2 Max. In PG
/// cost units (1.0 ≈ 1ms), this is ~0.3. We use 0.5 as a conservative
/// estimate.
pub const PREAGG_FIXED_OVERHEAD: f64 = 0.5;

/// Maximum index selectivity at which a GiST/SP-GiST index scan is
/// considered "cheap enough" to skip Custom Scan injection.
///
/// When an `IndexPath` using a spatial index (GiST or SP-GiST) has
/// selectivity below this threshold, the index is highly selective —
/// very few rows pass the filter — and PostgreSQL's native index scan
/// is faster because it avoids touching most heap pages entirely.
/// Custom Scan + GPU dispatch adds overhead (geometry deserialization,
/// batch setup, kernel launch) that exceeds the savings when the index
/// already prunes >90% of rows.
///
/// Derivation: Benchmarked `spatial_filter` workload (ST_Intersects
/// with GiST index, ~5% selectivity). Vanilla PG index scan: 1.3ms.
/// pg_accel Custom Scan wrapping seq scan: 8.6ms (6.9x regression).
/// At 10% selectivity the index scan touches ~10% of pages, well below
/// the break-even point for GPU batch dispatch. Setting to 0.10 (10%)
/// ensures we defer to the index for selective spatial filters while
/// still intercepting full-table spatial joins and low-selectivity
/// predicates where GPU batching wins.
pub const SPATIAL_INDEX_SELECTIVITY_THRESHOLD: f64 = 0.10;

/// Maximum ratio of index-path total cost to seq-scan total cost at
/// which we defer to the index.
///
/// Even when selectivity is not available (e.g., bitmap paths), we can
/// compare the index path's total cost to the cheapest non-index path.
/// If the index path is this fraction or less of the seq scan cost,
/// the index is doing enough pruning that Custom Scan overhead would
/// be a net loss.
///
/// Derivation: At the spatial_filter regression, index scan cost was
/// ~15% of seq scan cost. A threshold of 0.40 (40%) catches cases
/// where the index saves more than half the work — in those cases the
/// GPU batch overhead is unlikely to recoup the savings.
pub const SPATIAL_INDEX_COST_RATIO_THRESHOLD: f64 = 0.40;

/// Estimated per-row cost for a spatial predicate (geometry deser +
/// bbox + GPU kernel amortised).
///
/// Derivation: PostGIS ST_Intersects on complex polygons costs ~5µs/row
/// on CPU (PG EXPLAIN ANALYZE, M2 Max). The GPU three-layer pipeline
/// processes ~200K rows/sec including deser overhead, yielding ~5µs/row
/// amortised. In PG cost units (1.0 ≈ 1ms), 5µs ≈ 0.005, but geometry
/// deserialization adds ~10x overhead vs. numeric types, so 0.05.
pub const GPU_SPATIAL_PER_ROW_COST: f64 = 0.05;

/// Estimated per-row cost for a raster operation (pixel extraction +
/// GPU kernel amortised).
///
/// Derivation: Raster map algebra extracts pixel bands (~4µs/row
/// measured on M2 Max) and dispatches to GPU. Slightly cheaper than
/// spatial because pixel extraction is simpler than GSERIALIZED
/// deserialization. 4µs/row ≈ 0.004, with 10x deser overhead = 0.04.
pub const GPU_RASTER_PER_ROW_COST: f64 = 0.04;

/// Estimated per-row cost for an H3 operation.
///
/// Derivation: H3 cell operations are pure integer/trig math with no
/// geometry deserialization. GPU throughput measured at ~50M cells/sec
/// on M2 Max (h3_latlng_to_cell benchmark), yielding ~20ns/row.
/// In PG cost units: 20ns ≈ 0.00002, but we use 0.02 to account for
/// datum extraction overhead and to avoid GPU dispatch for trivially
/// small batches.
pub const GPU_H3_PER_ROW_COST: f64 = 0.02;

/// Estimated per-row cost for a GPU sort (key extraction + bitonic sort amortised).
///
/// Derivation: Measured on M2 Max with 10M wide rows (120 bytes/row):
/// GPU bitonic sort throughput is ~2.2M rows/sec (4,569ms / 10M rows),
/// PG external merge sort is ~660K rows/sec (15,137ms / 10M rows).
/// GPU per-row cost in PG units: PG's sort cost for 10M rows is ~150
/// in total_cost. 150 / 10M = 0.000015 per row PG-native. GPU is ~3.3x
/// faster, so 0.000015 / 3.3 ≈ 0.0000045, but we use 0.015 to include
/// key extraction, buffer setup, and provide a conservative estimate
/// that ensures GPU sort is only chosen when disk spill makes it
/// clearly beneficial.
pub const GPU_SORT_PER_ROW_COST: f64 = 0.015;

/// Estimated per-row cost for a GPU reduction (sum, min, max, count).
///
/// Includes materialization + value extraction + GPU dispatch overhead.
/// Must exceed PG's native Agg per-row cost (~0.01) so we only win
/// when the GPU spatial/h3 filter path already buffers the data.
///
/// Derivation: PG's native Agg node processes ~100M rows/sec for
/// simple SUM (cpu_operator_cost = 0.0025 per row). GPU reduction
/// adds materialization overhead (~20ns/row) on top of the kernel.
/// We set this at 0.03 (3x PG's batched-eval baseline) so the GPU
/// aggregate path is only chosen when data is already buffered by
/// an upstream GPU scan/filter node, avoiding unnecessary
/// materialization for standalone aggregates.
pub const GPU_REDUCE_PER_ROW_COST: f64 = 0.03;

/// Estimated per-row cost for GPU expression evaluation (WHERE clauses).
///
/// GpuExpr evaluates standard numeric predicates (>, <, =, BETWEEN, AND, OR)
/// on columnar-transposed data via the GPU bytecode VM. Per-row cost includes
/// columnar transposition (~1ns/datum) + GPU kernel amortised over batch.
/// Must beat PG parallel only when expression is complex enough and rows are
/// large enough to amortise GPU launch overhead.
///
/// Derivation: GPU expr eval processes ~50M rows/sec on M2 Max for simple
/// predicates. In PG cost units: 20ns/row ≈ 0.00002. We use 0.025 to
/// include transposition overhead and ensure the GPU path only wins on
/// large batches with non-trivial expressions.
pub const GPU_EXPR_PER_ROW_COST: f64 = 0.025;

/// Estimated per-row cost for GPU hash join (build + probe).
///
/// Derivation: GPU hash join on M2 Max processes ~20M rows/sec for
/// int32/int64 keys (hash build + probe). In PG cost units: 50ns/row
/// ≈ 0.00005. We use 0.02 to include key extraction overhead and
/// Custom Scan yield overhead (~3μs/row for output construction).
pub const GPU_HASH_JOIN_PER_ROW_COST: f64 = 0.02;
