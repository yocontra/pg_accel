# Dense regression attribution diagnostic

No production changes, no rebuild, no cost fit, no performance-gate replacement.
Original failed INT2/INT8 2M old/new ABBA evidence remains authoritative.

Use the exact qualified PG18 old baseline-ce24 and final3 modules, sealed before
and after. Four fresh owned clusters in fixed old/new/new/old order. Within each,
run exact arithmetic-v1 INT2 then INT8 fixture at 2,000,017 rows, 64 non-NULL keys
plus NULL, with original value/qualifies columns. Original realistic settings are
applied to all three persistent backends; only planner/executor profiling is
additionally enabled. No strategy override, no cache purge, no source edits.
Each block must match the 76 common physical device limits and device identity
from the sealed original baseline receipt. The sole allowed inventory difference
is new-only gpu_dense_count_serial_work_cost=0.06788047049882354 with source
reference_device_calibrated; the diagnostic records it explicitly, not as a
physical-device mismatch or a newly fitted coefficient.

Native-off backend supplies the complete 65-row oracle. Two independent pinned
backends run direct SELECT and parameterless PREPARE/EXECUTE. Preparation and an
EXPLAIN ANALYZE actual dense-path proof precede all samples. Each receives five
warmup executions, then ten paired executions with a fixed five/five AB/BA
schedule. Retain every warmup, measured rowset, counter snapshot, and duration.
No trimming, retry-for-pass, choosing best run, or additional cells after results.

The client uses eleven typed libpq calls, checks every status, consumes all rows,
and frees each PGresult in finally. No dependency install or new binary build.
Raw host-wall interval includes PQexec, complete result consumption and decoding,
but excludes before/after counters and parity. It is diagnostic, not a headline.

Counter observer is itself parameterless PREPAREd and warmed; two successive
snapshots must be exactly equal before samples, proving no observer planning or
executor-counter contamination. No SET, EXPLAIN, PREPARE, DISCARD, oracle query,
or result-validation SQL occurs between measured before/after snapshots. Actual
result-producing SELECT/EXECUTE must show four GPU lifecycle calls, three dense
data calls, zero other physical modes/stock fallback, and full native equality.
Warm snapshots must show zero artifact builds/rebuilds/raw load.

Cached target execution must increase pg_prepared_statements.generic_plans by
exactly one, custom_plans remain zero, and planner-stage calls remain unchanged.
Direct SELECT must increment upper_group_agg calls. Counter snapshots retain
planner stage timings, seven descriptor stages, native transition/queue/copy
telemetry, artifact lifecycle, and complete stats. Native oracle itself is
bracketed and must show GPU/stock deltas zero. Profiling is configured before any
target preparation and never toggled within a backend.

Interpretation: compare old/new direct and prepared medians plus paired
direct-minus-prepared distribution, planner upper-group timing, descriptor plan
build/kernel/materialization, and native waits/calls. Report sample CV and all
block results. Prepared convergence with higher new direct planner time supports
planning attribution; persistent prepared regression points into execution.
This experiment changes cache handling deliberately (no DISCARD ALL, required
for cached-plan reuse), enables profiling, and pairs direct/prepared rather than
GPU/native; it cannot certify original no-slowdown gates or isolated device time.

Runner rejects existing outputs/socket, bounds setup/start/probe/cleanup, stops
only its owned clusters (fast then immediate), checks stopped/no PID, and seals
original modules/source/harness/runtime/libpq and root release artifacts. Shared
JIT cache is new and owned by this diagnostic; historical cache is untouched.
No runtime until independent review approves the frozen harness.
