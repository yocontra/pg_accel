# Same-protocol final3 versus shared-input attribution

Compare retained qualified final3 PG18 module (before,6e219570...) against the
existing shared-input production18 module (after), in before/after/after/before
order. No rebuild. Root's read-only final3-source snapshot at07f7c9bd passes the
original final3 source.sha256; current source passes production18's manifest.
Both modules have the same reference coefficient and common physical limits;
the probe requires that exact coefficient on both, not ce24's missing field.
Old ce24 binaries and original failed gates remain unchanged/protected.

Identical stable-fixture protocol to approved run3: INT2+INT8 at2,000,017 rows,
five warmups and ten balanced direct/prepared pairs per block,160 measured plus
80 warm total, manual ANALYZE and table-local autovacuum-disabled fixed-width
corpus, full native parity/counters/generic-plan checks. Every sample retained.
Same persistent prepared observer must prove nonperturbing equality. New run4,
new probe-substages.py and owned JIT cache; earlier probes/runs remain immutable.

Only added measurement is existing pg_accel_planner_substage_stats in the
prepared JSON observer: query_fingerprint, decline_cache_lookup,
dependency_revalidation, native_cost_reconstruction and rejection_recording,
each calls/elapsed_us. Prepared result executions must retain zero planner
stage AND substage activity. Both profiling settings already enabled before
PREPARE as before. No new production telemetry, selected-mode switch or refit.
Do not sum nested substage clocks or subtract them as disjoint wall components;
compare each stage independently with hook totals, descriptor/native clocks,
paired wall distribution, block order and CVs. Test-only fresh-input oracle is
not compiled into either production module.

Hypotheses: if remaining direct upper-hook difference is in fingerprint/cache
or native comparator, existing subtimers identify it. If not, repeated selected
builder/capability/residency/budget/physical-work calculations remain unseparated;
report the instrumentation limit rather than assume a cause. This comparison
isolates before/after artifact behavior under one protocol, unlike run1/run3;
it is still diagnostic and cannot replace original six no-slowdown gates.

Run once only after frozen independent review and a quiet task-runtime window.
Same bounded cluster lifecycle/seals; no automatic retries on invalidation.
