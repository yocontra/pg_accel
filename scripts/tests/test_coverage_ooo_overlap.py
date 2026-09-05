from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
RUNNER = REPO_ROOT / "scripts" / "coverage_ooo_overlap.sh"


class CoverageOooOverlapTests(unittest.TestCase):
    def run_mode(self, mode: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            profiles = root / "profiles"
            profiles.mkdir()
            binary = root / "fake-overlap"
            binary.write_text(
                """#!/usr/bin/env bash
set -euo pipefail
host_profile="${LLVM_PROFILE_FILE//%p/$$}"
host_profile="${host_profile//%m/fake}"
if [ "$FAKE_OVERLAP_MODE" != "missing-profiles" ]; then
    printf 'host-profile\n' > "$host_profile"
    printf 'device-profile\n' > "$ACPP_METAL_DEVICE_PROFILE_DIR/fake.proftext"
fi
case "$FAKE_OVERLAP_MODE" in
    success)
        printf 'span_ms reduce=1 resident=1 final=1 spans_overlap=yes improved=yes\n'
        printf 'test_ooo_overlap: OK\n'
        exit 0
        ;;
    no-overlap)
        printf 'span_ms reduce=1 resident=1 final=2 spans_overlap=no improved=no\n'
        printf 'test_ooo_overlap: resident/reduce GPU spans did not overlap\n'
        exit 1
        ;;
    malformed-success)
        printf 'test_ooo_overlap: OK\n'
        exit 0
        ;;
    malformed-failure)
        printf 'unrelated failure\n'
        exit 1
        ;;
    contradictory-failure)
        printf 'span_ms reduce=1 resident=1 final=1 spans_overlap=yes improved=yes\n'
        printf 'test_ooo_overlap: resident/reduce GPU spans did not overlap\n'
        exit 1
        ;;
    overflow)
        printf 'overflow\n' > "$ACPP_METAL_DEVICE_PROFILE_DIR/fake.overflow"
        printf 'span_ms reduce=1 resident=1 final=1 spans_overlap=yes improved=yes\n'
        printf 'test_ooo_overlap: OK\n'
        exit 0
        ;;
    missing-profiles)
        printf 'span_ms reduce=1 resident=1 final=1 spans_overlap=yes improved=yes\n'
        printf 'test_ooo_overlap: OK\n'
        exit 0
        ;;
    *)
        exit 2
        ;;
esac
""",
                encoding="utf-8",
            )
            binary.chmod(0o755)
            env = os.environ.copy()
            env["FAKE_OVERLAP_MODE"] = mode
            return subprocess.run(
                ["bash", str(RUNNER), str(binary), str(profiles)],
                cwd=REPO_ROOT,
                env=env,
                text=True,
                capture_output=True,
                check=False,
            )

    def test_accepts_proven_improved_overlap(self) -> None:
        result = self.run_mode("success")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("PASS (real-overlap success", result.stdout)

    def test_accepts_exact_known_no_overlap_result(self) -> None:
        result = self.run_mode("no-overlap")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("PASS (expected no-overlap structural result", result.stdout)

    def test_rejects_unproven_success_and_unexpected_failure(self) -> None:
        for mode in (
            "malformed-success",
            "malformed-failure",
            "contradictory-failure",
            "other",
        ):
            with self.subTest(mode=mode):
                result = self.run_mode(mode)
                self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_rejects_missing_or_overflowed_profiles(self) -> None:
        for mode in ("missing-profiles", "overflow"):
            with self.subTest(mode=mode):
                result = self.run_mode(mode)
                self.assertNotEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
