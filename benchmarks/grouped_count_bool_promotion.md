# Boolean Grouped COUNT Promotion Gate

Status: **promoted for the 250K–1M Metal envelope; clean-candidate release
rerun still required**.

The released descriptor groups one nullable boolean fact column and computes
`COUNT` over one distinct nullable boolean fact column. It has no filter, join,
`HAVING`, aggregate modifier, or additional measure. Global, same-column,
filtered, joined, and multi-measure variants remain PostgreSQL-native with
`generic_serial_kernel_mode_unqualified`.

The deterministic fixture produces false, true, and NULL keys; every eleventh
measure is NULL. Its arithmetic oracle derives all three counts from the integer
row identifier rather than reusing the grouped query. SQL94 proves exact output,
the verified `parallel_dense_count` physical mode, the
`dense_bool_count_plain` specialization, positive dispatch, and zero fallback.
SQL95 covers prepared-plan reuse plus DML/DDL invalidation. SQL96 preserves the
same-column native boundary.

## Specialized design

The hierarchical dense-count kernel keeps two independent partials:

- selected rows, which determine group activity and query-level selected count;
- non-NULL measure rows, which determine SQL `COUNT(column)` and its non-NULL
  lane.

That separation preserves an all-NULL group as an active group with count zero.
Malformed null bytes fail before either partial is changed. Membership and SQL
mask variants are compile-time specializations, though normal planning admits
only the narrower no-join/no-filter release descriptor.

## Current characterization

The PG18.4/Apple M2 Max envelope run used five warmups, ten measured randomized
pairs per scale, raw wall-clock timing, realistic observed GUCs, and a warm
resident cache. It measured:

- 250K: pg_accel **1.66 ms**, PostgreSQL **9.59 ms**, **5.77x**
- 1M: pg_accel **2.92 ms**, PostgreSQL **21.13 ms**, **7.23x**
- required floor: **1.15x** at both boundaries
- physical calls: **20/20 parallel dense-count**, zero serial-generic
- artifact steady state: **20/20 hits**
- stock executor fallback: **0**
- correctness diff: **pass**, including false, true, NULL keys and NULL values

Characterization evidence is in
.codex/scratch/bool-count-fastpath-envelope-characterization`; its `artifact_index.json`
SHA-256 is
`a93e5fcfae6afa21f93ba7fdd1b13ab8be8c9f57d76de088aec244871632de61`.
An unrelated user process was consuming a CPU core during this run, so these
numbers qualify the implementation but are not the final clean-candidate
release artifact.

## Historical losing implementation

The former serial generic descriptor path measured 3,508.11 ms versus 18.87 ms
(0.0054x) and was correctly kept native. Its sealed evidence remains under
`benchmarks/artifacts/grouped-count-bool-promotion`. The new physical kernel
removes that recurring dictionary/generic aggregation cost instead of weakening
the gate or broadening admission.
