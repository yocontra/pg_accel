#!/usr/bin/env python3
"""Focused tests for the final-warm native-decline parity gate."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "final_warm_native_parity.py"
SPEC = importlib.util.spec_from_file_location("final_warm_native_parity", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
ANALYZER = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ANALYZER
SPEC.loader.exec_module(ANALYZER)
RUNNER = SCRIPT.parent / "run_native_parity_p0.sh"


def samples(value: float) -> list[float]:
    return [value] * ANALYZER.EXPECTED_ITERATIONS


def native_row(*, accel: float = 10.0, parallel: float = 10.0) -> dict[str, object]:
    def sample(accel_first: bool) -> dict[str, object]:
        return {
            "accel_ms": accel,
            "parallel_ms": parallel,
            "accel_first": accel_first,
            "cache_state": "warm",
            "cache_purge": "not_requested",
        }

    def stage_vector(*, calls: int) -> list[dict[str, object]]:
        return [
            {
                "stage": stage,
                "calls": calls,
                "elapsed_us": calls * 3,
                "fast_declines": calls,
            }
            for stage in ANALYZER.PLANNER_STAGE_NAMES
        ]

    def substage_vector(*, calls: int) -> list[dict[str, object]]:
        return [
            {
                "substage": substage,
                "calls": calls,
                "elapsed_us": calls * 2,
            }
            for substage in ANALYZER.PLANNER_SUBSTAGE_NAMES
        ]

    iterations = [
        sample(index < ANALYZER.EXPECTED_ITERATIONS // 2)
        for index in range(ANALYZER.EXPECTED_ITERATIONS)
    ]
    raw_captures = []
    for index, iteration in enumerate(iterations):
        accel_first = iteration["accel_first"]
        raw_captures.append(
            {
                "pair_index": index,
                "accel_first": accel_first,
                "sequence": (
                    ["accel", "disabled_postgresql", "disabled_postgresql", "accel"]
                    if accel_first
                    else [
                        "disabled_postgresql",
                        "accel",
                        "accel",
                        "disabled_postgresql",
                    ]
                )
                * (ANALYZER.EXPECTED_REPETITIONS_PER_ARM // 2),
                "accel_ms": [accel] * ANALYZER.EXPECTED_REPETITIONS_PER_ARM,
                "parallel_ms": [parallel] * ANALYZER.EXPECTED_REPETITIONS_PER_ARM,
                "accel_average_ms": accel,
                "parallel_average_ms": parallel,
            }
        )

    return {
        "plan_selected": False,
        "planner_declined": True,
        "gpu_kernel_dispatched": False,
        "gpu_kernel_execution_delta": 0,
        "iterations": iterations,
        "native_parity_pair_captures": raw_captures,
        "warmup_iterations": [sample(index % 2 == 0) for index in range(5)],
        "planner_stage_captures": [
            {
                "pair_index": index,
                "cache_state": "warm",
                "stages": stage_vector(calls=1),
                "observer_probe": stage_vector(calls=0),
                "substages": substage_vector(calls=1),
                "substage_observer_probe": substage_vector(calls=0),
            }
            for index in range(ANALYZER.EXPECTED_ITERATIONS)
        ],
    }


def audit(*, workload: str = "decline", rows: int = 1000) -> dict[str, object]:
    return {
        "schema_version": 1,
        "timing_skew_threshold_fraction": 0.1,
        "evaluated_no_dispatch_rows": 1,
        "clean_rows": 1,
        "warning_rows": 0,
        "timing_skew_rows": 0,
        "plan_mismatch_rows": 0,
        "selected_custom_scan_not_dispatched_rows": 0,
        "missing_plan_evidence_rows": 0,
        "ignored_dispatching_rows": 0,
        "rows": [
            {
                "workload": workload,
                "rows": rows,
                "speedup_median_vs_parallel": 1.0,
                "timing_skew_fraction": 0.0,
                "timing_skew": False,
                "plan_mismatch": False,
                "missing_plan_evidence": False,
                "accel_plan_signature": "aggregate | seq scan",
                "baseline_plan_signature": "aggregate | seq scan",
                "status": "comparable_native",
                "action": "no GPU credit; use only as native-decline/stability evidence",
            }
        ],
    }


class RunnerContractTests(unittest.TestCase):
    def test_runner_requires_clean_head_tree_before_creating_evidence(self) -> None:
        runner = RUNNER.read_text(encoding="utf-8")
        self.assertIn("require_exact_candidate()", runner)
        self.assertIn(
            "status --porcelain=v1 --untracked-files=all --ignore-submodules=none",
            runner,
        )
        self.assertIn("printf 'git_head_tree=%s", runner)
        self.assertLess(
            runner.index("require_exact_candidate\n"),
            runner.index('mkdir -p "$artifact_root/cells"'),
        )


class BoundTests(unittest.TestCase):
    def test_submillisecond_median_uses_absolute_allowance(self) -> None:
        result = ANALYZER.evaluate_native_parity(samples(1.24), samples(1.0))
        self.assertEqual(result["median"]["allowance_ms"], 0.25)
        self.assertTrue(result["median"]["bounds_pass"])
        self.assertTrue(result["non_inferiority"]["pass"])
        self.assertFalse(result["parity_pass"])
        self.assertEqual(
            result["non_inferiority"]["permutation_count"],
            1 << ANALYZER.EXPECTED_ITERATIONS,
        )
        self.assertEqual(
            result["non_inferiority"]["enumeration_strategy"],
            "meet_in_the_middle_exact_count",
        )
        self.assertAlmostEqual(
            result["non_inferiority"]["p_value"],
            1 / (1 << ANALYZER.EXPECTED_ITERATIONS),
        )

    def test_direct_and_meet_in_the_middle_are_both_exact(self) -> None:
        direct = ANALYZER.exact_paired_sign_flip_non_inferiority(
            [9.0] * 20, [10.0] * 20, 0.25
        )
        meet_in_middle = ANALYZER.exact_paired_sign_flip_non_inferiority(
            [9.0] * 30, [10.0] * 30, 0.25
        )
        self.assertEqual(direct["enumeration_strategy"], "direct_exact_enumeration")
        self.assertEqual(direct["lower_tail_count"], 1)
        self.assertEqual(direct["permutation_count"], 1 << 20)
        self.assertEqual(
            meet_in_middle["enumeration_strategy"],
            "meet_in_the_middle_exact_count",
        )
        self.assertEqual(meet_in_middle["lower_tail_count"], 1)
        self.assertEqual(meet_in_middle["permutation_count"], 1 << 30)

    def test_large_median_uses_two_percent_allowance(self) -> None:
        result = ANALYZER.evaluate_native_parity(samples(102.0), samples(100.0))
        self.assertEqual(result["median"]["allowance_ms"], 2.0)
        self.assertTrue(result["median"]["bounds_pass"])
        self.assertAlmostEqual(result["median"]["delta_percent"], 2.0)

    def test_p95_over_five_percent_fails(self) -> None:
        enabled = [100.0] * 9 + [110.0]
        disabled = [100.0] * 10
        result = ANALYZER.evaluate_native_parity(enabled, disabled)
        self.assertFalse(result["p95"]["bounds_pass"])
        self.assertFalse(result["descriptive_bounds_pass"])
        self.assertIn("p95_bound_exceeded", result["failure_reasons"])

    def test_clear_improvement_passes_non_inferiority(self) -> None:
        result = ANALYZER.evaluate_native_parity(samples(9.0), samples(10.0))
        self.assertTrue(result["descriptive_bounds_pass"])
        self.assertEqual(result["parity_verdict"], "pass")
        self.assertTrue(result["parity_pass"])

    def test_margin_boundary_does_not_establish_non_inferiority(self) -> None:
        result = ANALYZER.evaluate_native_parity(samples(10.25), samples(10.0))
        self.assertTrue(result["descriptive_bounds_pass"])
        self.assertEqual(result["non_inferiority"]["p_value"], 1.0)
        self.assertFalse(result["non_inferiority"]["pass"])
        self.assertIn("non_inferiority_not_established", result["failure_reasons"])


class ArmTests(unittest.TestCase):
    def test_missing_arm_is_rejected(self) -> None:
        row = native_row()
        del row["iterations"][0]["parallel_ms"]  # type: ignore[index]
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.extract_native_arms(row, "fixture")


class PlannerStageTests(unittest.TestCase):
    def test_valid_capture_is_aggregated_by_stage(self) -> None:
        attribution = ANALYZER.extract_planner_stage_attribution(native_row(), "fixture")
        self.assertEqual(
            attribution["measured_pair_count"], ANALYZER.EXPECTED_ITERATIONS
        )
        self.assertEqual(len(attribution["stages"]), 7)
        self.assertTrue(
            all(
                stage["calls"] == ANALYZER.EXPECTED_ITERATIONS
                for stage in attribution["stages"]
            )
        )
        self.assertEqual(len(attribution["substages"]), 5)
        self.assertTrue(
            all(
                substage["calls"] == ANALYZER.EXPECTED_ITERATIONS
                for substage in attribution["substages"]
            )
        )

    def test_missing_or_duplicate_stage_evidence_is_rejected(self) -> None:
        row = native_row()
        del row["planner_stage_captures"][0]["stages"][-1]  # type: ignore[index]
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.extract_planner_stage_attribution(row, "fixture")

        row = native_row()
        stages = row["planner_stage_captures"][0]["stages"]  # type: ignore[index]
        stages[-1]["stage"] = stages[0]["stage"]
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.extract_planner_stage_attribution(row, "fixture")

    def test_observer_probe_must_remain_zero(self) -> None:
        row = native_row()
        row["planner_stage_captures"][0]["observer_probe"][0]["calls"] = 1  # type: ignore[index]
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.extract_planner_stage_attribution(row, "fixture")

        row = native_row()
        row["planner_stage_captures"][0]["substage_observer_probe"][0]["calls"] = 1  # type: ignore[index]
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.extract_planner_stage_attribution(row, "fixture")

    def test_missing_substage_evidence_is_rejected(self) -> None:
        row = native_row()
        del row["planner_stage_captures"][0]["substages"][-1]  # type: ignore[index]
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.extract_planner_stage_attribution(row, "fixture")

    def test_cell_analysis_requires_stages_only_in_explicit_diagnostic_mode(self) -> None:
        row = native_row()
        del row["planner_stage_captures"]
        row.update(
            {
                "name": "decline",
                "rows": 1000,
                "native_decline_evidence": {
                    "source": "planner_reported",
                    "reason": "expected_reason",
                },
                "accel_median_ms": 10.0,
                "accel_p95_ms": 10.0,
                "parallel_median_ms": 10.0,
                "parallel_p95_ms": 10.0,
                "speedup_median_vs_parallel": 1.0,
            }
        )
        standard = ANALYZER.analyze_native_cell(
            row,
            audit(),
            ordinal="01",
            workload="decline",
            rows=1000,
            expected_reason="expected_reason",
        )
        self.assertNotIn("planner_stage_attribution", standard)
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.analyze_native_cell(
                row,
                audit(),
                ordinal="01",
                workload="decline",
                rows=1000,
                expected_reason="expected_reason",
                require_planner_stages=True,
            )

    def test_selected_arm_is_rejected(self) -> None:
        row = native_row()
        row["plan_selected"] = True
        row["planner_declined"] = False
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.extract_native_arms(row, "fixture")

    def test_measured_order_must_be_typed_and_balanced(self) -> None:
        row = native_row()
        del row["iterations"][0]["accel_first"]  # type: ignore[index]
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.extract_native_arms(row, "fixture")

        row = native_row()
        for sample in row["iterations"]:  # type: ignore[union-attr]
            sample["accel_first"] = True
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.extract_native_arms(row, "fixture")


class AuditBindingTests(unittest.TestCase):
    def test_no_dispatch_audit_binds_identity_speedup_and_plan(self) -> None:
        result = ANALYZER.validate_no_dispatch_audit(
            audit(),
            workload="decline",
            rows=1000,
            speedup_median=1.0,
            label="fixture",
        )
        self.assertTrue(result["comparable_native_plans"])
        self.assertEqual(result["status"], "comparable_native")

        changed = audit()
        changed["rows"][0]["workload"] = "other"  # type: ignore[index]
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.validate_no_dispatch_audit(
                changed,
                workload="decline",
                rows=1000,
                speedup_median=1.0,
                label="fixture",
            )

        changed = audit()
        changed["timing_skew_threshold_fraction"] = 0.2
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.validate_no_dispatch_audit(
                changed,
                workload="decline",
                rows=1000,
                speedup_median=1.0,
                label="fixture",
            )

        changed = audit()
        changed["rows"][0]["status"] = "timing_skew"  # type: ignore[index]
        with self.assertRaises(ANALYZER.AnalysisError):
            ANALYZER.validate_no_dispatch_audit(
                changed,
                workload="decline",
                rows=1000,
                speedup_median=1.0,
                label="fixture",
            )

    def test_manifest_hash_binds_no_dispatch_audit(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            relative = "cells/01-decline-1000/no_dispatch_audit.json"
            path = root / relative
            path.parent.mkdir(parents=True)
            path.write_text("{}\n", encoding="utf-8")
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            manifest = {relative: digest}
            self.assertEqual(
                ANALYZER.verify_manifest_member(root, manifest, relative), path
            )
            path.write_text('{"tampered":true}\n', encoding="utf-8")
            with self.assertRaises(ANALYZER.AnalysisError):
                ANALYZER.verify_manifest_member(root, manifest, relative)

    def test_sealed_artifact_requires_selected_floor_and_emits_resolved_pass(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            matrix = (
                "ordinal\tworkload\trows\texpectation\treason\n"
                "01\twinner\t1000\tgpu_winner\t-\n"
                "02\tdecline\t1000\tnative_decline\texpected_reason\n"
            )
            (root / "matrix.tsv").write_text(matrix, encoding="utf-8")
            verification = {
                "schema_version": 2,
                "verdict": "pass",
                "performance_floor": 1.15,
                "summary": {
                    "selected_gpu_cells": 1,
                    "selected_at_or_above_1_15_cells": 1,
                    "native_decline_cells": 1,
                },
            }
            (root / "verification.json").write_text(
                json.dumps(verification), encoding="utf-8"
            )
            cell = root / "cells/02-decline-1000"
            cell.mkdir(parents=True)
            row = native_row()
            row.update(
                {
                    "name": "decline",
                    "rows": 1000,
                    "native_decline_evidence": {
                        "source": "planner_reported",
                        "reason": "expected_reason",
                    },
                    "accel_median_ms": 10.0,
                    "accel_p95_ms": 10.0,
                    "parallel_median_ms": 10.0,
                    "parallel_p95_ms": 10.0,
                    "speedup_median_vs_parallel": 1.0,
                }
            )
            (cell / "report.json").write_text(
                json.dumps(
                    {
                        "crashes": [],
                        "methodology": {
                            "iterations": ANALYZER.EXPECTED_ITERATIONS,
                            "warmup": ANALYZER.EXPECTED_WARMUPS,
                            "native_parity_pairing": True,
                            "native_parity_repetitions_per_arm": ANALYZER.EXPECTED_REPETITIONS_PER_ARM,
                        },
                        "workloads": [row],
                    }
                ),
                encoding="utf-8",
            )
            (cell / "no_dispatch_audit.json").write_text(
                json.dumps(audit()), encoding="utf-8"
            )
            members = [
                "matrix.tsv",
                "verification.json",
                "cells/02-decline-1000/report.json",
                "cells/02-decline-1000/no_dispatch_audit.json",
            ]
            (root / "SHA256SUMS").write_text(
                "".join(
                    f"{hashlib.sha256((root / member).read_bytes()).hexdigest()}  ./{member}\n"
                    for member in members
                ),
                encoding="utf-8",
            )

            result = ANALYZER.analyze_artifact(root)
            self.assertEqual(result["analysis_status"], "valid")
            self.assertEqual(result["parity_verdict"], "pass")
            self.assertEqual(result["summary"]["descriptive_bounds_pass_cells"], 1)
            self.assertEqual(result["summary"]["non_inferiority_pass_cells"], 1)


if __name__ == "__main__":
    unittest.main()
