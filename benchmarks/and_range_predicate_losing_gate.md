# Multiple Range Predicate Losing Gate

The provisional production lane
`and_range_predicate_expression_grouped_agg_int4` was measured on PostgreSQL
18.4 at 1,000,000 rows with five warmups, ten measured iterations, raw timing,
and a warm cache. The installed and expected release library SHA-256 was
`a63cf2c832aa950937026d5fae22660763f968105eac6e441987d5bcd4a9e3b7`.

The selected plan was `Custom Scan (GpuAccelAgg)` with
`GPU Resident Pipeline: true`, one canonical range descriptor, and 256 output
rows. Exact PostgreSQL parity passed with zero rows in either direction of the
correctness diff.

Measured accelerated milliseconds:

`10099.61, 10100.99, 10099.64, 10100.47, 10102.02, 10099.79, 10105.47, 10098.76, 10100.23, 10102.69`

Measured PostgreSQL parallel milliseconds:

`18.16, 21.36, 19.31, 19.31, 19.35, 18.96, 19.12, 19.96, 19.77, 18.48`

The medians were 10,100.35 ms accelerated and 19.31 ms PostgreSQL, or about
0.00191x speedup (approximately 523x slower). This misses the predeclared
1.15x floor catastrophically. Production therefore declines a second scalar
range clause on the same column with `shape_multiple_range_predicates`;
PostgreSQL's analyzed `BETWEEN` form has the same boundary. One scalar
comparison remains eligible.

Raw local evidence is preserved without relabeling at
`benchmarks/artifacts/and-range-predicate-1m-warm-20260802T172332Z`. The run was
stopped after the complete 1M cell, during unnecessary 10M setup, so the
artifact is losing diagnostic evidence rather than a finalized release bundle.
