# Boolean Grouped COUNT Promotion Gate

Status: **not promoted; explicit native decline retained**.

The candidate is `grouped_count_bool_candidate` at exactly 1,000,000 rows. It
groups one nullable boolean key and computes `COUNT` over a distinct nullable
boolean column. The deterministic fixture produces false, true, and NULL keys;
every eleventh measure is NULL. Its arithmetic oracle derives all three counts
from the integer row identifier and does not reuse the grouped query.

The temporary planner experiment admitted only one fact relation, one nullable
boolean column key, one distinct boolean column `COUNT` measure, no filter or
join, and the canonical key-then-count projection. It proved correct selected
execution, a positive kernel counter delta, and zero stock fallback, but failed
the performance gate by a wide margin. The exception was therefore removed;
both grouped and global boolean column `COUNT` remain native with
`shape_unsupported_aggregate_input`.

## Measured losing gate

The independent PG18.4/Apple M2 Max release run used five warmups, ten measured
iterations, raw wall-clock timing, randomized paired ordering, and a warm
resident cache. It measured:

- pg_accel median: **3508.11 ms**
- PostgreSQL parallel median: **18.87 ms**
- median speedup: **0.0054x** against a required **1.15x**
- kernel counter delta: **50**
- stock executor fallback delta: **0**
- correctness diff: **pass**, including false, true, NULL keys and NULL values

The durable evidence is in
`benchmarks/artifacts/grouped-count-bool-promotion`. Module SHA-256:
`58a0913b1b86d3ce86278ce55be03d1ae5e5e7c472b735f7f885ba45d70a8126`.

Future work must first remove the roughly 3.5-second recurring resident
dictionary/grouped-count execution cost, then repeat this same gate. The
isolated workload and SQL94 native/NULL contract remain as the regression and
promotion fixture; no released performance envelope exists for this shape.
