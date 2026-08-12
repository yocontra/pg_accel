#!/usr/bin/env python3
"""Contract tests for deterministic third-party extension setup."""

from __future__ import annotations

import subprocess
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts/setup_pg_extensions.sh"


class SetupPgExtensionsTests(unittest.TestCase):
    def test_shell_is_syntactically_valid(self) -> None:
        result = subprocess.run(
            ["bash", "-n", str(SCRIPT)],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_postgis_source_fallback_avoids_generated_sql_race(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        start = source.index("build_postgis_from_source()")
        postgis = source[start : source.index("\ncollect_packaged_dirs\n", start)]
        self.assertIn('--prefix="$prefix"', postgis)
        self.assertIn("--without-topology", postgis)
        self.assertIn("make -j 1", postgis)
        self.assertNotIn('make -j "$jobs"', postgis)


if __name__ == "__main__":
    unittest.main()
