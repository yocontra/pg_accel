# PG19 SQL117 latency diagnostic

Original full matrix process 36837 is terminal. Its wrapper reported matrix
exit 1: PG18 passed 100/100 files and 462/462 fixed assertions; PG19 attempted
all 100 files, with 99 passing files and 460/462 qualified assertions. SQL117
completed its semantic checks but emitted a 5,924,664us accumulate-call warning
at the unchanged 5000ms threshold. Its original logs remain under `../fixed`.
Both full-matrix clusters stopped successfully before these diagnostics.

The scratch probe is `/private/tmp/pgaccel-numeric-latency-probe.lIrAM4/probe.py`.
It runs SQL117's exact eight-column, 1,000,017-row fixture and first SUM query,
pins all columns, verifies stock PostgreSQL SUM parity and positive native
execution deltas, records descriptor stages, then records an explicitly
separate fourth EXPLAIN ANALYZE. Neither million-row cap nor warning threshold
was changed. These durations are host wall/native-call timings, not GPU times.

`same-backend.jsonl` preserves an initial probe harness failure before fixture
setup: `current_setting` formats 5000ms as `5s`. The scratch probe was corrected
to require canonical `pg_settings.setting=5000` and `unit=ms`.

`same-backend-fixed.jsonl` (session 49829, exit 0) used a fresh backend on owned
port 29419 with existing disk cache. Three CTAS wall times: 43.685ms, 13.698ms,
13.740ms. Native lifecycle totals: 40.925ms, 11.733ms, 12.079ms. All results
matched stock, all GPU deltas were positive, no SQL warnings. Fourth EXPLAIN
verified parallel_dense_numeric, selected/resident/dispatched, 1,000,017 rows,
and three lifecycle calls.

`cold/first-backend.jsonl` (session 68750, exit 0) used a new disposable cluster
on port 29519 and newly created private `ACPP_APPDB_DIR=cold/appdb`. CTAS wall
times: 2794.129ms, 14.102ms, 9.907ms. Native lifecycle totals: 2792.421ms,
12.783ms, 8.815ms. All stock parity/GPU delta assertions passed, no SQL warning.
`cold/server.log` confirms fresh JIT archive publication under that private
directory. This difference is consistent with first-use compilation overhead,
but does not directly apportion the original 5.924664s warning.

The first cold probe's cache snapshot path was incorrect: the installed runtime
writes `$ACPP_APPDB_DIR/apps/global/jit-cache`, whereas repository observability
currently expects `$ACPP_APPDB_DIR/global/jit-cache`. Its snapshots consequently
show no directory; the query/timing/parity evidence remains valid. Subsequent
diagnostics must use the actual observed path. Shared cache was not cleared.

No original PostgreSQL installation or user server was modified. No threshold
was relaxed. Successful diagnostic repetitions do not replace the failed full
PG19 qualification or establish GPU-only timing/cancellation bounds.

## Attached Metal trace

The approved original probe's nonce-matched ready/go handshake gated a third
fresh private-cache cluster on port29619 before its first measured query.
`traced-cold/probe.jsonl` (process16735) and `xctrace.log` (process9278) both
reached terminal exit0. Only owned backend11458 was explicitly attached. The
trace still includes system-wide metadata, so the binary, TOC and broad raw
XML export must remain scratch-only, not published as project evidence.

Traced first CTAS was4330.042ms (native lifecycle4327.236ms); repeats13.341ms
and10.044ms. All parity/delta assertions passed, no SQL warning. The traced
sample is distinct from untraced cold timing and may include tracing overhead.

`summarize_trace.py` resolves XML reference IDs and exports only rows whose
process PID is11458. Derived `owned-backend-events.json` and
`owned-backend-summary.jsonl` contain81 GPU intervals totaling/union23.995128ms
over the whole traced workload (four executions);159 submissions total3.314580ms.
Eight shader compiler events attributed to the backend total0.266542ms;
external archive-builder compilation work is not included in that backend
compiler total. GPU intervals start at17.103278s and end19.830736s in trace
time, separated by large host-side gaps during first-use archive creation.

This trace places the observed multi-second cold overhead overwhelmingly
outside measured GPU execution. It is consistent with JIT/archive preparation,
but cannot retroactively apportion the original5.924664s warning or establish
cold-start latency reliability. No arbitrary chunk cap reduction is justified
by these observations. All three diagnostic clusters were stopped cleanly.
