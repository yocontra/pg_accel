# pg_accel Benchmark Artifacts

Log/telemetry tails are capped at `65536` bytes per file, with at most `32` unique candidate files considered.

Each log capture also writes a bounded delta capped at `262144` bytes, using the source file size observed when the artifact writer started.

- `artifact_index.json`: machine-readable inventory of generated artifact files, sizes, and modified timestamps.
- `artifact_checklist.md`: markdown checklist view of the generated artifact inventory.
- `resume_audit_manifest.json`: durable resume/audit manifest linking bounded log policy, run-start log offsets, and generated evidence files.
- `report.json`, `report.md`, `report.csv`: rendered benchmark report.
- `no_dispatch_audit.json`, `no_dispatch_audit.md`: machine-readable and markdown audit for rows that did not prove pg_accel GPU dispatch and therefore cannot be used as GPU performance conclusions.
- `resident_boundary_audit.json`, `resident_boundary_audit.md`: machine-readable and markdown audit for selected pg_accel Custom Scan resident-pipeline / CPU-boundary evidence.
- `benchmark_failure_ledger.json`, `benchmark_failure_ledger.md`: merged work queue for release-blocking gate failures and measured rows still below PostgreSQL-parallel parity.
- `crashes.json`, `crashes.md`: crash inventory and repro commands.
- `crash_contexts/`: per-crash repro context with linked plan, correctness, GUC, and log excerpts.
- `correctness_diffs/`: bounded accel-vs-baseline result diff summaries captured before timing.
- `guc_snapshot.json`: PostgreSQL settings observed by the harness.
- `provenance.json`: pg_config, SQL metadata, and extension binary hashes.
- `provenance-warnings.txt`: provenance gaps or hard-fail reasons, when present.
- `plan_snippets/`: EXPLAIN snippets captured before timed execution.
- `fixture_manifests/`: pinned external-corpus provenance, copied before setup.
- `pre_risk_contexts/`: same-backend `pg_backend_pid()`, workload SQL, config basics, and `EXPLAIN` without `ANALYZE` captured before risky execution.
- `log_tails/`: bounded PostgreSQL and pg_accel log/telemetry tails plus run-start deltas per failure and at run completion.
