#!/usr/bin/env python3
"""Analyze a strengthened native-parity diagnostic with report-derived sample size."""

from __future__ import annotations

import argparse
import bisect
import csv
import json
import math
from pathlib import Path
from typing import Any

try:
    from .final_warm_native_parity import (
        AnalysisError,
        MEDIAN_ABSOLUTE_ALLOWANCE_MS,
        MEDIAN_RELATIVE_ALLOWANCE,
        NON_INFERIORITY_ALPHA,
        NON_INFERIORITY_METHOD,
        P95_RELATIVE_ALLOWANCE,
        _load_report_row,
        _positive_number,
        extract_native_arms as extract_mirrored_native_arms,
        load_json,
        percentile,
        validate_no_dispatch_audit,
        validate_reported_native_statistics,
    )
except ImportError:  # Direct script execution has no package context.
    from final_warm_native_parity import (
        AnalysisError,
        MEDIAN_ABSOLUTE_ALLOWANCE_MS,
        MEDIAN_RELATIVE_ALLOWANCE,
        NON_INFERIORITY_ALPHA,
        NON_INFERIORITY_METHOD,
        P95_RELATIVE_ALLOWANCE,
        _load_report_row,
        _positive_number,
        extract_native_arms as extract_mirrored_native_arms,
        load_json,
        percentile,
        validate_no_dispatch_audit,
        validate_reported_native_statistics,
    )

EXPECTED_WARMUPS = 5
EXPECTED_PAIRS = 30
ENUMERATION_STRATEGY = "meet_in_the_middle_exact_count"


def _signed_sums(values: list[float]) -> list[float]:
    sums = [0.0]
    for value in values:
        sums = [total - value for total in sums] + [total + value for total in sums]
    return sums


def exact_paired_sign_flip_non_inferiority(
    enabled_native_ms: list[float],
    disabled_postgresql_ms: list[float],
    margin_ms: float,
) -> dict[str, Any]:
    """Count the exact sign-flip distribution using two half-sample sets."""

    if len(enabled_native_ms) != len(disabled_postgresql_ms):
        raise AnalysisError("native-parity arms are not paired")
    if not enabled_native_ms:
        raise AnalysisError("native-parity arms are empty")
    adjusted = [
        enabled - disabled - margin_ms
        for enabled, disabled in zip(
            enabled_native_ms, disabled_postgresql_ms, strict=True
        )
    ]
    observed_sum = sum(adjusted)
    tolerance = max(1.0, abs(observed_sum), *(abs(value) for value in adjusted)) * 1e-12
    midpoint = len(adjusted) // 2
    left = _signed_sums(adjusted[:midpoint])
    right = sorted(_signed_sums(adjusted[midpoint:]))
    threshold = observed_sum + tolerance
    lower_tail_count = sum(
        bisect.bisect_right(right, threshold - left_sum) for left_sum in left
    )
    permutation_count = 1 << len(adjusted)
    p_value = lower_tail_count / permutation_count
    return {
        "method": NON_INFERIORITY_METHOD,
        "enumeration_strategy": ENUMERATION_STRATEGY,
        "alternative": "enabled_native_minus_disabled_postgresql_below_margin",
        "alpha": NON_INFERIORITY_ALPHA,
        "margin_ms": margin_ms,
        "paired_sample_count": len(adjusted),
        "observed_adjusted_mean_ms": observed_sum / len(adjusted),
        "permutation_count": permutation_count,
        "lower_tail_count": lower_tail_count,
        "p_value": p_value,
        "pass": p_value <= NON_INFERIORITY_ALPHA,
    }


def extract_native_arms(row: dict[str, Any], label: str) -> tuple[list[float], list[float], dict[str, Any]]:
    if row.get("plan_selected") is not False or row.get("planner_declined") is not True:
        raise AnalysisError(f"{label}: row is not a production native decline")
    if row.get("gpu_kernel_dispatched") is not False or row.get("gpu_kernel_execution_delta") != 0:
        raise AnalysisError(f"{label}: native arm dispatched GPU work")
    iterations = row.get("iterations")
    warmups = row.get("warmup_iterations")
    if not isinstance(iterations, list) or not iterations or len(iterations) % 2:
        raise AnalysisError(f"{label}: measured pair count must be positive and even")
    if not isinstance(warmups, list) or len(warmups) != EXPECTED_WARMUPS:
        raise AnalysisError(f"{label}: expected exactly {EXPECTED_WARMUPS} warmups")

    enabled: list[float] = []
    disabled: list[float] = []
    order: list[str] = []
    for index, sample in enumerate(iterations, start=1):
        if not isinstance(sample, dict):
            raise AnalysisError(f"{label}: measured pair {index} is malformed")
        if sample.get("cache_state") != "warm" or sample.get("cache_purge") != "not_requested":
            raise AnalysisError(f"{label}: measured pair {index} is not warm/unpurged")
        accel_first = sample.get("accel_first")
        if not isinstance(accel_first, bool):
            raise AnalysisError(f"{label}: measured pair {index} has no typed arm order")
        enabled.append(_positive_number(sample.get("accel_ms"), f"{label}: accel_ms[{index}]"))
        disabled.append(_positive_number(sample.get("parallel_ms"), f"{label}: parallel_ms[{index}]"))
        order.append("accel_first" if accel_first else "disabled_first")
    accel_first_count = order.count("accel_first")
    if accel_first_count != len(iterations) // 2:
        raise AnalysisError(f"{label}: measured arm order is not exactly balanced")
    return enabled, disabled, {
        "contract": "balanced_randomized_measured_pairs",
        "paired_sample_count": len(iterations),
        "accel_first_count": accel_first_count,
        "disabled_first_count": len(iterations) - accel_first_count,
        "sequence": order,
        "balanced": True,
    }


def evaluate_native_parity(enabled: list[float], disabled: list[float]) -> dict[str, Any]:
    enabled_median = percentile(enabled, 50.0)
    disabled_median = percentile(disabled, 50.0)
    enabled_p95 = percentile(enabled, 95.0)
    disabled_p95 = percentile(disabled, 95.0)
    median_delta = enabled_median - disabled_median
    p95_delta = enabled_p95 - disabled_p95
    median_allowance = max(
        MEDIAN_ABSOLUTE_ALLOWANCE_MS,
        disabled_median * MEDIAN_RELATIVE_ALLOWANCE,
    )
    p95_allowance = disabled_p95 * P95_RELATIVE_ALLOWANCE
    non_inferiority = exact_paired_sign_flip_non_inferiority(
        enabled, disabled, median_allowance
    )
    median_pass = median_delta <= median_allowance
    p95_pass = p95_delta <= p95_allowance
    bounds_pass = median_pass and p95_pass
    parity_pass = bounds_pass and non_inferiority["pass"]
    failures = []
    if not median_pass:
        failures.append("median_bound_exceeded")
    if not p95_pass:
        failures.append("p95_bound_exceeded")
    if not non_inferiority["pass"]:
        failures.append("non_inferiority_not_established")
    return {
        "enabled_native": {
            "sample_count": len(enabled),
            "median_ms": enabled_median,
            "p95_ms": enabled_p95,
        },
        "disabled_postgresql": {
            "sample_count": len(disabled),
            "median_ms": disabled_median,
            "p95_ms": disabled_p95,
        },
        "median": {
            "delta_ms": median_delta,
            "delta_percent": (median_delta / disabled_median) * 100.0,
            "allowance_ms": median_allowance,
            "allowance_rule": "max(0.25ms, 2% of disabled PostgreSQL median)",
            "bounds_pass": median_pass,
        },
        "p95": {
            "delta_ms": p95_delta,
            "delta_percent": (p95_delta / disabled_p95) * 100.0,
            "allowance_ms": p95_allowance,
            "allowance_percent": P95_RELATIVE_ALLOWANCE * 100.0,
            "bounds_pass": p95_pass,
        },
        "descriptive_bounds_pass": bounds_pass,
        "non_inferiority": non_inferiority,
        "parity_pass": parity_pass,
        "parity_verdict": "pass" if parity_pass else "fail",
        "failure_reasons": failures,
    }


def analyze_cell(root: Path, matrix_row: dict[str, str]) -> dict[str, Any]:
    ordinal = matrix_row["ordinal"]
    workload = matrix_row["workload"]
    rows = int(matrix_row["rows"])
    label = f"{ordinal}:{workload}@{rows}"
    cell = root / "cells" / f"{ordinal}-{workload}-{rows}"
    row = _load_report_row(cell / "report.json", label)
    context = load_json(
        cell / "pre_risk_contexts" / f"{workload}-{rows}.json",
        f"{label} pre-risk context",
    )
    if not isinstance(context, dict) or context.get("capture_planner_stages") is not False:
        raise AnalysisError(f"{label}: planner-stage profiling was not disabled")
    if context.get("native_parity_pairing") is not True:
        raise AnalysisError(f"{label}: pre-risk context does not prove same-backend pairing")
    if row.get("planner_stage_captures") not in (None, []):
        raise AnalysisError(f"{label}: production report contains planner-stage captures")
    decline = row.get("native_decline_evidence")
    if not isinstance(decline, dict) or decline.get("source") != "planner_reported":
        raise AnalysisError(f"{label}: typed planner decline is missing")
    expected_reason = matrix_row.get("reason")
    if expected_reason and decline.get("reason") != expected_reason:
        raise AnalysisError(
            f"{label}: decline reason {decline.get('reason')!r} does not match "
            f"the registered reason {expected_reason!r}"
        )

    enabled, disabled, order = extract_mirrored_native_arms(row, label)
    validate_reported_native_statistics(row, enabled, disabled, label)
    result = evaluate_native_parity(enabled, disabled)
    audit = load_json(cell / "no_dispatch_audit.json", f"{label} no-dispatch audit")
    audit_result = validate_no_dispatch_audit(
        audit,
        workload=workload,
        rows=rows,
        speedup_median=result["disabled_postgresql"]["median_ms"]
        / result["enabled_native"]["median_ms"],
        label=label,
    )
    if not audit_result["comparable_native_plans"]:
        result["failure_reasons"].insert(0, "no_dispatch_audit_not_comparable")
        result["parity_pass"] = False
        result["parity_verdict"] = "fail"
    result.update(
        ordinal=ordinal,
        workload=workload,
        rows=rows,
        decline_reason=decline.get("reason"),
        arm_order=order,
        no_dispatch_audit=audit_result,
    )
    return result


def analyze_artifact(root: Path) -> dict[str, Any]:
    with (root / "matrix.tsv").open(encoding="utf-8", newline="") as handle:
        matrix = list(csv.DictReader(handle, delimiter="\t"))
    required_columns = {"ordinal", "workload", "rows", "cohort"}
    if not matrix or not required_columns.issubset(matrix[0]):
        raise AnalysisError("strengthened matrix is missing or malformed")
    cells = [analyze_cell(root, row) for row in matrix]
    sample_counts = {cell["arm_order"]["paired_sample_count"] for cell in cells}
    if len(sample_counts) != 1:
        raise AnalysisError("strengthened cells do not share one report-derived sample count")
    pair_count = sample_counts.pop()
    if pair_count != EXPECTED_PAIRS:
        raise AnalysisError(
            f"strengthened cells contain {pair_count} pairs; expected {EXPECTED_PAIRS}"
        )
    return {
        "schema_version": 1,
        "kind": "strengthened_native_parity_diagnostic",
        "profiling": False,
        "contract": {
            "paired_sample_count": pair_count,
            "sample_count_source": "sealed_report_iterations",
            "warmup_count": EXPECTED_WARMUPS,
            "median_allowance": "max(0.25ms, 2% of disabled PostgreSQL median)",
            "p95_allowance_percent": P95_RELATIVE_ALLOWANCE * 100.0,
            "non_inferiority_method": NON_INFERIORITY_METHOD,
            "non_inferiority_enumeration_strategy": ENUMERATION_STRATEGY,
            "non_inferiority_alpha": NON_INFERIORITY_ALPHA,
            "measured_arm_order": "exactly balanced randomized pairs",
        },
        "summary": {
            "cell_count": len(cells),
            "descriptive_bounds_pass_count": sum(cell["descriptive_bounds_pass"] for cell in cells),
            "non_inferiority_pass_count": sum(cell["non_inferiority"]["pass"] for cell in cells),
            "parity_pass_count": sum(cell["parity_pass"] for cell in cells),
        },
        "cells": cells,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = analyze_artifact(args.artifact.resolve())
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output is None:
        print(rendered, end="")
    else:
        args.output.write_text(rendered, encoding="utf-8")
    return 0 if result["summary"]["parity_pass_count"] == result["summary"]["cell_count"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
