#!/usr/bin/env python3
"""Evaluate native-decline overhead in a sealed final-warm artifact.

The benchmark's ``accel_ms`` arm runs with pg_accel loaded and enabled. For a
production-declined row it therefore measures extension-enabled native
execution. The paired ``parallel_ms`` arm runs the same query with
``pg_accel.enabled=off``. This analyzer recomputes the descriptive P0A bounds
from those raw samples and binds every comparison to the report's sealed
no-dispatch audit.

The release contract predeclares an exact one-sided paired sign-flip
non-inferiority test at alpha 0.05. Its margin is the same frozen median rule
used by the descriptive gate: max(0.25ms, 2% of the disabled PostgreSQL
median). A pass also requires the descriptive median and p95 bounds, balanced
measured arm order, and a comparable sealed no-dispatch audit.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import sys
from pathlib import Path
from typing import Any


EXPECTED_ITERATIONS = 10
EXPECTED_WARMUPS = 5
SELECTED_SPEEDUP_FLOOR = 1.15
MEDIAN_ABSOLUTE_ALLOWANCE_MS = 0.25
MEDIAN_RELATIVE_ALLOWANCE = 0.02
P95_RELATIVE_ALLOWANCE = 0.05
NO_DISPATCH_TIMING_SKEW_THRESHOLD = 0.10
NON_INFERIORITY_ALPHA = 0.05
NON_INFERIORITY_METHOD = "exact_one_sided_paired_sign_flip"
STATISTICALLY_RESOLVED = "resolved_exact_paired_sign_flip"


class AnalysisError(Exception):
    """The artifact cannot establish the native-parity evidence contract."""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_manifest(path: Path) -> dict[str, str]:
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise AnalysisError(f"cannot read artifact manifest at {path}: {error}") from error

    entries: dict[str, str] = {}
    for line in lines:
        parts = line.split(None, 1)
        if len(parts) != 2 or len(parts[0]) != 64:
            raise AnalysisError(f"malformed SHA256SUMS line: {line!r}")
        relative = parts[1].lstrip("*")
        if relative.startswith("./"):
            relative = relative[2:]
        if not relative or relative.startswith("/") or ".." in Path(relative).parts:
            raise AnalysisError(f"unsafe SHA256SUMS path: {relative!r}")
        if relative in entries:
            raise AnalysisError(f"duplicate SHA256SUMS path: {relative!r}")
        entries[relative] = parts[0].lower()
    return entries


def verify_manifest_member(
    root: Path, manifest: dict[str, str], relative: str
) -> Path:
    expected = manifest.get(relative)
    if expected is None:
        raise AnalysisError(f"artifact manifest does not contain {relative}")
    path = root / relative
    if path.is_symlink() or not path.is_file():
        raise AnalysisError(f"artifact member is missing or not a regular file: {path}")
    if sha256(path) != expected:
        raise AnalysisError(f"artifact member hash mismatch: {relative}")
    return path


def load_json(path: Path, label: str) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise AnalysisError(f"cannot read {label} at {path}: {error}") from error


def read_matrix(path: Path) -> list[dict[str, str]]:
    try:
        with path.open(encoding="utf-8", newline="") as handle:
            rows = list(csv.DictReader(handle, delimiter="\t"))
    except OSError as error:
        raise AnalysisError(f"cannot read final matrix at {path}: {error}") from error
    if not rows:
        raise AnalysisError("final matrix is empty")
    if set(rows[0]) != {"ordinal", "workload", "rows", "expectation", "reason"}:
        raise AnalysisError("final matrix header does not match the evidence contract")
    return rows


def percentile(values: list[float], percent: float) -> float:
    if not values:
        raise AnalysisError("cannot compute a percentile of an empty sample")
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = min(max(percent / 100.0, 0.0), 1.0) * (len(ordered) - 1)
    low = math.floor(rank)
    high = math.ceil(rank)
    fraction = rank - low
    return ordered[low] + ((ordered[high] - ordered[low]) * fraction)


def _positive_number(value: Any, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise AnalysisError(f"{label} is not numeric")
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise AnalysisError(f"{label} must be finite and positive")
    return number


def extract_native_arms(
    row: dict[str, Any], label: str
) -> tuple[list[float], list[float], dict[str, Any]]:
    """Return extension-enabled native and extension-disabled PG samples."""

    if row.get("plan_selected") is not False:
        raise AnalysisError(f"{label}: selected/custom-scan row is not a native arm")
    if row.get("planner_declined") is not True:
        raise AnalysisError(f"{label}: production planner did not record a decline")
    if row.get("gpu_kernel_dispatched") is not False:
        raise AnalysisError(f"{label}: extension-enabled arm dispatched GPU work")
    if row.get("gpu_kernel_execution_delta") != 0:
        raise AnalysisError(f"{label}: extension-enabled arm changed the kernel counter")

    iterations = row.get("iterations")
    if not isinstance(iterations, list) or len(iterations) != EXPECTED_ITERATIONS:
        raise AnalysisError(
            f"{label}: expected exactly {EXPECTED_ITERATIONS} measured iterations"
        )
    warmups = row.get("warmup_iterations")
    if not isinstance(warmups, list) or len(warmups) != EXPECTED_WARMUPS:
        raise AnalysisError(f"{label}: expected exactly {EXPECTED_WARMUPS} warmups")

    enabled: list[float] = []
    disabled: list[float] = []
    measured_order: list[bool] = []
    for sample_kind, samples in (("warmup", warmups), ("measured", iterations)):
        for index, sample in enumerate(samples, start=1):
            if not isinstance(sample, dict):
                raise AnalysisError(f"{label}: {sample_kind} sample {index} is not an object")
            if (
                sample.get("cache_state") != "warm"
                or sample.get("cache_purge") != "not_requested"
            ):
                raise AnalysisError(
                    f"{label}: {sample_kind} sample {index} is not warm/unpurged"
                )
            accel_first = sample.get("accel_first")
            if not isinstance(accel_first, bool):
                raise AnalysisError(
                    f"{label}: {sample_kind} sample {index} has no typed arm order"
                )
            accel = _positive_number(
                sample.get("accel_ms"), f"{label}: {sample_kind} accel_ms[{index}]"
            )
            parallel = _positive_number(
                sample.get("parallel_ms"),
                f"{label}: {sample_kind} parallel_ms[{index}]",
            )
            if sample_kind == "measured":
                enabled.append(accel)
                disabled.append(parallel)
                measured_order.append(accel_first)

    accel_first_count = sum(measured_order)
    disabled_first_count = len(measured_order) - accel_first_count
    if accel_first_count != EXPECTED_ITERATIONS // 2 or disabled_first_count != EXPECTED_ITERATIONS // 2:
        raise AnalysisError(
            f"{label}: measured arm order is not exactly balanced "
            f"({accel_first_count} accel-first, {disabled_first_count} disabled-first)"
        )
    return enabled, disabled, {
        "contract": "balanced_randomized_measured_pairs",
        "accel_first_count": accel_first_count,
        "disabled_first_count": disabled_first_count,
        "sequence": [
            "accel_first" if accel_first else "disabled_first"
            for accel_first in measured_order
        ],
        "balanced": True,
    }


def _close(actual: Any, expected: float, label: str) -> None:
    number = _positive_number(actual, label)
    if not math.isclose(number, expected, rel_tol=1e-9, abs_tol=1e-9):
        raise AnalysisError(
            f"{label} disagrees with raw samples: reported={number}, recomputed={expected}"
        )


def validate_reported_native_statistics(
    row: dict[str, Any], enabled: list[float], disabled: list[float], label: str
) -> None:
    enabled_median = percentile(enabled, 50.0)
    enabled_p95 = percentile(enabled, 95.0)
    disabled_median = percentile(disabled, 50.0)
    disabled_p95 = percentile(disabled, 95.0)
    for field, expected in (
        ("accel_median_ms", enabled_median),
        ("accel_p95_ms", enabled_p95),
        ("parallel_median_ms", disabled_median),
        ("parallel_p95_ms", disabled_p95),
        ("speedup_median_vs_parallel", disabled_median / enabled_median),
    ):
        _close(row.get(field), expected, f"{label}: {field}")


def validate_no_dispatch_audit(
    audit: Any,
    *,
    workload: str,
    rows: int,
    speedup_median: float,
    label: str,
) -> dict[str, Any]:
    if not isinstance(audit, dict) or audit.get("schema_version") != 1:
        raise AnalysisError(f"{label}: no-dispatch audit is missing or has unknown schema")
    threshold = audit.get("timing_skew_threshold_fraction")
    if (
        isinstance(threshold, bool)
        or not isinstance(threshold, (int, float))
        or not math.isfinite(float(threshold))
        or not math.isclose(
            float(threshold),
            NO_DISPATCH_TIMING_SKEW_THRESHOLD,
            rel_tol=0.0,
            abs_tol=1e-12,
        )
    ):
        raise AnalysisError(f"{label}: no-dispatch audit timing-skew threshold changed")
    audit_rows = audit.get("rows")
    if not isinstance(audit_rows, list) or len(audit_rows) != 1:
        raise AnalysisError(f"{label}: no-dispatch audit must contain exactly one row")
    item = audit_rows[0]
    if not isinstance(item, dict):
        raise AnalysisError(f"{label}: no-dispatch audit row is malformed")
    if item.get("workload") != workload or item.get("rows") != rows:
        raise AnalysisError(f"{label}: no-dispatch audit identity mismatch")
    _close(
        item.get("speedup_median_vs_parallel"),
        speedup_median,
        f"{label}: audited median speedup",
    )

    status = item.get("status")
    if not isinstance(status, str):
        raise AnalysisError(f"{label}: no-dispatch audit status is missing")
    expected_skew_fraction = (
        speedup_median - 1.0
        if speedup_median >= 1.0
        else (1.0 / speedup_median) - 1.0
    )
    skew_fraction = item.get("timing_skew_fraction")
    if (
        isinstance(skew_fraction, bool)
        or not isinstance(skew_fraction, (int, float))
        or not math.isfinite(float(skew_fraction))
        or float(skew_fraction) < 0.0
        or not math.isclose(
            float(skew_fraction), expected_skew_fraction, rel_tol=1e-9, abs_tol=1e-9
        )
    ):
        raise AnalysisError(f"{label}: no-dispatch audit timing skew disagrees with raw samples")
    timing_skew = item.get("timing_skew")
    plan_mismatch = item.get("plan_mismatch")
    missing_plan = item.get("missing_plan_evidence")
    if not all(isinstance(value, bool) for value in (timing_skew, plan_mismatch, missing_plan)):
        raise AnalysisError(f"{label}: no-dispatch audit flags are malformed")
    expected_timing_skew = (
        expected_skew_fraction >= NO_DISPATCH_TIMING_SKEW_THRESHOLD
    )
    if timing_skew is not expected_timing_skew:
        raise AnalysisError(f"{label}: no-dispatch audit timing-skew flag is inconsistent")
    expected_status = (
        "timing_skew_and_plan_mismatch"
        if timing_skew and plan_mismatch
        else "plan_mismatch"
        if plan_mismatch
        else "timing_skew"
        if timing_skew
        else "missing_plan_evidence"
        if missing_plan
        else "comparable_native"
    )
    if status != expected_status:
        raise AnalysisError(f"{label}: no-dispatch audit status is inconsistent with its flags")

    accel_signature = item.get("accel_plan_signature")
    baseline_signature = item.get("baseline_plan_signature")
    signatures_match = (
        isinstance(accel_signature, str)
        and bool(accel_signature)
        and accel_signature != "-"
        and accel_signature == baseline_signature
    )
    comparable = (
        status in {"comparable_native", "timing_skew"}
        and not plan_mismatch
        and not missing_plan
        and signatures_match
    )

    warning = status != "comparable_native"
    expected_counts = {
        "evaluated_no_dispatch_rows": 1,
        "clean_rows": 0 if warning else 1,
        "warning_rows": 1 if warning else 0,
        "timing_skew_rows": int(timing_skew),
        "plan_mismatch_rows": int(plan_mismatch),
        "selected_custom_scan_not_dispatched_rows": int(
            status == "selected_custom_scan_not_dispatched"
        ),
        "missing_plan_evidence_rows": int(missing_plan),
        "ignored_dispatching_rows": 0,
    }
    for field, expected in expected_counts.items():
        if audit.get(field) != expected:
            raise AnalysisError(
                f"{label}: no-dispatch audit {field}={audit.get(field)!r}, expected {expected}"
            )
    return {
        "schema_version": 1,
        "status": status,
        "comparable_native_plans": comparable,
        "accel_plan_signature": accel_signature,
        "disabled_plan_signature": baseline_signature,
    }


def exact_paired_sign_flip_non_inferiority(
    enabled_native_ms: list[float],
    disabled_postgresql_ms: list[float],
    margin_ms: float,
) -> dict[str, Any]:
    """Return the exact lower-tail sign-flip test against the NI margin.

    The paired residual is ``enabled - disabled - margin``. Under the sharp
    boundary null, its signs are exchangeable. Enumerating all 2^n sign
    assignments gives an exact one-sided p-value for the alternative that the
    extension-enabled native arm is below the allowed loss.
    """

    if len(enabled_native_ms) != len(disabled_postgresql_ms):
        raise AnalysisError("native-parity arms are not paired")
    if not enabled_native_ms:
        raise AnalysisError("native-parity arms are empty")
    if len(enabled_native_ms) > 20:
        raise AnalysisError("exact sign-flip test is limited to 20 paired samples")

    adjusted = [
        enabled - disabled - margin_ms
        for enabled, disabled in zip(
            enabled_native_ms, disabled_postgresql_ms, strict=True
        )
    ]
    observed_sum = sum(adjusted)
    permutation_count = 1 << len(adjusted)
    tolerance = max(1.0, abs(observed_sum), *(abs(value) for value in adjusted)) * 1e-12
    lower_tail_count = 0
    for assignment in range(permutation_count):
        permuted_sum = sum(
            value if assignment & (1 << index) else -value
            for index, value in enumerate(adjusted)
        )
        if permuted_sum <= observed_sum + tolerance:
            lower_tail_count += 1
    p_value = lower_tail_count / permutation_count
    return {
        "method": NON_INFERIORITY_METHOD,
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


def evaluate_native_parity(
    enabled_native_ms: list[float], disabled_postgresql_ms: list[float]
) -> dict[str, Any]:
    """Evaluate the predeclared P0A descriptive and statistical bounds."""

    if len(enabled_native_ms) != len(disabled_postgresql_ms):
        raise AnalysisError("native-parity arms are not paired")
    if not enabled_native_ms:
        raise AnalysisError("native-parity arms are empty")
    enabled = [_positive_number(value, "enabled-native sample") for value in enabled_native_ms]
    disabled = [
        _positive_number(value, "disabled-PostgreSQL sample")
        for value in disabled_postgresql_ms
    ]

    enabled_median = percentile(enabled, 50.0)
    disabled_median = percentile(disabled, 50.0)
    enabled_p95 = percentile(enabled, 95.0)
    disabled_p95 = percentile(disabled, 95.0)
    median_delta = enabled_median - disabled_median
    p95_delta = enabled_p95 - disabled_p95
    median_delta_pct = (median_delta / disabled_median) * 100.0
    p95_delta_pct = (p95_delta / disabled_p95) * 100.0
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

    failure_reasons = []
    if not median_pass:
        failure_reasons.append("median_bound_exceeded")
    if not p95_pass:
        failure_reasons.append("p95_bound_exceeded")
    if not non_inferiority["pass"]:
        failure_reasons.append("non_inferiority_not_established")
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
            "delta_percent": median_delta_pct,
            "allowance_ms": median_allowance,
            "allowance_rule": "max(0.25ms, 2% of disabled PostgreSQL median)",
            "bounds_pass": median_pass,
        },
        "p95": {
            "delta_ms": p95_delta,
            "delta_percent": p95_delta_pct,
            "allowance_ms": p95_allowance,
            "allowance_percent": P95_RELATIVE_ALLOWANCE * 100.0,
            "bounds_pass": p95_pass,
        },
        "descriptive_bounds_pass": bounds_pass,
        "non_inferiority": non_inferiority,
        "statistical_resolution": STATISTICALLY_RESOLVED,
        "parity_pass": parity_pass,
        "parity_verdict": "pass" if parity_pass else "fail",
        "failure_reasons": failure_reasons,
    }


def analyze_native_cell(
    row: dict[str, Any],
    audit: Any,
    *,
    ordinal: str,
    workload: str,
    rows: int,
    expected_reason: str,
) -> dict[str, Any]:
    label = f"{ordinal}:{workload}@{rows}"
    if row.get("name") != workload or row.get("rows") != rows:
        raise AnalysisError(f"{label}: report identity mismatch")
    decline = row.get("native_decline_evidence")
    if not isinstance(decline, dict):
        raise AnalysisError(f"{label}: typed native-decline evidence is missing")
    if decline.get("source") != "planner_reported" or decline.get("reason") != expected_reason:
        raise AnalysisError(f"{label}: planner-reported decline reason mismatch")

    enabled, disabled, arm_order = extract_native_arms(row, label)
    validate_reported_native_statistics(row, enabled, disabled, label)
    parity = evaluate_native_parity(enabled, disabled)
    audit_result = validate_no_dispatch_audit(
        audit,
        workload=workload,
        rows=rows,
        speedup_median=parity["disabled_postgresql"]["median_ms"]
        / parity["enabled_native"]["median_ms"],
        label=label,
    )
    if not audit_result["comparable_native_plans"]:
        parity["failure_reasons"].insert(0, "no_dispatch_audit_not_comparable")
        parity["parity_pass"] = False
        parity["parity_verdict"] = "fail"
    parity["no_dispatch_audit"] = audit_result
    parity["arm_order"] = arm_order
    parity["ordinal"] = ordinal
    parity["workload"] = workload
    parity["rows"] = rows
    parity["decline_reason"] = expected_reason
    return parity


def _load_report_row(path: Path, label: str) -> dict[str, Any]:
    report = load_json(path, f"{label} report")
    if not isinstance(report, dict) or report.get("crashes") != []:
        raise AnalysisError(f"{label}: report is malformed or contains crashes")
    workloads = report.get("workloads")
    if not isinstance(workloads, list) or len(workloads) != 1:
        raise AnalysisError(f"{label}: report must contain exactly one workload")
    row = workloads[0]
    if not isinstance(row, dict):
        raise AnalysisError(f"{label}: workload row is malformed")
    return row


def analyze_artifact(root: Path) -> dict[str, Any]:
    if not root.is_dir():
        raise AnalysisError(f"artifact root is missing: {root}")
    manifest = read_manifest(root / "SHA256SUMS")
    matrix_path = verify_manifest_member(root, manifest, "matrix.tsv")
    verification_path = verify_manifest_member(root, manifest, "verification.json")
    verification = load_json(verification_path, "final-warm verification")
    if not isinstance(verification, dict) or verification.get("verdict") != "pass":
        raise AnalysisError("existing final-warm verification did not pass")
    if verification.get("performance_floor") != SELECTED_SPEEDUP_FLOOR:
        raise AnalysisError("existing selected-path speedup floor is not 1.15x")
    summary = verification.get("summary")
    if not isinstance(summary, dict):
        raise AnalysisError("existing final-warm verification summary is missing")
    selected = summary.get("selected_gpu_cells")
    selected_at_floor = summary.get("selected_at_or_above_1_15_cells")
    if not isinstance(selected, int) or selected <= 0 or selected_at_floor != selected:
        raise AnalysisError("existing selected-path 1.15x gate is incomplete")

    matrix = read_matrix(matrix_path)
    seen: set[tuple[str, str, int]] = set()
    cells: list[dict[str, Any]] = []
    selected_rows = 0
    for matrix_row in matrix:
        ordinal = matrix_row["ordinal"]
        workload = matrix_row["workload"]
        try:
            rows = int(matrix_row["rows"])
        except ValueError as error:
            raise AnalysisError(f"{ordinal}:{workload}: row scale is not an integer") from error
        key = (ordinal, workload, rows)
        if key in seen:
            raise AnalysisError(f"duplicate final-matrix identity: {key!r}")
        seen.add(key)
        expectation = matrix_row["expectation"]
        if expectation == "gpu_winner":
            selected_rows += 1
            continue
        if expectation != "native_decline":
            raise AnalysisError(f"{ordinal}:{workload}@{rows}: unknown expectation")
        reason = matrix_row["reason"]
        if not reason or reason == "-":
            raise AnalysisError(f"{ordinal}:{workload}@{rows}: decline reason is missing")

        cell_relative = f"cells/{ordinal}-{workload}-{rows}"
        report_path = verify_manifest_member(root, manifest, f"{cell_relative}/report.json")
        audit_path = verify_manifest_member(
            root, manifest, f"{cell_relative}/no_dispatch_audit.json"
        )
        row = _load_report_row(report_path, f"{ordinal}:{workload}@{rows}")
        audit = load_json(audit_path, f"{ordinal}:{workload}@{rows} no-dispatch audit")
        cells.append(
            analyze_native_cell(
                row,
                audit,
                ordinal=ordinal,
                workload=workload,
                rows=rows,
                expected_reason=reason,
            )
        )

    if selected_rows != selected:
        raise AnalysisError("matrix selected count disagrees with final-warm verification")
    if len(cells) != summary.get("native_decline_cells"):
        raise AnalysisError("matrix native-decline count disagrees with final-warm verification")
    if not cells:
        raise AnalysisError("final matrix contains no native-decline sentinels")

    bounds_pass = sum(cell["descriptive_bounds_pass"] for cell in cells)
    comparable = sum(
        cell["no_dispatch_audit"]["comparable_native_plans"] for cell in cells
    )
    parity_pass = sum(cell["parity_pass"] for cell in cells)
    return {
        "schema_version": 2,
        "kind": "resident_v2_native_decline_parity_analysis",
        "analysis_status": "valid",
        "parity_verdict": "pass" if parity_pass == len(cells) else "fail",
        "statistical_resolution": STATISTICALLY_RESOLVED,
        "contract": {
            "median_allowance": "max(0.25ms, 2% of disabled PostgreSQL median)",
            "p95_allowance_percent": P95_RELATIVE_ALLOWANCE * 100.0,
            "selected_speedup_floor_preserved": SELECTED_SPEEDUP_FLOOR,
            "non_inferiority_method": NON_INFERIORITY_METHOD,
            "non_inferiority_alpha": NON_INFERIORITY_ALPHA,
            "measured_arm_order": "exactly 5 accel-first and 5 disabled-first pairs",
            "parity_requires_comparable_no_dispatch_audit": True,
        },
        "summary": {
            "native_decline_cells": len(cells),
            "descriptive_bounds_pass_cells": bounds_pass,
            "descriptive_bounds_fail_cells": len(cells) - bounds_pass,
            "comparable_no_dispatch_audit_cells": comparable,
            "non_inferiority_pass_cells": sum(
                cell["non_inferiority"]["pass"] for cell in cells
            ),
            "non_inferiority_fail_cells": sum(
                not cell["non_inferiority"]["pass"] for cell in cells
            ),
            "parity_pass_cells": parity_pass,
            "parity_fail_cells": len(cells) - parity_pass,
        },
        "cells": cells,
    }


def invalid_report(error: Exception) -> dict[str, Any]:
    return {
        "schema_version": 2,
        "kind": "resident_v2_native_decline_parity_analysis",
        "analysis_status": "invalid",
        "parity_verdict": "fail",
        "errors": [str(error)],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--root",
        type=Path,
        required=True,
        help="completed and sealed final-warm artifact root",
    )
    args = parser.parse_args()
    try:
        report = analyze_artifact(args.root.resolve())
    except (AnalysisError, OSError, TypeError, ValueError) as error:
        json.dump(invalid_report(error), sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")
        return 2
    json.dump(report, sys.stdout, indent=2, sort_keys=True, allow_nan=False)
    sys.stdout.write("\n")
    return 0 if report["parity_verdict"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
