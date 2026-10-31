"""Descriptive fixed-ABBA arithmetic; independent receipt review still required."""
import json
import math
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parent
LANES = [f"dense_int{kind}_count_control" for kind in (2, 4, 8)]
SCALES = (1_000_017, 2_000_017)


def cv(values):
    return statistics.stdev(values) / statistics.mean(values) * 100


cells = []
for lane in LANES:
    for rows in SCALES:
        samples = {"old": [], "new": []}
        blocks = []
        for block, arm in ((1, "old"), (2, "new"), (3, "new"), (4, "old")):
            run = ROOT / f"control-comparison-{block}-{lane}"
            assert (run / "terminal.log").read_text().strip() == "terminal=0"
            report = json.loads((run / "report.json").read_text())
            assert len(report["workloads"]) == 2
            assert {(w["name"], w["rows"]) for w in report["workloads"]} == {
                (lane, scale) for scale in SCALES
            }
            w = next(w for w in report["workloads"] if w["rows"] == rows)
            assert len(w["iterations"]) == 10 and len(w["warmup_iterations"]) == 5
            assert sum(i["accel_first"] for i in w["iterations"]) == 5
            values = [i["accel_ms"] for i in w["iterations"]]
            assert all(math.isfinite(v) and v > 0 for v in values)
            assert w["physical_kernel_mode_verified"] is True
            assert w["pg_accel_stock_exec_delta"] == 0
            if arm == "old":
                assert w["physical_kernel_mode"].startswith("parallel_dense")
            samples[arm].extend(values)
            blocks.append({
                "block": block, "arm": arm, "run": str(run),
                "mode": w["physical_kernel_mode"],
                "accel_median_ms": statistics.median(values),
                "accel_cv_pct": cv(values),
                "parallel_cv_pct": w["parallel_cv_pct"],
                "speedup_vs_parallel": w["speedup_median_vs_parallel"],
                "warm_summary": w["warm_summary"],
                "artifact_lifecycle_probe": w["artifact_lifecycle_probe"],
                "accel_samples_ms": values,
            })
        assert all(len(values) == 20 for values in samples.values())
        medians = {arm: statistics.median(values) for arm, values in samples.items()}
        ratio = medians["old"] / medians["new"]
        cells.append({"lane": lane, "rows": rows,
                      "old_median_ms": medians["old"], "new_median_ms": medians["new"],
                      "old_over_new_median": ratio, "no_median_slowdown": ratio >= 1,
                      "old_cv_pct": cv(samples["old"]), "new_cv_pct": cv(samples["new"]),
                      "blocks": blocks})
print(json.dumps({"description": "Fixed ABBA pooled20/version; descriptive arithmetic, not provenance approval",
                  "all_six_no_median_slowdown": all(c["no_median_slowdown"] for c in cells),
                  "cells": cells}, indent=2))
