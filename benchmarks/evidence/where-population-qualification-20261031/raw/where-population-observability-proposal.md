# Completed native WHERE population evidence

Scope: read-only observability for the frozen 32-cell full-SQL qualification. No ABI, native dispatch, cost, cache, capacity, admission, successful-leaf publication, or sampling changes.

## Owned files

Production accessors and EXPLAIN only:

- `pg_accel/src/engine/executor/agg/hash_partition.rs`
- `pg_accel/src/engine/executor/agg/descriptor.rs`
- `pg_accel/src/engine/executor/agg/execute.rs`
- `pg_accel/src/engine/ffi/custom_scan/explain.rs`

Inline driver/accessor unit tests remain in those modules. Actual PG18/PG19 readback and parity tests extend only `pg_accel/src/engine/executor/agg/raw_count_test.rs` and `pg_accel/src/engine/executor/agg/h3_count_test.rs`. The already-owned five benchmark files retain their current scope; a new scratch qualification checker will enforce exact matrix/artifact inventory.

## Existing source facts and proof chain

`descriptor.rs` requires `counters.where_rows` on a successful canonical WHERE execution (currently near line 4370). The only successful-leaf retirement calls `complete_population_leaf(selected, whole, fact_rows)` (near 4459). That function validates whole <= input, leaf <= whole, stable whole across leaves, checked population addition, and exact final coverage. Failed capacity or semantic attempts cannot retire a successful leaf. No new collection is needed.

The driver getter returns `(whole, sum, leaves)` only when current is None, pending_len is zero, whole_rows is Some, selected_rows equals whole, and completed_leaves is positive. Incomplete, failed, or non-population retirement returns None. Zero is a valid population, not an absence sentinel.

The descriptor getter additionally requires the existing canonical `hash_where_filter` validator to succeed and return Some, and an existing adaptive driver. It adds the existing logical fact_rows as input. It does not infer native v2 from a plan label. No-WHERE executions omit proof even though their driver also checks conservation.

The execution getter requires adaptive state present, descriptor.completed, and gpu_dispatched before reading the descriptor getter. Existing rescan/reset clears completed and dispatched, so an incomplete/cancelled next execution cannot expose the previous execution's proof. The scope is the last completed execution, rescan-local, not a cumulative backend counter. A natural one-shot successful execution emits proof even when overflow_attempts is zero. Empty input that does not dispatch omits proof; if a zero-population dispatch completes, zero whole/sum remains representable.

Only the existing EXPLAIN ANALYZE branch emits the proof. Proposed same-node fields: `GPU WHERE Population ABI: hash_partition_v2`, `GPU WHERE Population Scope: last completed execution (rescan-local)`, `GPU WHERE Unique Input Rows`, `GPU WHERE Whole Rows`, `GPU WHERE Successful Leaf Rows`, and `GPU WHERE Completed Leaves`. Fields are emitted together only after the above guards. Integer conversion must not silently turn overflow into an apparently exact value.

## Benchmark consumption and tests

The same-node parser requires all fields together, exact ABI and scope, input equal to the cell's fixed N, whole equal to the independently computed fixture accepted population, successful-leaf sum equal to whole, and leaves > 0. A different Custom Scan node cannot supply missing facts. Missing, wrong, incomplete, no-WHERE, failed, and reset cases reject. Existing lifecycle/mask/hash/zero-stock/full parity requirements remain independent.

Add passing designated gate fixtures for warm and CacheMode::Both, then individually mutate settings, lifecycle, consumed output, masks, native fallback, population fields, and speed floor. Both must merge new WHERE settings and baseline deltas exactly like existing evidence, and expected output uses checked 16389 * measured iteration count rather than a hardcoded ten-query total. This compatibility test does not change the fixed warm qualification schedule.

The outer checker requires exactly the frozen 32 name/scale pairs, with no missing, duplicate, or unknown cells, and authenticates retained per-cell workload/SQL/config manifests and ordered H3 seed artifacts/digests. Missing, tampered, or another cell's manifest/seed evidence rejects. All raw timing samples remain retained. No production telemetry is inferred from historical correctness qualification.

Validation: inline driver/getter tests (initial/incomplete/error/split/one-shot/zero/no-WHERE/reset), parser and full gate positives/negatives, PG18/PG19 actual ANALYZE readback plus full result parity, strict lint/format/ordinary tests and source review before any timing. Current fixed 32/22/G4096/six-control performance protocols remain unchanged.
