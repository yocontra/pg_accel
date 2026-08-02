import unittest

from scripts.final_warm_native_parity import (
    exact_paired_sign_flip_non_inferiority as exhaustive,
)
from scripts.strengthened_native_parity import (
    exact_paired_sign_flip_non_inferiority as strengthened,
    extract_native_arms,
)


class StrengthenedNativeParityTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
