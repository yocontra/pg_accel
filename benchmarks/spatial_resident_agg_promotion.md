# Resident Spatial Aggregate Promotion Gate

Status: **promoted for the exact evidenced shape**.

The candidate is `spatial_resident_agg_candidate` at exactly 1,000,000 rows. It
measures one resident `geometry(Point,4326)` column against one constant simple
polygon through `ST_Intersects`, consumed by `count(*)`. The polygon contains
1,025 coordinates, clears the hardware-scaled vertex minimum, crosses the
cooperative-kernel boundary, and produces more than the calibrated 500M
vertex-row work floor. The deterministic fixture includes NULL, exterior,
interior, and boundary points. Its result oracle derives membership from the
integer grid and never calls PostGIS.

Normal planning admits only ungrouped, unjoined `COUNT(*)` over
`ST_Intersects(point_column, one_ring_polygon_constant)` with a same, nonzero
SRID. The column must be the left operand. `Contains`, `Within`, `DWithin`,
reversed operands, grouped aggregates, joins, and other aggregate projections
remain outside normal admission. The external
`92_spatial_resident_agg_contract.sql` contract requires PostGIS/oracle parity,
the selected resident plan and dispatch proof on a capable host, a positive
kernel delta, and zero stock fallback.

The release-build experiment ran on the qualified PG18/Metal host through
normal planner admission, not the `pg_test` force GUC:

1. Prove the selected `GpuAccelAgg` plan, resident pipeline, actual output row,
   positive kernel counter delta, and zero fallback.
2. Run the PostGIS differential and independent arithmetic oracle.
3. Exercise the 65,536-row chunk boundary, full uncertain-row accounting,
   exact recheck, cancellation, injected dispatch failure, cleanup balance,
   and backend reuse contracts.
4. The retained 10-iteration warm result at 1M rows is 14.82x by median:
   3.56 ms for pg_accel versus 52.70 ms for PostgreSQL parallel. The benchmark
   ship gate required 1.15x and passed.

The passing artifact is
`benchmarks/artifacts/spatial-promotion-pass-3`. It records the selected
`GpuAccelAgg` plan, resident-pipeline proof, 10 positive kernel-counter deltas,
zero stock fallback, the independent correctness diff, and the 1.15x threshold
matrix pass. No planner cost override or test forcing was used.
