# FUTURE.md — Extension Acceleration Roadmap

Extensions pg_accel could accelerate beyond the initial PostGIS + h3-pg + builtins scope.
Ordered by community usage and potential impact.

---

## Tier 1 — High Impact, Large User Base

### pgvector
**What:** Vector similarity search for AI/ML embeddings.
**Hot path:** `l2_distance`, `cosine_distance`, `inner_product` on 768–4096 dim float32 vectors.
**How:**
- **GpuReduce**: distance computation is embarrassingly parallel — dot product / L2 norm
  across 1536 floats per pair. Ideal GPU workload.
- **GPU kernel**: single SYCL kernel computing batched pairwise distances. One invocation
  replaces thousands of sequential distance calls during index recheck or exact scan.
- **Late materialization**: for filtered ANN queries (`WHERE metadata->>'type' = 'X'
  ORDER BY embedding <-> query LIMIT 20`), evaluate cheap metadata filter before
  deserializing expensive 6KB vector columns.
- **Estimated impact**: 5-10x for exact scan, 2-3x for HNSW recheck on large candidate sets.

### pg_trgm
**What:** Trigram-based text similarity and fuzzy search.
**Hot path:** `similarity()`, `word_similarity()`, `%` operator on text columns.
**How:**
- **BatchedEval**: trigram extraction + Jaccard computation called on main thread,
  but late materialization skips deserializing other columns for non-matching rows.
- **GPU kernel** (future): trigram set intersection is a bulk bitwise operation —
  represent trigram sets as bitmaps, compute Jaccard on GPU for large candidate sets
  from GIN/GiST index recheck.
- **Estimated impact**: 2-3x for `SELECT * FROM docs WHERE name % 'search' ORDER BY similarity(name, 'search') DESC LIMIT 20` on 1M+ rows.

### PostGIS Raster (postgis_raster)
**Included in v0.1.0** — see Phase 6 (GPU raster kernels) and Phase 7 (raster adapter).
ST_MapAlgebra, ST_Clip, ST_Reclass all have GPU kernels. Target: 10-50x for map algebra.

### pgcrypto
**What:** Cryptographic functions — hashing, encryption.
**Hot path:** `digest()`, `crypt()`, `gen_salt()`, `pgp_sym_encrypt/decrypt`.
**How:**
- **GPU kernel**: SHA-256/bcrypt are massively parallel. Batch hash computation
  across thousands of rows simultaneously on GPU.
- Use case: bulk data anonymization (`UPDATE large_table SET email = digest(email, 'sha256')`)
  or bulk verification.
- **Caveat**: encryption functions have security implications — GPU memory must be
  zeroed after use, and key material must never be sent to GPU global memory.
- **Estimated impact**: 5-20x for bulk hashing operations.

---

## Tier 2 — Medium Impact, Specialized User Base

### pg_stat_statements / pg_qualstats
**What:** Query statistics tracking.
**How:** Not a function-acceleration target, but pg_accel could *feed* better stats.
Track which queries were accelerated, by how much, and expose via
`pg_accel_query_stats` view joining with `pg_stat_statements`.

### temporal_tables / periods
**What:** Temporal/bitemporal table support.
**Hot path:** Range overlap joins (`&&`), `tstzrange` containment.
**How:**
- **GPU kernel**: range overlap is 2 comparisons per pair (similar to bbox).
  Batch range containment/overlap checks on GPU.
- **Late materialization**: temporal joins often have cheap temporal filter + expensive
  payload columns — skip payload deser for non-overlapping ranges.
- **Estimated impact**: 3-5x for temporal join queries on large event tables.

### pg_partman / declarative partitioning
**What:** Partition management and queries across many partitions.
**How:** Not direct function acceleration, but pg_accel's Custom Scan could do
cross-partition batched evaluation — accumulate rows from multiple partition scans
into one batch before evaluating expensive predicates. Currently PG evaluates
predicates per-partition sequentially.

### hstore
**What:** Key-value store in a column.
**Hot path:** `->`, `?`, `@>` operators, `each()`, `avals()`.
**How:**
- **BatchedEval**: main-thread batched evaluation with late materialization.
  For queries like `WHERE hstore_col -> 'key' = 'value' AND expensive_func(other_col)`,
  evaluate cheap hstore lookup first, skip expensive column for non-matches.
- **Estimated impact**: 1.5-2x for filtered queries on wide tables with hstore.

### citext
**What:** Case-insensitive text type.
**Hot path:** Comparison operators, `=`, `<>`, sorting.
**How:**
- **BatchedEval**: citext comparison is just `lower()` + compare. Late materialization
  helps when citext filter is one of multiple predicates.
- Minimal standalone impact, but free acceleration when citext columns appear in
  queries that already use our Custom Scan for other reasons.

---

## Tier 3 — Niche but Interesting

### pgRouting
**What:** Graph routing on geospatial networks (Dijkstra, A*, TSP).
**Hot path:** `pgr_dijkstra`, `pgr_astar`, `pgr_TSP` on large road networks.
**How:**
- **GPU kernel**: parallel SSSP (single-source shortest path) via GPU BFS/Bellman-Ford.
  Research area — GPU graph algorithms are active field.
- **Caveat**: pgRouting's algorithms are complex, stateful, and not easily decomposed
  into independent parallel work items. Would require custom GPU kernels, not just
  wrapping existing functions.
- **Estimated impact**: 3-10x for large graph queries (10M+ edges), but significant
  engineering effort. Better as a separate project collaborating with pgRouting maintainers.

### timescaledb (compression layer)
**What:** Time-series compression and aggregation.
**Hot path:** Decompression + aggregation on compressed chunks.
**How:**
- **GPU reduce**: SUM/AVG/MIN/MAX on decompressed numeric arrays. TimescaleDB stores
  compressed chunks as arrays — bulk reduce on GPU after decompression.
- **Caveat**: TimescaleDB has its own Custom Scan nodes. Would need to compose our
  Custom Scan with theirs, or integrate at the adapter level.
- **Estimated impact**: 2-3x for aggregation over compressed hypertables.

### pgroonga / pg_bigm
**What:** Full-text search with CJK support.
**Hot path:** `@@` operator, ranking functions.
**How:**
- **BatchedEval**: late materialization on recheck — GIN/GiST index returns candidates,
  our Custom Scan batches the recheck + ranking computation.
- **GPU kernel** (speculative): batch TF-IDF or BM25 scoring on GPU for large candidate
  sets. Scoring is independent per document — embarrassingly parallel.

### citus (distributed queries)
**What:** Distributed PostgreSQL.
**How:** pg_accel runs per-shard. Each Citus worker node gets pg_accel acceleration
independently. No special integration needed — just install pg_accel on each worker.
The per-shard speedup compounds with Citus's horizontal scaling.

---

## How to Add Any of These

Every extension above follows the same adapter pattern:

```rust
// 1. Create adapters/myext.rs
pub fn myext_adapter() -> ExtensionAdapter {
    ExtensionAdapter {
        name: "myext",
        version_query: "SELECT extversion FROM pg_extension WHERE extname = 'myext'",
        functions: vec![
            // BatchedEval for functions that call palloc
            FunctionAccelEntry {
                pattern: FunctionPattern::new("myext_func", &["float8"], "float8"),
                strategy: AccelStrategy::BatchedEval,
            },
            // GpuSpatial for geometry/distance predicates with GPU kernel
            FunctionAccelEntry {
                pattern: FunctionPattern::new("myext_distance", &["vector", "vector"], "float8"),
                strategy: AccelStrategy::GpuSpatial { /* kernel config */ },
            },
        ],
    }
}

// 2. Add TypeExtractor if extension has custom types
// 3. Add GPU kernel to pgaccel-kernels/ if using GpuSpatial/GpuReduce
// 4. Add to all_adapters() in adapters/mod.rs
// 5. Write tests: ON == OFF for every function
```

The adapter pattern is designed so community contributors can add support for
their favorite extension without touching pg_accel's core.

---

## Prioritization Criteria

When deciding which extension to accelerate next:
1. **User base size** — more users = more impact
2. **Hot path parallelizability** — can the work be batched? Is it GPU-friendly?
3. **Existing pain** — do users actually hit performance limits here?
4. **Engineering effort** — BatchedEval adapters are < 50 lines. GPU kernels are weeks.
5. **Correctness risk** — simpler functions are safer to wrap

**Recommended next after v0.1.0:** pgvector (huge user base, perfect GPU fit),
then PostGIS Raster (massive GPU win for a vocal user community).
