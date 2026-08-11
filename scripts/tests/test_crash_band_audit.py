from __future__ import annotations

import copy
import importlib.util
import json
from pathlib import Path
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "crash_band_audit.py"
MANIFEST = REPO_ROOT / "coverage" / "crash-bands.json"
SPEC = importlib.util.spec_from_file_location("crash_band_audit", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
crash = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(crash)


def live_manifest() -> dict[str, object]:
    return json.loads(MANIFEST.read_text(encoding="utf-8"))


class CrashBandAuditTests(unittest.TestCase):
    def test_live_contract_passes(self) -> None:
        result = crash.audit(REPO_ROOT, live_manifest())
        self.assertEqual(result["status"], "pass")
        self.assertFalse(result["row_returning_join_planner_selectable"])

    def test_missing_lane_fails_closed(self) -> None:
        manifest = live_manifest()
        manifest["historical_lanes"] = manifest["historical_lanes"][:-1]
        with self.assertRaisesRegex(crash.AuditError, "inventory drift"):
            crash.audit(REPO_ROOT, manifest)

    def test_raised_threshold_fails_closed(self) -> None:
        manifest = copy.deepcopy(live_manifest())
        manifest["historical_lanes"][0]["first_unsafe_rows"] = 100_001
        with self.assertRaisesRegex(crash.AuditError, "first unsafe row changed"):
            crash.audit(REPO_ROOT, manifest)

    def test_missing_evidence_symbol_fails_closed(self) -> None:
        manifest = copy.deepcopy(live_manifest())
        manifest["required_evidence"][0]["symbol"] = "missing_crash_band_evidence"
        with self.assertRaisesRegex(crash.AuditError, "evidence symbol"):
            crash.audit(REPO_ROOT, manifest)


if __name__ == "__main__":
    unittest.main()
