#!/usr/bin/env python3
"""Contract tests for deterministic third-party extension setup."""

from __future__ import annotations

import subprocess
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts/setup_pg_extensions.sh"


class SetupPgExtensionsTests(unittest.TestCase):
    def test_server_soft_file_limit_is_raised_without_lowering_or_hard_limit_changes(self) -> None:
        probe = r'''
source scripts/pg_versions.sh
test_limit="$1"
test_raise_fails="$2"
ulimit() {
    if [ "$#" = 1 ] && [ "$1" = -Sn ]; then
        printf '%s\n' "$test_limit"
    elif [ "$#" = 2 ] && [ "$1" = -Sn ] && [ "$2" = 4096 ]; then
        [ "$test_raise_fails" = no ] || return 1
        test_limit="$2"
    else
        echo 'unexpected hard-limit or resource modification' >&2
        return 99
    fi
}
pg_accel_prepare_server_file_limit
printf '%s\n' "$test_limit"
'''
        for initial, expected in (("256", "4096"), ("1024", "4096"),
                                  ("4096", "4096"), ("8192", "8192"),
                                  ("unlimited", "unlimited")):
            with self.subTest(initial=initial):
                result = subprocess.run(
                    ["bash", "-c", probe, "probe", initial, "no"],
                    cwd=REPO_ROOT, text=True, capture_output=True, check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), expected)
        failed = subprocess.run(
            ["bash", "-c", probe, "probe", "256", "yes"],
            cwd=REPO_ROOT, text=True, capture_output=True, check=False,
        )
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn("requires at least 4096 open files", failed.stderr)
        self.assertEqual(failed.stdout, "")

    def test_install_prepares_inherited_limit_before_server_mutation(self) -> None:
        source = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")
        recipe = source.split('install-pg-accel pg="":', 1)[1].split("\n#", 1)[0]
        gate = recipe.index("pg_accel_prepare_server_file_limit")
        self.assertLess(gate, recipe.index("scripts/setup_pg_extensions.sh"))
        self.assertLess(gate, recipe.index("cargo pgrx stop"))
        self.assertLess(gate, recipe.index("cargo pgrx start"))
        self.assertIn('open_files_soft=$(ulimit -Sn)', recipe)

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
