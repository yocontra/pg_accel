from __future__ import annotations

import argparse
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "risk_coverage_audit.py"
SPEC = importlib.util.spec_from_file_location("risk_coverage_audit", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
risk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(risk)


class RiskCoverageAuditTests(unittest.TestCase):
    def test_live_registry_maps_every_production_unsafe_site(self) -> None:
        args = argparse.Namespace(
            repo_root=REPO_ROOT,
            registry=REPO_ROOT / "coverage" / "risk-register.json",
            baseline=REPO_ROOT / "coverage" / "release-baseline.json",
            rust_lcov=None,
            cpp_lcov=None,
            minimum_unsafe_percent=90.0,
        )
        result = risk.audit(args)
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["domain_count"], len(risk.REQUIRED_DOMAINS))
        self.assertGreater(result["unsafe_site_count"], 0)

    def test_unmapped_unsafe_site_fails_closed(self) -> None:
        with self.assertRaisesRegex(risk.AuditError, "unregistered production unsafe"):
            risk._validate_unsafe_mapping(
                [("pg_accel/src/unmapped.rs", 7)],
                [{"glob": "pg_accel/src/gpu/**", "domain": "unsafe_ffi"}],
            )

    def test_unsafe_coverage_below_threshold_fails_closed(self) -> None:
        sites = [("pg_accel/src/example.rs", 10), ("pg_accel/src/example.rs", 20)]
        lcov = {"pg_accel/src/example.rs": {10: 1, 20: 0}}
        with self.assertRaisesRegex(risk.AuditError, "below 90.000%"):
            risk._validate_unsafe_coverage(sites, lcov, 90.0)

    def test_candidate_worktree_paths_normalize_to_repository_paths(self) -> None:
        raw = "/tmp/candidate/worktree/pg_accel/src/engine/residency/store.rs"
        self.assertEqual(
            risk._repo_path(raw),
            "pg_accel/src/engine/residency/store.rs",
        )

    def test_malformed_lcov_record_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bad.info"
            path.write_text(
                "SF:/tmp/work/pg_accel/src/lib.rs\nDA:not-a-line,1\nend_of_record\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(risk.AuditError, "malformed LCOV"):
                risk._parse_lcov(path)


if __name__ == "__main__":
    unittest.main()
