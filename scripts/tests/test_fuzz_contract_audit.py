from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "fuzz_contract_audit.py"
MANIFEST = REPO_ROOT / "coverage" / "fuzz-contracts.json"
SPEC = importlib.util.spec_from_file_location("fuzz_contract_audit", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
fuzz = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fuzz)


def live_manifest() -> dict[str, object]:
    return json.loads(MANIFEST.read_text(encoding="utf-8"))


class FuzzContractAuditTests(unittest.TestCase):
    def test_live_manifest_registers_every_required_target(self) -> None:
        result = fuzz.audit(REPO_ROOT, live_manifest())
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["target_count"], len(fuzz.REQUIRED_TARGETS))
        self.assertGreaterEqual(result["minimum_case_count"], 10_000)

    def test_missing_target_fails_closed(self) -> None:
        manifest = live_manifest()
        manifest["targets"] = manifest["targets"][:-1]
        with self.assertRaisesRegex(fuzz.AuditError, "inventory drift"):
            fuzz.audit(REPO_ROOT, manifest)

    def test_post_allocation_rejection_contract_fails_closed(self) -> None:
        manifest = copy.deepcopy(live_manifest())
        manifest["targets"][0]["fails_before_allocation_or_dereference"] = False
        with self.assertRaisesRegex(fuzz.AuditError, "pre-allocation/pre-dereference"):
            fuzz.audit(REPO_ROOT, manifest)

    def test_renamed_test_symbol_fails_closed(self) -> None:
        manifest = copy.deepcopy(live_manifest())
        manifest["targets"][0]["tests"][0]["symbol"] = "missing_test_symbol"
        with self.assertRaisesRegex(fuzz.AuditError, "test symbol"):
            fuzz.audit(REPO_ROOT, manifest)

    def test_seed_must_be_fixed_lowercase_u64(self) -> None:
        manifest = live_manifest()
        manifest["seed"] = "random"
        with self.assertRaisesRegex(fuzz.AuditError, "fixed lowercase 64-bit"):
            fuzz.audit(REPO_ROOT, manifest)


if __name__ == "__main__":
    unittest.main()
