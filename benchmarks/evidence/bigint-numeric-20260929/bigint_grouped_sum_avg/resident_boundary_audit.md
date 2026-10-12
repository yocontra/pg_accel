# Resident Boundary Audit

| Selected Custom Scan rows | Passed | Failed | Ignored non-Custom-Scan rows |
|---:|---:|---:|---:|
| 2 | 2 | 0 | 0 |

Every selected pg_accel Custom Scan must carry explicit `GPU Resident Pipeline: true` proof. A typed derived-output lane may also prove that one sealed lifecycle GPU kernel constructed the exact final artifact and that every measured warm query consumed a dependency-stable hit without fresh GPU work. CPU/PostgreSQL boundary reasons are preserved as diagnostics, but they are failures for SQL-plan admission.

| Workload | Scale | GPU dispatched | GPU-resident pipeline | Status | Boundary | Detail |
|---|---:|---|---|---|---|---|
| bigint_grouped_sum_avg | 250K | true | reported | reported_resident_pipeline | - | selected Custom Scan reported a GPU-resident pipeline |
| bigint_grouped_sum_avg | 1M | true | reported | reported_resident_pipeline | - | selected Custom Scan reported a GPU-resident pipeline |
