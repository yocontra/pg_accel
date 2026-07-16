from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
AUDIT_SCRIPT = REPO_ROOT / "scripts" / "release_checklist_audit.sh"
TRACKED_CHECKLIST = REPO_ROOT / "docs" / "release-checklist-1.0.md"


class ReleaseChecklistAuditTests(unittest.TestCase):
    def run_audit(self, evidence_path: Path | None = None) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env.pop("RELEASE_CHECKLIST_EVIDENCE_PATH", None)
        if evidence_path is not None:
            env["RELEASE_CHECKLIST_EVIDENCE_PATH"] = str(evidence_path)
        return subprocess.run(
            ["bash", str(AUDIT_SCRIPT)],
            cwd=REPO_ROOT,
            env=env,
            check=False,
            capture_output=True,
            text=True,
        )

    def test_external_completed_ledger_passes(self) -> None:
        checklist = TRACKED_CHECKLIST.read_text(encoding="utf-8")
        checklist = checklist.replace("- [ ]", "- [x]")
        replacements = {
            "<sha-or-url>": "https://example.invalid/evidence/sha-or-url",
            "<release-url>": "https://example.invalid/releases/v1.0.0",
            "<url>": "https://example.invalid/evidence/url",
            "<sha>": "0123456789abcdef0123456789abcdef01234567",
            "<name>": "Release Maintainer",
        }
        for placeholder, evidence in replacements.items():
            checklist = checklist.replace(placeholder, evidence)

        with tempfile.TemporaryDirectory() as directory:
            ledger = Path(directory) / "tag-pr-checklist.md"
            ledger.write_text(checklist, encoding="utf-8")
            completed = self.run_audit(ledger)

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn(f"release checklist audit: PASS ({ledger})", completed.stdout)

    def test_default_tracked_template_remains_red(self) -> None:
        completed = self.run_audit()

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("release checklist audit: FAIL", completed.stderr)
        self.assertIn("unchecked item(s)", completed.stderr)
        self.assertIn("placeholder evidence token(s)", completed.stderr)

    def test_missing_external_ledger_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            missing = Path(directory) / "missing-checklist.md"
            completed = self.run_audit(missing)

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("evidence path must name a readable regular file", completed.stderr)
        self.assertIn(str(missing), completed.stderr)


if __name__ == "__main__":
    unittest.main()
