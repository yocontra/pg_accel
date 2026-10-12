#!/usr/bin/env python3
"""Regression coverage for the optimized release Mach-O stripping workaround."""

import tomllib
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class ReleaseProfileTests(unittest.TestCase):
    def test_release_disables_rust_postlink_stripping(self) -> None:
        with (REPO_ROOT / "Cargo.toml").open("rb") as manifest:
            release = tomllib.load(manifest).get("profile", {}).get("release", {})
        self.assertEqual(release.get("strip"), "none")

    def test_release_preserves_inherited_optimization_settings(self) -> None:
        with (REPO_ROOT / "Cargo.toml").open("rb") as manifest:
            release = tomllib.load(manifest).get("profile", {}).get("release", {})
        # Before the stripping workaround, release had no profile overrides.
        self.assertEqual({key: value for key, value in release.items() if key != "strip"}, {})


if __name__ == "__main__":
    unittest.main()
