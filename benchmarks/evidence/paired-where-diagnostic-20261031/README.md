# Internal paired WHERE candidate diagnostic

One fixed PG18 run on the qualified test build completed with primary/secondary status zero. All 442 receipts (100 scheduled executions plus four separate JIT probes) passed strict inventory, full multiset parity, actual dispatch, lifecycle, ownership and v2 population checks. No samples were removed or retried. This compares two independently lowered pg_accel candidates, **not pg_accel versus PostgreSQL** and not the broader WHERE performance gates.

The measured interval starts before descriptor/executor construction and ends after actual EOF, normal close and PostgreSQL resource frees. It is not whole-SQL latency. Stock oracles, default-plan proofs, recipe capture, invalidation and durable receipt writes are outside the interval. Included diagnostic capture is reported separately, not subtracted. Consumption includes dispatch; its residual after subtracting dispatch is not an isolated materialization measurement.

## Descriptive results

All values are medians in milliseconds; CV uses sample standard deviation divided by mean. Each reported phase has ten balanced pairs per fixture, after five warmup pairs. Fresh-artifact means exact Legacy derived-artifact eviction, not raw relation reload or cold OS cache.

| Fixture / phase | Legacy ms | Raw ms | Ratio of arm medians (Legacy/Raw) | Legacy / Raw CV |
|---|---:|---:|---:|---:|
| INT2 COUNT-star warm | 54.245 | 18.553 | 2.924 | 4.294% / 2.062% |
| INT2 COUNT-star fresh artifact | 584.173 | 18.593 | 31.419 | 1.000% / 2.964% |
| INT8 filtered COUNT warm | 961.410 | 17.719 | 54.260 | 0.242% / 7.169% |
| INT8 filtered COUNT fresh artifact | 1502.875 | 17.944 | 83.751 | 0.203% / 10.755% |

Every individual paired ratio favored Raw; all values, ranges, CVs and order are retained in `raw/paired-run1/descriptive-summary.json`. The table's ratios divide the arm medians; they are not medians of individual paired ratios. The table's CVs concern the total descriptor interval only, not every timing component. Warm dispatch medians were 46.312 versus 11.025 ms (INT2) and 953.660 versus 10.582 ms (INT8). Fresh Legacy preparation medians were 528.796 and 541.427 ms. The preparation metric is not an isolated artifact-construction timer. Consumption-minus-dispatch medians were approximately 6.94–7.74 ms, with included diagnostic capture approximately 5–8 microseconds.

Actual lifecycle counts were Legacy 3 (2 data + 1 control) versus Raw 17 (1 aggregate + 16 mask conversions) for INT2; Legacy 29 (28 data + 1 control) versus Raw 33 (1 aggregate + 32 mask conversions) for INT8. Natural capacities were used. Both separate default-plan proofs selected verified parallel hash with no stock fallback. Captured total costs ranked Raw below Legacy: 5544.275 versus 18745.443 and 7653.199 versus 8633.216. This supports the corrected ranking locally; it is not a coefficient fit or a general performance claim.

## Provenance and limits

`copy-inventory.json` maps the exact 461 artifacts (24,553,218 bytes), including every raw receipt, terminal/log/seal record, summary generator, checker, SQL, enabled wrapper and its preserved disabled predecessor. The run binds validation7 and both PG18/PG19 cleanup3 qualifications; the actual timing module is PG18. PG19 correctness does not establish PG19 performance.

No PGDATA, JIT cache or compiled binary is included. Original path-based seals require the original checkout and pinned toolchain for replay; compiled module hashes identify rather than distribute dependencies. Earlier harness correctness and failures are archived separately in `../paired-where-harness-20261030/`. Keep this diagnostic distinct from the still-required 32 WHERE cells, released 22-cell matrix, G4096 controls and six strict baseline comparisons. No published speedup or completed full qualification is claimed.
