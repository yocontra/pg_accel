#!/usr/bin/env python3
"""Tests for repo-local PostgreSQL install validation."""

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
PG_SOURCE = REPO_ROOT / "scripts/pg_source.sh"


class PgSourceInstallTests(unittest.TestCase):
    def run_function(self, function: str, argument: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; "$2" "$3"',
                "bash",
                str(PG_SOURCE),
                function,
                argument,
            ],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    def run_probe(self, pg_config: Path, platform: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "bash",
                "-c",
                'source "$1"; pg_accel_pg_install_is_usable "$2" "$3"',
                "bash",
                str(PG_SOURCE),
                str(pg_config),
                platform,
            ],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )

    @staticmethod
    def write_pg_config(root: Path, cppflags: str) -> Path:
        pg_config = root / "pg_config"
        pg_config.write_text(
            "#!/usr/bin/env bash\n"
            'if [ "${1:-}" = "--cppflags" ]; then\n'
            f"    printf '%s\\n' {cppflags!r}\n"
            "fi\n",
            encoding="utf-8",
        )
        pg_config.chmod(0o755)
        return pg_config

    def test_existing_macos_sysroot_is_usable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sdk = root / "MacOSX.sdk"
            sdk.mkdir()
            pg_config = self.write_pg_config(root, f"-O2 -isysroot {sdk}")

            result = self.run_probe(pg_config, "Darwin")

            self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_macos_sysroot_is_stale(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            missing = root / "Missing.sdk"
            pg_config = self.write_pg_config(root, f"-isysroot{missing}")

            result = self.run_probe(pg_config, "Darwin")

            self.assertEqual(result.returncode, 1)
            self.assertIn("references missing macOS SDK", result.stderr)

    def test_non_macos_install_does_not_require_an_apple_sdk(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pg_config = self.write_pg_config(root, "-isysroot /missing/MacOSX.sdk")

            result = self.run_probe(pg_config, "Linux")

            self.assertEqual(result.returncode, 0, result.stderr)

    def test_missing_pg_config_is_not_usable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_probe(Path(directory) / "pg_config", "Darwin")

            self.assertEqual(result.returncode, 1)

    def test_release_version_major_is_parsed(self) -> None:
        result = self.run_function("pg_accel_pg_major_from_version", "18.4")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "18\n")

    def test_beta_version_major_is_parsed(self) -> None:
        result = self.run_function("pg_accel_pg_major_from_version", "19beta1")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "19\n")

    def test_release_candidate_major_is_parsed(self) -> None:
        result = self.run_function("pg_accel_pg_major_from_version", "pg19rc2")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "19\n")

    def test_invalid_version_is_rejected(self) -> None:
        result = self.run_function("pg_accel_pg_major_from_version", "beta1")

        self.assertEqual(result.returncode, 1)
        self.assertIn("invalid PostgreSQL version", result.stderr)


if __name__ == "__main__":
    unittest.main()
