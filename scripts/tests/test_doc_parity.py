#!/usr/bin/env python3
"""Adversarial tests for the strict documentation parity gate."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "doc_parity.py"
SPEC = importlib.util.spec_from_file_location("doc_parity", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
doc_parity = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = doc_parity
SPEC.loader.exec_module(doc_parity)


class CitationParityTests(unittest.TestCase):
    def test_shorthand_path_is_not_resolved(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "pg_accel/src").mkdir(parents=True)
            (root / "pg_accel/src/mod.rs").write_text("source\n")
            (root / "CLAUDE.md").write_text("See `mod.rs:1`.\n")

            result = doc_parity.audit_citations(root, ("CLAUDE.md",))

            self.assertTrue(any("no shorthand resolution" in error for error in result.errors))

    def test_eof_plus_one_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("one\ntwo\n")
            (root / "README.md").write_text("See `src/lib.rs:3`.\n")

            result = doc_parity.audit_citations(root, ("README.md",))

            self.assertTrue(any("exactly 2 lines" in error for error in result.errors))

    def test_range_end_is_validated(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src").mkdir()
            (root / "src/lib.rs").write_text("one\ntwo\n")
            (root / "README.md").write_text("See `src/lib.rs:1-4`.\n")

            result = doc_parity.audit_citations(root, ("README.md",))

            self.assertTrue(any("exactly 2 lines" in error for error in result.errors))


class GucParityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.specs, cls.source_errors = doc_parity.parse_released_gucs(doc_parity.REPO_ROOT)
        cls.readme_table, cls.table_errors = doc_parity.parse_guc_table(
            (doc_parity.REPO_ROOT / "README.md").read_text()
        )

    def test_source_inventory_is_complete_and_excludes_test_gucs(self) -> None:
        self.assertEqual(self.source_errors, [])
        self.assertEqual(len(self.specs), 17)
        self.assertIn("pg_accel.execution_profiling", self.specs)
        self.assertFalse(any(name.startswith("pg_accel.test_") for name in self.specs))

    def test_missing_released_guc_fails(self) -> None:
        table = dict(self.readme_table)
        table.pop("pg_accel.auto_load")

        errors = doc_parity.validate_guc_table("fixture.md", table, self.specs)

        self.assertTrue(any("pg_accel.auto_load" in error and "missing" in error for error in errors))

    def test_wrong_source_metadata_fails(self) -> None:
        table = dict(self.readme_table)
        value_type, _default, context, value_range, effect = table["pg_accel.enabled"]
        table["pg_accel.enabled"] = (value_type, "off", context, value_range, effect)

        errors = doc_parity.validate_guc_table("fixture.md", table, self.specs)

        self.assertTrue(any("does not match source" in error for error in errors))

    def test_missing_effect_semantics_fails(self) -> None:
        table = dict(self.readme_table)
        value_type, default, context, value_range, _effect = table["pg_accel.kernel_timeout_ms"]
        table["pg_accel.kernel_timeout_ms"] = (
            value_type,
            default,
            context,
            value_range,
            "Cancels slow kernels.",
        )

        errors = doc_parity.validate_guc_table("fixture.md", table, self.specs)

        self.assertTrue(any("semantic marker" in error for error in errors))


class CapabilityParityTests(unittest.TestCase):
    def test_planner_status_drift_fails(self) -> None:
        table = dict(doc_parity.EXPECTED_CAPABILITIES)
        table["Window"] = ("Present", "Selectable")

        errors = doc_parity.validate_capability_table(table)

        self.assertTrue(any("Window" in error and "does not match" in error for error in errors))

    def test_missing_capability_fails(self) -> None:
        table = dict(doc_parity.EXPECTED_CAPABILITIES)
        table.pop("Raster")

        errors = doc_parity.validate_capability_table(table)

        self.assertTrue(any("Raster" in error and "missing" in error for error in errors))

    def test_adapter_inventory_drift_fails(self) -> None:
        expected = {"PostGIS": {"st_intersects"}, "H3 scalar": {"h3_latlng_to_cell"}}
        documented = {"PostGIS": {"st_intersects"}, "H3 scalar": set()}

        errors = doc_parity.validate_adapter_table(documented, expected)

        self.assertTrue(any("does not match constructors" in error for error in errors))

    def test_current_source_capability_contract_passes(self) -> None:
        errors, capability_count, adapter_count = doc_parity.audit_capabilities(
            doc_parity.REPO_ROOT
        )

        self.assertEqual(errors, [])
        self.assertEqual(capability_count, 11)
        self.assertEqual(adapter_count, 3)


class MacosPrerequisiteParityTests(unittest.TestCase):
    def test_current_prerequisite_contract_passes(self) -> None:
        self.assertEqual(doc_parity.audit_macos_prerequisites(doc_parity.REPO_ROOT), [])

    def test_formula_drift_fails(self) -> None:
        canonical = "brew install " + " ".join(
            doc_parity.MACOS_HOMEBREW_PREREQUISITES
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for document in doc_parity.MACOS_PREREQUISITE_DOCS:
                (root / document).write_text(canonical + "\n")
            (root / "README.md").write_text(
                canonical.replace("lld@20", "lld") + "\n"
            )

            errors = doc_parity.audit_macos_prerequisites(root)

        self.assertEqual(len(errors), 1)
        self.assertIn("README.md", errors[0])
        self.assertIn("lld@20", errors[0])

    def test_extra_formula_fails(self) -> None:
        canonical = "brew install " + " ".join(
            doc_parity.MACOS_HOMEBREW_PREREQUISITES
        )
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for document in doc_parity.MACOS_PREREQUISITE_DOCS:
                (root / document).write_text(canonical + "\n")
            (root / "CHANGELOG.md").write_text(canonical + " ninja\n")

            errors = doc_parity.audit_macos_prerequisites(root)

        self.assertEqual(len(errors), 1)
        self.assertIn("CHANGELOG.md", errors[0])
        self.assertIn("ninja", errors[0])

    def test_setup_prefers_lld20_and_recommends_it(self) -> None:
        setup = (doc_parity.REPO_ROOT / "scripts/setup_acpp.sh").read_text()
        for executable in ("ld64.lld", "lld"):
            versioned = f'"$brew_root/opt/lld@20/bin/{executable}"'
            unversioned = f'"$brew_root/opt/lld/bin/{executable}"'
            self.assertIn(versioned, setup)
            self.assertIn(unversioned, setup)
            self.assertLess(setup.index(versioned), setup.index(unversioned))
        self.assertIn("install Homebrew lld@20 or set ACPP_LLD_PATH", setup)

    def test_setup_forces_headers_matching_the_macos_sdk_runtime(self) -> None:
        setup = (doc_parity.REPO_ROOT / "scripts/setup_acpp.sh").read_text()
        self.assertIn('ACPP_MACOS_SDK_PATH="$(xcrun --show-sdk-path)"', setup)
        self.assertIn(
            '"-DCMAKE_CXX_FLAGS=-nostdinc++ -isystem '
            '$ACPP_MACOS_SDK_PATH/usr/include/c++/v1"',
            setup,
        )
        self.assertIn('"${ACPP_REQUIRED_CMAKE_ARGS[@]}"', setup)
        self.assertLess(
            setup.index('${ACPP_CMAKE_FLAGS:-} "${ACPP_REQUIRED_CMAKE_ARGS[@]}"'),
            setup.index('cmake --build "$ACPP_BUILD_DIR"'),
        )

    def test_soft_fp_2_pin_and_metal_contract_are_consistent(self) -> None:
        setup = (doc_parity.REPO_ROOT / "scripts/setup_acpp.sh").read_text()
        self.assertIn(
            'SOFT_FP64_REQUIRED_TAG="${SOFT_FP64_REQUIRED_TAG:-v2.0.1}"',
            setup,
        )
        for option in (
            "-DSOFT_FP_BUILD_FP128=OFF",
            "-DSOFT_FP_BUILD_FP256=OFF",
            "-DSOFT_FP64_OCL=on",
            "-DSOFT_FP64_FTZ=off",
            "-DSOFT_FP64_FENV=disabled",
            "-DSOFT_FP64_SNAN=quiet",
        ):
            self.assertIn(option, setup)
        self.assertIn("apply_soft_fp_2_package_patch", setup)
        self.assertNotIn("apply_soft_fp64_device_patch", setup)
        self.assertNotIn("SF64_DEVICE_CONSTEXPR_BITCAST", setup)
        self.assertNotIn("soft_fp64_device_patch=", setup)
        self.assertFalse(
            any((doc_parity.REPO_ROOT / "patches/soft-fp").glob("*.patch"))
        )
        self.assertIn("soft_fp_package_dir=$SOFT_FP_PACKAGE_DIR", setup)
        self.assertIn("soft_fp64_package_version=$SOFT_FP64_PACKAGE_VERSION", setup)

        for relative in (
            ".github/workflows/ci.yml",
            ".github/workflows/release.yml",
            "README.md",
            "NOTICE",
            "scripts/metal_stress_gate.sh",
        ):
            text = (doc_parity.REPO_ROOT / relative).read_text()
            self.assertIn("v2.0.1", text, relative)

    def test_adaptivecpp_caches_include_integration_patch_and_setup_inputs(self) -> None:
        hash_expression = (
            "${{ hashFiles('.acpp-version', 'patches/adaptivecpp/*.patch', "
            "'scripts/setup_acpp.sh') }}"
        )
        workflow_names = ("ci.yml", "release.yml")
        cache_key_count = 0
        for workflow_name in workflow_names:
            workflow = (
                doc_parity.REPO_ROOT / ".github/workflows" / workflow_name
            ).read_text()
            for line in workflow.splitlines():
                if "key:" in line and "acpp-" in line:
                    cache_key_count += 1
                    self.assertIn(hash_expression, line, line)
        self.assertEqual(cache_key_count, 5)

    def test_ci_schema_artifact_runs_and_uploads_only_after_success(self) -> None:
        workflow = (
            doc_parity.REPO_ROOT / ".github/workflows/ci.yml"
        ).read_text()
        self.assertIn(
            "      - name: Generate package schema artifact\n"
            "        if: steps.pg-support.outputs.skip != 'true' && success()",
            workflow,
        )
        self.assertIn(
            "      - name: Upload schema artifact\n"
            "        if: always() && steps.schema.outcome == 'success'",
            workflow,
        )


class LinuxPrerequisiteParityTests(unittest.TestCase):
    def test_setup_selects_one_complete_llvm_clang_prefix(self) -> None:
        setup = (doc_parity.REPO_ROOT / "scripts/setup_acpp.sh").read_text()

        for required in (
            '"$candidate/bin/clang"',
            '"$candidate/bin/clang++"',
            '"$candidate/bin/llvm-config"',
            '"$candidate/lib/cmake/llvm"',
            '"$candidate/include/llvm/Passes/PassPlugin.h"',
            '"$candidate/include/clang/AST/ASTContext.h"',
        ):
            self.assertIn(required, setup)

        versioned = "        llvm-config-18 \\\n"
        unversioned = "        llvm-config; do\n"
        self.assertIn(versioned, setup)
        self.assertIn(unversioned, setup)
        self.assertLess(setup.index(versioned), setup.index(unversioned))
        self.assertIn("generic|cpu) select_linux_llvm ;;", setup)
        self.assertIn(
            "install matching llvm-dev, clang, and libclang-dev packages",
            setup,
        )


if __name__ == "__main__":
    unittest.main()
