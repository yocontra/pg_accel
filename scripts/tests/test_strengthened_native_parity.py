import unittest

from scripts.final_warm_native_parity import (
    AnalysisError,
    exact_paired_sign_flip_non_inferiority as exhaustive,
)
from scripts.strengthened_native_parity import (
    exact_paired_sign_flip_non_inferiority as strengthened,
    extract_native_arms,
    validate_setup_quiescence,
)


class StrengthenedNativeParityTests(unittest.TestCase):
    @staticmethod
    def quiescence_audit() -> dict:
        snapshot = {
            "num_timed": 10,
            "num_requested": 20,
            "num_done": 30,
            "restartpoints_timed": 0,
            "restartpoints_requested": 0,
            "restartpoints_done": 0,
            "write_time_ms": 40.5,
            "sync_time_ms": 5.25,
            "buffers_written": 100,
            "slru_written": 2,
        }
        return {
            "schema_version": 1,
            "checkpoint_completed": True,
            "checkpoint_completed_at": "2026-08-22 00:00:00+00",
            "checkpoint_wal_lsn": "2F9/443DD40",
            "measurement_completed_at": "2026-08-22 00:01:00+00",
            "measurement_completed_wal_lsn": "2F9/443DD40",
            "checkpointer_before": dict(snapshot),
            "checkpointer_after": dict(snapshot),
            "checkpointer_unchanged": True,
        }

    def test_meet_in_the_middle_matches_exhaustive_count(self) -> None:
        enabled = [1.1, 1.2, 1.0, 1.4, 0.9, 1.3, 1.1, 1.2, 1.0, 1.1]
        disabled = [1.0] * len(enabled)
        expected = exhaustive(enabled, disabled, 0.25)
        actual = strengthened(enabled, disabled, 0.25)
        self.assertEqual(actual["lower_tail_count"], expected["lower_tail_count"])
        self.assertEqual(actual["permutation_count"], expected["permutation_count"])
        self.assertEqual(actual["p_value"], expected["p_value"])

    def test_thirty_pair_exact_count_and_report_derived_balance(self) -> None:
        iterations = [
            {
                "accel_ms": 1.0,
                "parallel_ms": 1.0,
                "accel_first": index % 2 == 0,
                "cache_state": "warm",
                "cache_purge": "not_requested",
            }
            for index in range(30)
        ]
        row = {
            "plan_selected": False,
            "planner_declined": True,
            "gpu_kernel_dispatched": False,
            "gpu_kernel_execution_delta": 0,
            "iterations": iterations,
            "warmup_iterations": [{} for _ in range(5)],
        }
        enabled, disabled, order = extract_native_arms(row, "test")
        self.assertEqual(order["paired_sample_count"], 30)
        self.assertEqual(order["accel_first_count"], 15)
        self.assertEqual(order["disabled_first_count"], 15)
        result = strengthened(enabled, disabled, 0.25)
        self.assertEqual(result["permutation_count"], 1 << 30)
        self.assertEqual(result["lower_tail_count"], 1)
        self.assertIs(result["pass"], True)

    def test_setup_quiescence_requires_identical_checkpointer_snapshots(self) -> None:
        audit = self.quiescence_audit()
        self.assertEqual(
            validate_setup_quiescence({"setup_quiescence": audit}, "test"),
            audit,
        )

        changed = self.quiescence_audit()
        changed["checkpointer_after"]["buffers_written"] += 1
        with self.assertRaisesRegex(AnalysisError, "checkpointer changed"):
            validate_setup_quiescence({"setup_quiescence": changed}, "test")

    def test_setup_quiescence_fails_closed_on_missing_or_claim_only_evidence(self) -> None:
        with self.assertRaisesRegex(AnalysisError, "audit is missing"):
            validate_setup_quiescence({}, "test")

        claimed = self.quiescence_audit()
        claimed["checkpointer_unchanged"] = False
        with self.assertRaisesRegex(AnalysisError, "verdict is not a pass"):
            validate_setup_quiescence({"setup_quiescence": claimed}, "test")


if __name__ == "__main__":
    unittest.main()
