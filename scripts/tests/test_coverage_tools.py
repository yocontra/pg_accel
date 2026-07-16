from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "coverage_tools.py"
REPO_ROOT = SCRIPT.parent.parent
SPEC = importlib.util.spec_from_file_location("coverage_tools", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
coverage_tools = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(coverage_tools)


def quiet_call(function, *args):
    with (
        contextlib.redirect_stdout(io.StringIO()),
        contextlib.redirect_stderr(io.StringIO()),
    ):
        return function(*args)


def gpu_test_log_text(
    test_name: str,
    body: str,
    *,
    exit_code: int = 0,
    result: str = "PASS",
) -> str:
    if body and not body.endswith("\n"):
        body += "\n"
    raw_lines = len(body.splitlines())
    body_sha256, binding_sha256 = coverage_tools.gpu_test_body_hashes(
        test_name, exit_code, result, raw_lines, body
    )
    return (
        f"PGACCEL_TEST_START name={test_name}\n"
        + body
        + f"PGACCEL_TEST_RESULT name={test_name} exit_code={exit_code} "
        f"result={result} raw_lines={raw_lines} body_sha256={body_sha256} "
        f"binding_sha256={binding_sha256}\n"
    )


def ctest_pass_log(names: list[str]) -> str:
    total = len(names)
    lines = [
        f"{index}/{total} Test #{index}: {name} .... Passed 0.01 sec"
        for index, name in enumerate(names, start=1)
    ]
    lines.extend(
        (
            f"100% tests passed, 0 tests failed out of {total}",
            "",
            "Total Test time (real) = 1.00 sec",
        )
    )
    return "\n".join(lines) + "\n"


class LineCoverageTests(unittest.TestCase):
    def write_scope(
        self, root: pathlib.Path, *, include_build_script: bool = False
    ) -> pathlib.Path:
        roots = ["crate/src"]
        if include_build_script:
            roots.append("crate/build.rs")
        scope = {
            "schema_version": 2,
            "minimum_percent": 90.0,
            "layers": {
                "rust": {
                    "description": "test Rust scope",
                    "roots": roots,
                    "extensions": [".rs"],
                    "required_extensions": [".rs"],
                    "exclude": ["**/tests.rs"],
                    "require_executable_mapping_only": False,
                    "production_mapping": "compiler-derived-pg18-without-pg_test",
                },
                "cpp": {
                    "description": "test C++ scope",
                    "roots": ["cpp"],
                    "extensions": [".cpp"],
                    "required_extensions": [".cpp"],
                    "exclude": [],
                },
            },
        }
        path = root / "scope.json"
        path.write_text(json.dumps(scope), encoding="utf-8")
        return path

    def write_lcov(
        self,
        root: pathlib.Path,
        records: dict[pathlib.Path, dict[int, int]],
        *,
        name: str = "raw.info",
    ) -> pathlib.Path:
        lines = ["TN:"]
        for source, hits in records.items():
            lines.append(f"SF:{source}")
            lines.extend(f"DA:{line},{count}" for line, count in sorted(hits.items()))
            lines.append("end_of_record")
        path = root / name
        path.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return path

    def rust_args(
        self,
        root: pathlib.Path,
        scope: pathlib.Path,
        report: pathlib.Path,
        *,
        execution_status: int = 0,
    ) -> argparse.Namespace:
        return argparse.Namespace(
            repo_root=str(root),
            scope=str(scope),
            layer="rust",
            threshold=90.0,
            execution_status=execution_status,
            input=str(report),
            production_map=str(report),
            format="lcov",
            output_dir=str(root / "output"),
            artifact_dir=None,
        )

    def test_summarize_fails_below_fixed_threshold(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "crate/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("fn covered() {}\n", encoding="utf-8")
            scope = self.write_scope(root)
            report = self.write_lcov(
                root, {source: {line: int(line <= 8) for line in range(1, 11)}}
            )

            status = quiet_call(
                coverage_tools.summarize_layer,
                self.rust_args(root, scope, report),
            )

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(summary["line_percent"], 80.0)
            self.assertFalse(summary["passed"])

    def test_missing_required_mapping_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            mapped = root / "crate/src/lib.rs"
            missing = root / "crate/src/ffi.rs"
            mapped.parent.mkdir(parents=True)
            mapped.write_text("fn mapped() {}\n", encoding="utf-8")
            missing.write_text("fn missing() {}\n", encoding="utf-8")
            scope = self.write_scope(root)
            report = self.write_lcov(root, {mapped: {1: 1}})

            status = quiet_call(
                coverage_tools.summarize_layer,
                self.rust_args(root, scope, report),
            )

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(
                summary["mapping"]["missing_required_files"], ["crate/src/ffi.rs"]
            )

    def test_nonzero_execution_forces_red_at_full_coverage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "crate/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("fn covered() {}\n", encoding="utf-8")
            scope = self.write_scope(root)
            report = self.write_lcov(root, {source: {1: 1}})

            status = quiet_call(
                coverage_tools.summarize_layer,
                self.rust_args(root, scope, report, execution_status=7),
            )

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(summary["execution"]["exit_code"], 7)
            self.assertFalse(summary["passed"])

    def test_prior_failed_stage_cannot_be_overwritten_by_full_coverage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "crate/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("fn covered() {}\n", encoding="utf-8")
            scope = self.write_scope(root)
            report = self.write_lcov(root, {source: {1: 1}})
            artifacts = root / "artifacts"
            quiet_call(
                coverage_tools.init_artifacts,
                argparse.Namespace(
                    artifact_dir=str(artifacts),
                    rust_threshold=90.0,
                    cpp_threshold=90.0,
                    sql_threshold=90.0,
                ),
            )
            quiet_call(
                coverage_tools.mark_layer_error,
                argparse.Namespace(
                    artifact_dir=str(artifacts),
                    layer="rust",
                    threshold=90.0,
                    stage="provenance",
                    message="dirty tree",
                    exit_code=1,
                ),
            )
            args = self.rust_args(root, scope, report)
            args.output_dir = str(artifacts / "rust")
            args.artifact_dir = str(artifacts)

            status = quiet_call(coverage_tools.summarize_layer, args)

            self.assertEqual(status, 1)
            summary = json.loads((artifacts / "rust/layer-summary.json").read_text())
            self.assertFalse(summary["passed"])
            self.assertIn("dirty tree", summary["errors"])
            self.assertEqual(summary["execution"]["exit_code"], 1)

    def test_compiler_map_ignores_6000_line_pg_test_supplement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "crate/src/lib.rs"
            source.parent.mkdir(parents=True)
            production = "\n".join(
                f"fn production_{i}() -> usize {{ {i} }}" for i in range(6000)
            )
            supplement = "\n".join(
                f"fn pg_test_{i}() {{ assert_eq!({i}, {i}); }}" for i in range(6000)
            )
            text = (
                '#[cfg(not(feature = "pg_test"))]\n'
                "mod production {\n"
                f"{production}\n"
                "}\n"
                '#[cfg(feature = "pg_test")]\n'
                "mod pg_test_only {\n"
                '  const BRACES: &str = r###" }}} /* not syntax */ "###;\n'
                "  /* nested comment { /* } */ } */\n"
                f"{supplement}\n"
                "}\n"
                "fn after() -> usize { 2 }\n"
            )
            source.write_text(text, encoding="utf-8")
            production_lines = {line: 1 for line in range(3, 6003)}
            production_lines[text.count("\n")] = 1
            supplemental_lines = dict(production_lines)
            supplemental_lines.update({line: 1 for line in range(6009, 12009)})
            scope = self.write_scope(root)
            report = self.write_lcov(
                root,
                {source: supplemental_lines},
            )
            production_map = self.write_lcov(
                root,
                {source: production_lines},
                name="production-map.info",
            )
            args = self.rust_args(root, scope, report)
            args.production_map = str(production_map)

            status = quiet_call(
                coverage_tools.summarize_layer,
                args,
            )

            self.assertEqual(status, 0)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(summary["line_count"], 6001)
            self.assertEqual(summary["covered_lines"], 6001)
            self.assertGreaterEqual(summary["supplemental_nonproduction_lines"], 6000)
            self.assertEqual(
                summary["production_mapping"]["policy"],
                "compiler-derived-pg18-without-pg_test",
            )

    def test_rust_scope_requires_compiler_mapping_policy(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            scope = coverage_tools.read_json(self.write_scope(root))
            self.assertEqual(
                scope["layers"]["rust"]["production_mapping"],
                "compiler-derived-pg18-without-pg_test",
            )

    def test_build_script_is_a_required_owned_mapping(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "crate/src/lib.rs"
            build = root / "crate/build.rs"
            source.parent.mkdir(parents=True)
            source.write_text("fn library() {}\n", encoding="utf-8")
            build.write_text("fn main() {}\n", encoding="utf-8")
            scope = self.write_scope(root, include_build_script=True)
            report = self.write_lcov(root, {source: {1: 1}})

            status = quiet_call(
                coverage_tools.summarize_layer,
                self.rust_args(root, scope, report),
            )

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertIn(
                "crate/build.rs", summary["mapping"]["missing_required_files"]
            )

    def test_threshold_cannot_be_weakened_or_nonfinite(self) -> None:
        with self.assertRaises(coverage_tools.CoverageError):
            coverage_tools.validate_threshold(89.99)
        with self.assertRaises(coverage_tools.CoverageError):
            coverage_tools.validate_threshold(float("nan"))


class SqlSemanticCoverageTests(unittest.TestCase):
    @staticmethod
    def guard_source(condition: str) -> str:
        return (
            "DO $$ BEGIN\n"
            f"  IF {condition} THEN RAISE EXCEPTION 'bad'; END IF;\n"
            "END $$;\n"
        )

    def create_fixture(
        self,
        root: pathlib.Path,
        *,
        log_lines: list[str] | None = None,
        status: str = "pass",
        exit_code: int = 0,
    ) -> tuple[pathlib.Path, pathlib.Path, pathlib.Path]:
        tests = root / "tests"
        run = root / "run"
        logs = run / "logs"
        tests.mkdir()
        logs.mkdir(parents=True)
        source = tests / "00_probe.sql"
        source.write_text(
            "DO $$ DECLARE probe_value integer := 1; BEGIN\n"
            "  IF probe_value <> 1 THEN RAISE EXCEPTION 'bad'; END IF;\n"
            "END $$;\n"
            "\\echo 'PGACCEL_ASSERT_OK:00_probe.assert_001'\n"
            "\\echo 'PGACCEL_FILE_OK:00_probe'\n",
            encoding="utf-8",
        )
        manifest = root / "manifest.json"
        with (
            mock.patch.object(coverage_tools, "BASELINE_SQL_FILES", 1),
            mock.patch.object(coverage_tools, "BASELINE_SQL_ASSERTIONS", 1),
        ):
            quiet_call(
                coverage_tools.build_sql_manifest,
                argparse.Namespace(tests_dir=str(tests), output=str(manifest)),
            )
        lines = log_lines or [
            "PGACCEL_ASSERT_OK:00_probe.assert_001",
            "PGACCEL_FILE_OK:00_probe",
        ]
        (logs / "00_probe.sql.log").write_text(
            "\n".join(lines) + "\n", encoding="utf-8"
        )
        (run / "results.tsv").write_text(
            "file\tstatus\texit_code\tlog\n"
            f"00_probe.sql\t{status}\t{exit_code}\tlogs/00_probe.sql.log\n",
            encoding="utf-8",
        )
        return tests, run, manifest

    def inventory(
        self,
        root: pathlib.Path,
        tests: pathlib.Path,
        run: pathlib.Path,
        manifest: pathlib.Path,
    ) -> int:
        args = argparse.Namespace(
            tests_dir=str(tests),
            results=str(run / "results.tsv"),
            manifest=str(manifest),
            output_dir=str(root / "output"),
            threshold=90.0,
            execution_status=0,
            artifact_dir=None,
        )
        with (
            mock.patch.object(coverage_tools, "BASELINE_SQL_FILES", 1),
            mock.patch.object(coverage_tools, "BASELINE_SQL_ASSERTIONS", 1),
        ):
            return quiet_call(coverage_tools.sql_inventory, args)

    def test_unique_successful_assertion_is_credited(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests, run, manifest = self.create_fixture(root)

            status = self.inventory(root, tests, run, manifest)

            self.assertEqual(status, 0)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(summary["covered_assertions"], 1)
            self.assertEqual(summary["assertion_count"], 1)
            self.assertTrue(summary["passed"])

    def test_notice_emits_only_after_an_in_block_guard_succeeds(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests, run, manifest = self.create_fixture(root)
            source = tests / "00_probe.sql"
            source.write_text(
                "DO $$ DECLARE probe_value integer := 1; BEGIN\n"
                "  IF probe_value <> 1 THEN RAISE EXCEPTION 'bad'; END IF;\n"
                "  RAISE NOTICE 'PGACCEL_ASSERT_OK:00_probe.assert_001';\n"
                "END $$;\n"
                "\\echo 'PGACCEL_FILE_OK:00_probe'\n",
                encoding="utf-8",
            )
            with (
                mock.patch.object(coverage_tools, "BASELINE_SQL_FILES", 1),
                mock.patch.object(coverage_tools, "BASELINE_SQL_ASSERTIONS", 1),
            ):
                quiet_call(
                    coverage_tools.build_sql_manifest,
                    argparse.Namespace(tests_dir=str(tests), output=str(manifest)),
                )
            (run / "logs/00_probe.sql.log").write_text(
                "psql:/tmp/00_probe.sql:3: NOTICE:  "
                "PGACCEL_ASSERT_OK:00_probe.assert_001\n"
                "PGACCEL_FILE_OK:00_probe\n",
                encoding="utf-8",
            )

            self.assertEqual(self.inventory(root, tests, run, manifest), 0)

    def test_failed_file_earns_no_semantic_credit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests, run, manifest = self.create_fixture(root, status="fail", exit_code=3)

            status = self.inventory(root, tests, run, manifest)

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(summary["covered_assertions"], 0)

    def test_duplicate_observation_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests, run, manifest = self.create_fixture(
                root,
                log_lines=[
                    "PGACCEL_ASSERT_OK:00_probe.assert_001",
                    "PGACCEL_ASSERT_OK:00_probe.assert_001",
                    "PGACCEL_FILE_OK:00_probe",
                ],
            )

            status = self.inventory(root, tests, run, manifest)

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(
                summary["manifest"]["duplicate_observation_ids"],
                ["00_probe.assert_001"],
            )

    def test_one_log_line_cannot_credit_multiple_ids(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests, run, manifest = self.create_fixture(
                root,
                log_lines=[
                    "PGACCEL_ASSERT_OK:00_probe.assert_001 PGACCEL_ASSERT_OK:other",
                    "PGACCEL_FILE_OK:00_probe",
                ],
            )

            status = self.inventory(root, tests, run, manifest)

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertTrue(
                any("multiple declarations" in error for error in summary["errors"])
            )

    def test_warning_skip_and_caught_exception_logs_are_rejected(self) -> None:
        forbidden = [
            "WARNING: raster unavailable",
            "SKIPPED: optional operation",
            "caught exception while probing",
            "caught an expected exception while probing",
        ]
        for line in forbidden:
            with self.subTest(line=line), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                tests, run, manifest = self.create_fixture(
                    root,
                    log_lines=[
                        "PGACCEL_ASSERT_OK:00_probe.assert_001",
                        line,
                        "PGACCEL_FILE_OK:00_probe",
                    ],
                )
                self.assertEqual(self.inventory(root, tests, run, manifest), 1)

    def test_source_hash_drift_and_file_removal_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests, run, manifest = self.create_fixture(root)
            (tests / "00_probe.sql").write_text(
                (tests / "00_probe.sql").read_text() + "-- drift\n",
                encoding="utf-8",
            )
            self.assertEqual(self.inventory(root, tests, run, manifest), 1)
            (tests / "00_probe.sql").unlink()
            self.assertEqual(self.inventory(root, tests, run, manifest), 1)

    def test_final_file_marker_is_not_an_assertion(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests = root / "tests"
            tests.mkdir()
            (tests / "00_probe.sql").write_text(
                "SELECT 1;\n\\echo 'PGACCEL_FILE_OK:00_probe'\n",
                encoding="utf-8",
            )
            with (
                mock.patch.object(coverage_tools, "BASELINE_SQL_FILES", 1),
                mock.patch.object(coverage_tools, "BASELINE_SQL_ASSERTIONS", 1),
                self.assertRaises(coverage_tools.CoverageError),
            ):
                coverage_tools.build_sql_manifest(
                    argparse.Namespace(
                        tests_dir=str(tests), output=str(root / "manifest.json")
                    )
                )

    def test_duplicate_declaration_id_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests = root / "tests"
            tests.mkdir()
            duplicate = "PGACCEL_ASSERT_OK:shared.assert_001"
            for stem in ("00_one", "01_two"):
                (tests / f"{stem}.sql").write_text(
                    f"\\echo '{duplicate}'\n\\echo 'PGACCEL_FILE_OK:{stem}'\n",
                    encoding="utf-8",
                )
            with (
                mock.patch.object(coverage_tools, "BASELINE_SQL_FILES", 1),
                mock.patch.object(coverage_tools, "BASELINE_SQL_ASSERTIONS", 1),
                self.assertRaises(coverage_tools.CoverageError),
            ):
                coverage_tools.build_sql_manifest(
                    argparse.Namespace(
                        tests_dir=str(tests), output=str(root / "manifest.json")
                    )
                )

    def test_two_ids_cannot_claim_the_same_failure_guard(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests = root / "tests"
            tests.mkdir()
            (tests / "00_probe.sql").write_text(
                "DO $$ DECLARE probe_value integer := 1; BEGIN\n"
                "  IF probe_value <> 1 THEN RAISE EXCEPTION 'bad'; END IF;\n"
                "END $$;\n"
                "\\echo 'PGACCEL_ASSERT_OK:00_probe.assert_001'\n"
                "\\echo 'PGACCEL_ASSERT_OK:00_probe.assert_002'\n"
                "\\echo 'PGACCEL_FILE_OK:00_probe'\n",
                encoding="utf-8",
            )
            with (
                mock.patch.object(coverage_tools, "BASELINE_SQL_FILES", 1),
                mock.patch.object(coverage_tools, "BASELINE_SQL_ASSERTIONS", 1),
                self.assertRaises(coverage_tools.CoverageError),
            ):
                coverage_tools.build_sql_manifest(
                    argparse.Namespace(
                        tests_dir=str(tests), output=str(root / "manifest.json")
                    )
                )

    def test_constant_if_guard_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests = root / "tests"
            tests.mkdir()
            (tests / "00_probe.sql").write_text(
                "DO $$ BEGIN\n"
                "  IF 1 <> 1 THEN RAISE EXCEPTION 'bad'; END IF;\n"
                "END $$;\n"
                "\\echo 'PGACCEL_ASSERT_OK:00_probe.assert_001'\n"
                "\\echo 'PGACCEL_FILE_OK:00_probe'\n",
                encoding="utf-8",
            )
            with (
                mock.patch.object(coverage_tools, "BASELINE_SQL_FILES", 1),
                mock.patch.object(coverage_tools, "BASELINE_SQL_ASSERTIONS", 1),
                self.assertRaises(coverage_tools.CoverageError),
            ):
                coverage_tools.build_sql_manifest(
                    argparse.Namespace(
                        tests_dir=str(tests), output=str(root / "manifest.json")
                    )
                )

    def test_self_comparisons_and_constant_boolean_guards_are_rejected(self) -> None:
        invalid = (
            "observed = observed",
            "observed <> observed",
            "TRUE OR observed = 1",
            "FALSE AND observed = 1",
            "NOT (TRUE OR observed = 1)",
            "(observed = 1) AND NOT (observed = 1)",
            "(observed IS NULL) AND (observed IS NOT NULL)",
            "(observed IS NULL) OR (observed IS NOT NULL)",
            "(observed IS UNKNOWN) OR (observed IS NOT NULL)",
            "observed IS TRUE OR observed IS FALSE OR observed IS UNKNOWN",
            "observed IS TRUE AND observed IS FALSE",
            "observed = 1 OR observed <> 1 OR observed IS NULL",
            "observed IS NULL OR observed IS DISTINCT FROM NULL",
            "observed IS DISTINCT FROM observed",
            "observed OR NULL IS NULL",
            "observed AND NULL IS NOT NULL",
            "observed AND 1 = 2",
            "observed OR 1 < 2",
        )
        for condition in invalid:
            with (
                self.subTest(condition=condition),
                self.assertRaises(coverage_tools.CoverageError),
            ):
                coverage_tools.sql_guard_expressions(self.guard_source(condition))

    def test_nested_three_valued_guards_retain_distinct_evidence(self) -> None:
        valid = (
            "observed = expected",
            "observed <> 1",
            "NOT (observed IS NULL)",
            "observed IS UNKNOWN",
            "observed IS NULL AND expected IS NOT NULL",
            "(observed = 1 AND expected IS NOT NULL) OR retry_count > 2",
            "NOT ((observed = expected) OR retry_count < 0)",
            "observed = expected OR observed <> expected",
        )
        for condition in valid:
            with self.subTest(condition=condition):
                guards = coverage_tools.sql_guard_expressions(
                    self.guard_source(condition)
                )
                self.assertEqual(len(guards), 1)
                self.assertEqual(guards[0]["kind"], "if")

    def test_guard_normalization_ignores_comment_and_string_boolean_words(
        self,
    ) -> None:
        plain = coverage_tools.sql_guard_expressions(
            self.guard_source("observed = expected")
        )
        commented = coverage_tools.sql_guard_expressions(
            self.guard_source("observed /* TRUE OR observed = observed */ = expected")
        )
        self.assertEqual(plain, commented)
        string_guard = coverage_tools.sql_guard_expressions(
            self.guard_source("message = 'TRUE OR message = message'")
        )
        self.assertEqual(len(string_guard), 1)
        self.assertEqual(string_guard[0]["tokens"], ["message"])

    def test_guard_words_in_a_comment_do_not_create_an_assertion(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests = root / "tests"
            tests.mkdir()
            (tests / "00_probe.sql").write_text(
                "-- RAISE EXCEPTION is not executable here\n"
                "SELECT 1;\n"
                "\\echo 'PGACCEL_ASSERT_OK:00_probe.assert_001'\n"
                "\\echo 'PGACCEL_FILE_OK:00_probe'\n",
                encoding="utf-8",
            )
            with (
                mock.patch.object(coverage_tools, "BASELINE_SQL_FILES", 1),
                mock.patch.object(coverage_tools, "BASELINE_SQL_ASSERTIONS", 1),
                self.assertRaises(coverage_tools.CoverageError),
            ):
                coverage_tools.build_sql_manifest(
                    argparse.Namespace(
                        tests_dir=str(tests), output=str(root / "manifest.json")
                    )
                )

    def test_guard_words_in_literals_and_noncode_dollars_are_rejected(self) -> None:
        fake_guards = (
            "SELECT 'RAISE EXCEPTION';",
            'SELECT "RAISE EXCEPTION";',
            "SELECT $$ RAISE EXCEPTION $$;",
            "/* outer /* RAISE EXCEPTION */ still comment */ SELECT 1;",
        )
        for fake_guard in fake_guards:
            with (
                self.subTest(fake_guard=fake_guard),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                tests = root / "tests"
                tests.mkdir()
                (tests / "00_probe.sql").write_text(
                    fake_guard
                    + "\n\\echo 'PGACCEL_ASSERT_OK:00_probe.assert_001'\n"
                    + "\\echo 'PGACCEL_FILE_OK:00_probe'\n",
                    encoding="utf-8",
                )
                with (
                    mock.patch.object(coverage_tools, "BASELINE_SQL_FILES", 1),
                    mock.patch.object(coverage_tools, "BASELINE_SQL_ASSERTIONS", 1),
                    self.assertRaises(coverage_tools.CoverageError),
                ):
                    coverage_tools.build_sql_manifest(
                        argparse.Namespace(
                            tests_dir=str(tests),
                            output=str(root / "manifest.json"),
                        )
                    )

    def test_checked_in_manifest_matches_baseline_and_raster_guards(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "manifest.json"
            status = quiet_call(
                coverage_tools.build_sql_manifest,
                argparse.Namespace(
                    tests_dir=str(REPO_ROOT / "sql/tests"),
                    output=str(output),
                    baseline=str(REPO_ROOT / "coverage/release-baseline.json"),
                ),
            )
            self.assertEqual(status, 0)
            generated = coverage_tools.read_json(output)
            checked_in = coverage_tools.read_json(
                REPO_ROOT / "coverage/sql-semantic-assertions.json"
            )
            self.assertEqual(generated, checked_in)
            self.assertEqual(generated["declared_assertions"], 287)
            matrix = next(
                entry
                for entry in generated["files"]
                if entry["file"] == "85_function_matrix.sql"
            )
            raster_ids = {
                "85_function_matrix.assert_037",
                "85_function_matrix.assert_051",
                "85_function_matrix.assert_052",
            }
            declarations = {
                assertion["id"]: assertion for assertion in matrix["assertions"]
            }
            self.assertTrue(raster_ids.issubset(declarations))
            self.assertTrue(
                all(declarations[value]["emission"] == "notice" for value in raster_ids)
            )

    def test_normal_manifest_generation_rejects_baseline_id_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with self.assertRaises(coverage_tools.CoverageError):
                coverage_tools.build_sql_manifest(
                    argparse.Namespace(
                        tests_dir=str(REPO_ROOT / "sql/tests"),
                        output=str(root / "no-baseline.json"),
                        baseline="",
                    )
                )

    def test_normal_manifest_generation_rejects_baseline_guard_mutation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            baseline = coverage_tools.read_json(
                REPO_ROOT / "coverage/release-baseline.json"
            )
            identifier = sorted(baseline["sql"]["assertion_guards"])[0]
            baseline["sql"]["assertion_guards"][identifier] = "0" * 64
            baseline_path = root / "baseline.json"
            coverage_tools.write_json(baseline_path, baseline)
            with self.assertRaises(coverage_tools.CoverageError):
                coverage_tools.build_sql_manifest(
                    argparse.Namespace(
                        tests_dir=str(REPO_ROOT / "sql/tests"),
                        output=str(root / "manifest.json"),
                        baseline=str(baseline_path),
                    )
                )
            baseline = coverage_tools.read_json(
                REPO_ROOT / "coverage/release-baseline.json"
            )
            baseline["sql"]["assertion_ids"].pop()
            baseline_path = root / "baseline.json"
            coverage_tools.write_json(baseline_path, baseline)
            with self.assertRaises(coverage_tools.CoverageError):
                coverage_tools.build_sql_manifest(
                    argparse.Namespace(
                        tests_dir=str(REPO_ROOT / "sql/tests"),
                        output=str(root / "manifest.json"),
                        baseline=str(baseline_path),
                    )
                )


class AggregateNegativeMatrixTests(unittest.TestCase):
    def initialize_valid_gate(self, root: pathlib.Path) -> None:
        quiet_call(
            coverage_tools.init_artifacts,
            argparse.Namespace(
                artifact_dir=str(root),
                rust_threshold=90.0,
                cpp_threshold=90.0,
                sql_threshold=90.0,
            ),
        )
        repo_root = root / "checkout"
        rust_sources = ["crate/src/lib.rs", "crate/build.rs"]
        cpp_sources = [f"kernels/src/source_{index:02d}.cpp" for index in range(21)]
        ctest_names = [
            "test_device",
            *(f"test_{index:02d}" for index in range(1, 28)),
            "test_oom_invariant",
        ]
        families = [
            "reduce_f64",
            "sort_f64",
            "hashagg_f64",
            "spatial_f64",
            "h3_f64",
        ]
        for relative in rust_sources:
            path = repo_root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("fn fixture() {}\n", encoding="utf-8")
        for relative in cpp_sources:
            path = repo_root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("int fixture() { return 1; }\n", encoding="utf-8")
        cpp_header = "kernels/include/fixture.h"
        header_path = repo_root / cpp_header
        header_path.parent.mkdir(parents=True, exist_ok=True)
        header_path.write_text(
            "inline int fixture_header() { return 1; }\n", encoding="utf-8"
        )

        manifest_entries: list[dict[str, object]] = []
        assertion_ids: list[str] = []
        sql_dir = repo_root / "sql/tests"
        sql_dir.mkdir(parents=True)
        for file_index in range(52):
            stem = f"{file_index:02d}_fixture"
            name = f"{stem}.sql"
            assertion_count = 6 if file_index < 27 else 5
            source_lines: list[str] = []
            for assertion_index in range(1, assertion_count + 1):
                identifier = f"{stem}.assert_{assertion_index:03d}"
                source_lines.extend(
                    (
                        "DO $$ DECLARE fixture_value integer := 1; BEGIN",
                        "  IF fixture_value <> 1 THEN RAISE EXCEPTION 'bad'; END IF;",
                        "END $$;",
                        f"\\echo 'PGACCEL_ASSERT_OK:{identifier}'",
                    )
                )
                assertion_ids.append(identifier)
            source_lines.append(f"\\echo 'PGACCEL_FILE_OK:{stem}'")
            source_path = sql_dir / name
            source_path.write_text("\n".join(source_lines) + "\n", encoding="utf-8")
            assertions, completion, marker_errors = (
                coverage_tools.read_sql_source_markers(source_path)
            )
            self.assertEqual(marker_errors, [])
            manifest_entries.append(
                {
                    "file": name,
                    "sha256": coverage_tools.sha256(source_path),
                    "completion_id": completion,
                    "assertions": assertions,
                }
            )
        self.assertEqual(len(assertion_ids), 287)

        scope = {
            "schema_version": 2,
            "minimum_percent": 90.0,
            "sql_manifest": "coverage/sql-semantic-assertions.json",
            "layers": {
                "rust": {
                    "description": "fixture Rust scope",
                    "roots": ["crate/src", "crate/build.rs"],
                    "extensions": [".rs"],
                    "required_extensions": [".rs"],
                    "require_executable_mapping_only": False,
                    "production_mapping": "compiler-derived-pg18-without-pg_test",
                    "exclude": [],
                },
                "cpp": {
                    "description": "fixture C++ scope",
                    "roots": ["kernels/src", "kernels/include"],
                    "extensions": [".cpp", ".hpp", ".h"],
                    "required_extensions": [".cpp", ".hpp", ".h"],
                    "require_executable_mapping_only": True,
                    "exclude": [],
                },
            },
        }
        manifest = {
            "schema_version": 2,
            "kind": "sql-semantic-assertion-manifest",
            "test_root": "sql/tests",
            "baseline_files": 52,
            "baseline_assertions": 287,
            "declared_files": 52,
            "declared_assertions": 287,
            "files": manifest_entries,
        }
        baseline = {
            "schema_version": 2,
            "kind": "coverage-release-baseline",
            "minimum_percent": 90.0,
            "rust": {
                "roots": scope["layers"]["rust"]["roots"],
                "exclude": [],
                "production_feature": "pg18",
                "forbidden_production_feature": "pg_test",
                "mapping_policy": "compiler-derived-pg18-without-pg_test",
                "owned_files": rust_sources,
                "required_mapping_files": rust_sources,
            },
            "cpp": {
                "roots": scope["layers"]["cpp"]["roots"],
                "exclude": [],
                "extensions": [".cpp", ".hpp", ".h"],
                "required_extensions": [".cpp", ".hpp", ".h"],
                "require_executable_mapping_only": True,
                "sources": cpp_sources,
                "executable_headers": [cpp_header],
                "owned_files": [*cpp_sources, cpp_header],
                "required_mapping_files": [*cpp_sources, cpp_header],
                "ctest_names": sorted(ctest_names),
                "ctest_evidence": {
                    name: (
                        "device-family-dispatch-oom"
                        if name == "test_oom_invariant"
                        else "execution"
                    )
                    for name in sorted(ctest_names)
                },
                "oom_families": families,
            },
            "sql": {
                "files": [entry["file"] for entry in manifest_entries],
                "assertion_ids": sorted(assertion_ids),
                "assertion_guards": {
                    assertion["id"]: assertion["guard_sha256"]
                    for entry in manifest_entries
                    for assertion in entry["assertions"]
                },
            },
        }
        coverage_dir = repo_root / "coverage"
        coverage_dir.mkdir()
        coverage_tools.write_json(coverage_dir / "scope.json", scope)
        coverage_tools.write_json(coverage_dir / "release-baseline.json", baseline)
        coverage_tools.write_json(
            coverage_dir / "sql-semantic-assertions.json", manifest
        )
        patch_path = repo_root / "patches/adaptivecpp/sscp-host-coverage.patch"
        patch_path.parent.mkdir(parents=True)
        patch_path.write_text("fixture AdaptiveCpp coverage patch\n", encoding="utf-8")
        for command in (
            ["git", "init", "-q"],
            ["git", "config", "user.email", "coverage@example.invalid"],
            ["git", "config", "user.name", "Coverage Fixture"],
            ["git", "add", "."],
            ["git", "commit", "-q", "-m", "fixture"],
        ):
            subprocess.run(command, cwd=repo_root, check=True)

        for name in (
            "scope.json",
            "release-baseline.json",
            "sql-semantic-assertions.json",
        ):
            source_path = coverage_dir / name
            (root / name).write_bytes(source_path.read_bytes())
        (root / "adaptivecpp-sscp-host-coverage.patch").write_bytes(
            patch_path.read_bytes()
        )

        scope = coverage_tools.read_json(root / "scope.json")
        baseline = coverage_tools.read_json(root / "release-baseline.json")

        def write_lcov(
            path: pathlib.Path, relative_files: list[str], hits: int = 1
        ) -> None:
            lines = ["TN:"]
            for relative in relative_files:
                lines.extend(
                    (
                        f"SF:{repo_root / relative}",
                        f"DA:1,{hits}",
                        "end_of_record",
                    )
                )
            path.write_text("\n".join(lines) + "\n", encoding="utf-8")

        def write_export(path: pathlib.Path, relative_files: list[str]) -> None:
            coverage_tools.write_json(
                path,
                {
                    "type": "llvm.coverage.json.export",
                    "version": "2.0.1",
                    "data": [
                        {
                            "files": [
                                {
                                    "filename": str(repo_root / relative),
                                    "summary": {
                                        "lines": {
                                            "count": 1,
                                            "covered": 1,
                                            "percent": 100.0,
                                        }
                                    },
                                }
                                for relative in relative_files
                            ]
                        }
                    ],
                },
            )

        rust_files = baseline["rust"]["owned_files"]
        write_lcov(root / "rust/production-map.info", rust_files, hits=0)
        write_lcov(root / "rust/raw-lcov.info", rust_files)
        write_export(root / "rust/raw-coverage.json", rust_files)
        (root / "rust/raw-summary.txt").write_text("TOTAL 100.00%\n", encoding="utf-8")
        coverage_tools.write_json(
            root / "rust/production-config.json",
            {
                "postgres_major": 18,
                "default_features": False,
                "features": ["pg18"],
                "pg_test": False,
            },
        )
        rust_status = quiet_call(
            coverage_tools.summarize_layer,
            argparse.Namespace(
                repo_root=str(repo_root),
                scope=str(root / "scope.json"),
                layer="rust",
                threshold=90.0,
                execution_status=0,
                input=str(root / "rust/raw-lcov.info"),
                production_map=str(root / "rust/production-map.info"),
                format="lcov",
                output_dir=str(root / "rust"),
                artifact_dir=None,
            ),
        )
        self.assertEqual(rust_status, 0)

        _, cpp_required = coverage_tools.source_inventory(
            repo_root, scope["layers"]["cpp"]
        )
        cpp_files = sorted(cpp_required)
        write_export(root / "cpp/raw-coverage.json", cpp_files)
        write_lcov(root / "cpp/raw-lcov.info", cpp_files)
        (root / "cpp/raw-summary.txt").write_text("TOTAL 100.00%\n", encoding="utf-8")
        cpp_status = quiet_call(
            coverage_tools.summarize_layer,
            argparse.Namespace(
                repo_root=str(repo_root),
                scope=str(root / "scope.json"),
                layer="cpp",
                threshold=90.0,
                execution_status=0,
                input=str(root / "cpp/raw-coverage.json"),
                production_map=None,
                format="json",
                output_dir=str(root / "cpp"),
                artifact_dir=None,
            ),
        )
        self.assertEqual(cpp_status, 0)

        tools_dir = root / "tools"
        tools_dir.mkdir()
        tool_outputs = {
            "rustc": "rustc 1.99.0\nLLVM version: 20.1.0",
            "clang": "clang version 20.1.0",
        }
        tool_paths: dict[str, pathlib.Path] = {}
        for name, output in tool_outputs.items():
            path = tools_dir / name
            path.write_text(
                f"#!/bin/sh\nprintf '%s\\n' '{output}'\n",
                encoding="utf-8",
            )
            path.chmod(0o755)
            tool_paths[name] = path
        llvm_cov = tools_dir / "llvm-cov"
        llvm_cov.write_text(
            """#!/usr/bin/env python3
import json
import pathlib
import sys

with (pathlib.Path(sys.argv[0]).parent / "invocations.log").open("a") as log:
    log.write("llvm-cov " + " ".join(sys.argv[1:]) + "\\n")
if "--version" in sys.argv:
    print("LLVM version 20.1.0")
    raise SystemExit(0)
if len(sys.argv) < 2 or sys.argv[1] not in {"export", "report"}:
    raise SystemExit(2)
prof_arg = next((value for value in sys.argv if value.startswith("-instr-profile=")), None)
if prof_arg is None:
    raise SystemExit(3)
prof = pathlib.Path(prof_arg.split("=", 1)[1]).read_text()
prof_values = dict(line.split("=", 1) for line in prof.splitlines() if "=" in line)
objects = []
index = 2
while index < len(sys.argv):
    value = sys.argv[index]
    if value == "-object":
        objects.append(pathlib.Path(sys.argv[index + 1]))
        index += 2
        continue
    if not value.startswith("-"):
        objects.append(pathlib.Path(value))
    index += 1
files = set()
for path in objects:
    values = dict(line.split("=", 1) for line in path.read_text().splitlines() if "=" in line)
    if values.get("BUNDLE") != prof_values.get("BUNDLE"):
        print("profile/object bundle mismatch", file=sys.stderr)
        raise SystemExit(4)
    files.update(json.loads(values["FILES"]))
hits = int(prof_values.get("HITS", "0"))
if sys.argv[1] == "report":
    print("TOTAL 100.00%" if hits > 0 else "TOTAL 0.00%")
    raise SystemExit(0)
if "-format=lcov" in sys.argv:
    print("TN:")
    for filename in sorted(files):
        print(f"SF:{filename}")
        print(f"DA:1,{hits}")
        print("end_of_record")
else:
    print(json.dumps({
        "type": "llvm.coverage.json.export",
        "version": "2.0.1",
        "data": [{"files": [{
            "filename": filename,
            "summary": {"lines": {
                "count": 1,
                "covered": 1 if hits > 0 else 0,
                "percent": 100.0 if hits > 0 else 0.0,
            }},
        } for filename in sorted(files)]}],
    }, sort_keys=True))
""",
            encoding="utf-8",
        )
        llvm_cov.chmod(0o755)
        tool_paths["llvm-cov"] = llvm_cov
        profdata = tools_dir / "llvm-profdata"
        profdata.write_text(
            """#!/usr/bin/env python3
import pathlib
import sys

with (pathlib.Path(sys.argv[0]).parent / "invocations.log").open("a") as log:
    log.write("llvm-profdata " + " ".join(sys.argv[1:]) + "\\n")
if "--version" in sys.argv:
    print("LLVM version 20.1.0")
    raise SystemExit(0)
if len(sys.argv) > 1 and sys.argv[1] == "show":
    text = pathlib.Path(sys.argv[2]).read_text()
    raise SystemExit(0 if "BUNDLE=" in text else 2)
if len(sys.argv) > 1 and sys.argv[1] == "merge":
    output_index = sys.argv.index("-o")
    profiles = [pathlib.Path(value) for value in sys.argv[3:output_index]]
    values = [
        dict(line.split("=", 1) for line in path.read_text().splitlines() if "=" in line)
        for path in profiles
    ]
    bundles = {value.get("BUNDLE") for value in values}
    if len(bundles) != 1 or None in bundles:
        print("unrelated profiles", file=sys.stderr)
        raise SystemExit(3)
    hits = max(int(value.get("HITS", "0")) for value in values)
    pathlib.Path(sys.argv[output_index + 1]).write_text(
        f"VALID\\nBUNDLE={next(iter(bundles))}\\nHITS={hits}\\n"
    )
    raise SystemExit(0)
raise SystemExit(2)
""",
            encoding="utf-8",
        )
        profdata.chmod(0o755)
        tool_paths["llvm-profdata"] = profdata
        self.assertEqual(
            quiet_call(
                coverage_tools.validate_rust_toolchain,
                argparse.Namespace(
                    rustc=str(tool_paths["rustc"]),
                    llvm_cov=str(tool_paths["llvm-cov"]),
                    llvm_profdata=str(tool_paths["llvm-profdata"]),
                    output=str(root / "rust/toolchain.json"),
                ),
            ),
            0,
        )

        bundle_specs = (
            (
                "rust",
                "production",
                "rust-production",
                rust_files,
                root / "rust/production-profiles",
                root / "rust/production-objects",
                root / "rust/production-object-manifest.json",
                root / "rust/production-coverage.profdata",
                root / "rust/production-coverage.json",
                root / "rust/production-map.info",
                0,
            ),
            (
                "rust",
                "final",
                "rust-final",
                rust_files,
                root / "rust/profiles",
                root / "rust/objects",
                root / "rust/object-manifest.json",
                root / "rust/coverage.profdata",
                root / "rust/raw-coverage.json",
                root / "rust/raw-lcov.info",
                1,
            ),
            (
                "cpp",
                "final",
                "cpp-final",
                cpp_files,
                root / "cpp/profiles",
                root / "cpp/objects",
                root / "cpp/object-manifest.json",
                root / "cpp/coverage.profdata",
                root / "cpp/raw-coverage.json",
                root / "cpp/raw-lcov.info",
                1,
            ),
        )
        for (
            layer,
            role,
            bundle_id,
            mapped_files,
            profiles,
            objects,
            object_manifest,
            merged_profile,
            json_output,
            lcov_output,
            hits,
        ) in bundle_specs:
            profiles.mkdir(exist_ok=True)
            (profiles / "fixture.profraw").write_text(
                f"BUNDLE={bundle_id}\nHITS={hits}\n", encoding="utf-8"
            )
            if layer == "cpp":
                (profiles / "fixture.proftext").write_text(
                    f"BUNDLE={bundle_id}\nHITS={hits}\n", encoding="utf-8"
                )
            source_object = tools_dir / f"{bundle_id}.object"
            source_object.write_text(
                f"BUNDLE={bundle_id}\n"
                + "FILES="
                + json.dumps([str(repo_root / value) for value in mapped_files])
                + "\n",
                encoding="utf-8",
            )
            self.assertEqual(
                quiet_call(
                    coverage_tools.capture_coverage_bundle,
                    argparse.Namespace(
                        layer=layer,
                        role=role,
                        repo_root=str(repo_root),
                        scope=str(root / "scope.json"),
                        llvm_cov=str(tool_paths["llvm-cov"]),
                        llvm_profdata=str(tool_paths["llvm-profdata"]),
                        profile_dir=str(profiles),
                        candidate_root=None,
                        object=[str(source_object)],
                        object_dir=str(objects),
                        manifest=str(object_manifest),
                        profdata=str(merged_profile),
                        json_output=str(json_output),
                        lcov_output=str(lcov_output),
                        summary_output=(
                            str(root / layer / "raw-summary.txt")
                            if role == "final"
                            else None
                        ),
                    ),
                ),
                0,
            )
        self.assertEqual(
            quiet_call(
                coverage_tools.validate_toolchain,
                argparse.Namespace(
                    clang=str(tool_paths["clang"]),
                    llvm_cov=str(tool_paths["llvm-cov"]),
                    llvm_profdata=str(tool_paths["llvm-profdata"]),
                    output=str(root / "cpp/toolchain.json"),
                ),
            ),
            0,
        )
        device_object_root = root / "device-build/CMakeFiles/shared.dir"
        device_object_root.mkdir(parents=True)
        device_objects = []
        for index in range(coverage_tools.BASELINE_CPP_SOURCES):
            path = device_object_root / f"kernel_{index:02d}.cpp.o"
            path.write_bytes(f"device-object-{index}\n".encode("ascii"))
            device_objects.append(str(path))
        self.assertEqual(
            quiet_call(
                coverage_tools.audit_device_profile_intrinsics,
                argparse.Namespace(
                    object=device_objects,
                    object_root=str(device_object_root),
                    object_dir=str(root / "cpp/device-objects"),
                    output=str(root / "cpp/device-profile-audit.json"),
                ),
            ),
            0,
        )

        ctest_names = baseline["cpp"]["ctest_names"]
        families = baseline["cpp"]["oom_families"]
        per_test_logs = root / "cpp/per-test-logs"
        per_test_logs.mkdir()
        for test_name in ctest_names:
            raw_log = per_test_logs / f"{test_name}-fixture.log"
            if test_name == "test_oom_invariant":
                family_lines = [
                    "PGACCEL_OOM_FAMILY "
                    f"family={family} result=PASS dispatches=1 "
                    "peak_rss_bytes=100 rss_baseline_bytes=90 "
                    "rss_delta_bytes=10 rss_limit_bytes=300"
                    for family in families
                ]
                body = (
                    'PGACCEL_DEVICE_PROOF device="fixture" backend="cuda" '
                    "compute_units=1 max_alloc_bytes=100 real_device=1\n"
                    + "\n".join(family_lines)
                    + "\nPGACCEL_OOM_INVARIANT result=PASS families=5 "
                    "max_alloc_bytes=100 input_doubles=25 rss_limit_bytes=300\n"
                )
                raw_log.write_text(
                    gpu_test_log_text(test_name, body),
                    encoding="utf-8",
                )
            else:
                raw_log.write_text(
                    gpu_test_log_text(
                        test_name, f"PGACCEL_TEST_PASS name={test_name}\n"
                    ),
                    encoding="utf-8",
                )
        ctest_log = root / "cpp/ctest.log"
        ctest_log.write_text(ctest_pass_log(ctest_names), encoding="utf-8")
        self.assertEqual(
            quiet_call(
                coverage_tools.gpu_evidence,
                argparse.Namespace(
                    execution_status=0,
                    ctest_log=str(ctest_log),
                    per_test_log_dir=str(per_test_logs),
                    baseline=str(root / "release-baseline.json"),
                    output=str(root / "cpp/gpu-correctness-evidence.json"),
                ),
            ),
            0,
        )

        manifest_path = root / "sql-semantic-assertions.json"
        manifest_document = coverage_tools.read_json(manifest_path)
        manifest_hash = coverage_tools.sha256(manifest_path)
        successful_ids: list[str] = []
        file_rows: list[dict[str, object]] = []
        result_lines = ["file\tstatus\texit_code\tlog"]
        logs_dir = root / "sql/test-run/logs"
        logs_dir.mkdir(parents=True)
        for entry in manifest_document["files"]:
            name = entry["file"]
            identifiers = [assertion["id"] for assertion in entry["assertions"]]
            successful_ids.extend(identifiers)
            log_path = logs_dir / f"{name}.log"
            log_path.write_text(
                "\n".join(
                    [
                        *(
                            f"PGACCEL_ASSERT_OK:{identifier}"
                            for identifier in identifiers
                        ),
                        f"PGACCEL_FILE_OK:{entry['completion_id']}",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            relative_log = f"logs/{name}.log"
            result_lines.append(f"{name}\tpass\t0\t{relative_log}")
            file_rows.append(
                {
                    "file": name,
                    "status": "pass",
                    "exit_code": 0,
                    "observed_assertions": len(identifiers),
                    "completion_markers": 1,
                    "log": relative_log,
                    "log_sha256": coverage_tools.sha256(log_path),
                }
            )
        self.assertEqual(len(successful_ids), 287)
        (root / "sql/test-run/results.tsv").write_text(
            "\n".join(result_lines) + "\n", encoding="utf-8"
        )
        coverage_tools.write_json(
            root / "sql/assertion-inventory.json",
            {
                "schema_version": 2,
                "kind": "sql-semantic-assertion-inventory",
                "manifest_sha256": manifest_hash,
                "declared_assertions": len(successful_ids),
                "successful_assertions": len(successful_ids),
                "assertion_percent": 100.0,
                "declared_files": len(file_rows),
                "passed_files": len(file_rows),
                "completed_files": len(file_rows),
                "successful_assertion_ids": sorted(successful_ids),
                "errors": [],
                "complete": True,
                "files": file_rows,
            },
        )
        sql_summary = coverage_tools.initial_layer_summary("sql", 90.0)
        sql_manifest = coverage_tools.initial_manifest_state()
        sql_manifest.update(
            {
                "valid": True,
                "sha256": manifest_hash,
                "declared_files": len(file_rows),
                "declared_assertions": len(successful_ids),
                "completed_files": len(file_rows),
                "passed_test_files": len(file_rows),
                "test_files": len(file_rows),
            }
        )
        sql_summary.update(
            {
                "covered_units": len(successful_ids),
                "total_units": len(successful_ids),
                "uncovered_units": 0,
                "percent": 100.0,
                "covered_assertions": len(successful_ids),
                "assertion_count": len(successful_ids),
                "uncovered_assertions": 0,
                "assertion_percent": 100.0,
                "manifest": sql_manifest,
                "execution": {
                    "status": "complete",
                    "exit_code": 0,
                    "stages_complete": True,
                },
                "errors": [],
                "passed": True,
            }
        )
        coverage_tools.write_json(root / "sql/layer-summary.json", sql_summary)

        commit = subprocess.run(
            ["git", "rev-parse", "--verify", "HEAD"],
            cwd=repo_root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        coverage_tools.write_json(
            root / "provenance.json",
            {
                "schema_version": 2,
                "kind": "coverage-provenance",
                "commit": commit,
                "tree": "clean",
                "scope_sha256": coverage_tools.sha256(root / "scope.json"),
                "baseline_sha256": coverage_tools.sha256(
                    root / "release-baseline.json"
                ),
                "adaptivecpp_patch_sha256": coverage_tools.sha256(
                    root / "adaptivecpp-sscp-host-coverage.patch"
                ),
                "errors": [],
                "passed": True,
            },
        )
        for layer in ("rust", "cpp", "sql"):
            self.write_complete_stage(root, layer)
            self.assertEqual(
                quiet_call(
                    coverage_tools.seal_layer_evidence,
                    argparse.Namespace(artifact_dir=str(root), layer=layer),
                ),
                0,
            )

    def write_complete_stage(self, root: pathlib.Path, layer: str) -> None:
        coverage_tools.write_json(
            root / layer / "stage-status.json",
            {
                "schema_version": 2,
                "kind": "coverage-stage-status",
                "layer_id": layer,
                "complete": True,
                "stages": {
                    stage: {"status": "complete", "exit_code": 0}
                    for stage in coverage_tools.REQUIRED_STAGES[layer]
                },
                "errors": [],
            },
        )

    def aggregate(self, root: pathlib.Path) -> int:
        with mock.patch.object(
            coverage_tools, "validate_trusted_llvm_tools", return_value=[]
        ):
            return quiet_call(
                coverage_tools.aggregate,
                argparse.Namespace(
                    artifact_dir=str(root), repo_root=str(root / "checkout")
                ),
            )

    def reseal(self, root: pathlib.Path, layer: str) -> None:
        self.assertEqual(
            quiet_call(
                coverage_tools.seal_layer_evidence,
                argparse.Namespace(artifact_dir=str(root), layer=layer),
            ),
            0,
        )

    def mutate_summary(self, root: pathlib.Path, layer: str, mutation) -> None:
        path = root / layer / "layer-summary.json"
        document = json.loads(path.read_text())
        mutation(document)
        path.write_text(json.dumps(document), encoding="utf-8")

    def assert_mutation_fails(self, mutation, layer: str = "rust") -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            self.mutate_summary(root, layer, mutation)
            self.assertEqual(self.aggregate(root), 1)
            result = json.loads((root / "gate-summary.json").read_text())
            self.assertFalse(result["passed"])

    def test_valid_synthetic_gate_passes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            self.assertEqual(self.aggregate(root), 0)

    def test_script_llvm_tools_are_rejected_as_untrusted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            self.assertEqual(
                quiet_call(
                    coverage_tools.aggregate,
                    argparse.Namespace(
                        artifact_dir=str(root), repo_root=str(root / "checkout")
                    ),
                ),
                1,
            )

    def test_aggregate_regenerates_reports_with_recorded_llvm_tools(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            invocation_log = root / "tools/invocations.log"
            invocation_log.write_text("", encoding="utf-8")
            self.assertEqual(self.aggregate(root), 0)
            invocations = invocation_log.read_text(encoding="utf-8").splitlines()
            self.assertEqual(
                sum(line.startswith("llvm-profdata merge ") for line in invocations),
                3,
            )
            self.assertEqual(
                sum(line.startswith("llvm-cov export ") for line in invocations),
                6,
            )

    def test_handwritten_reports_fail_even_after_reseal(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            lcov_path = root / "rust/raw-lcov.info"
            lcov_path.write_text(
                lcov_path.read_text(encoding="utf-8").replace("DA:1,1", "DA:1,0"),
                encoding="utf-8",
            )
            json_path = root / "rust/raw-coverage.json"
            report = coverage_tools.read_json(json_path)
            for entry in report["data"][0]["files"]:
                entry["summary"]["lines"].update(covered=0, percent=0.0)
            coverage_tools.write_json(json_path, report)
            self.reseal(root, "rust")
            self.assertEqual(self.aggregate(root), 1)

    def test_unrelated_raw_profile_fails_even_after_reseal(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            (root / "rust/profiles/unrelated.profraw").write_text(
                "BUNDLE=foreign\nHITS=1\n", encoding="utf-8"
            )
            self.reseal(root, "rust")
            self.assertEqual(self.aggregate(root), 1)

    def test_missing_or_tampered_retained_object_fails_after_reseal(self) -> None:
        for mutation in ("missing", "tampered"):
            with (
                self.subTest(mutation=mutation),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                self.initialize_valid_gate(root)
                object_path = next((root / "cpp/objects").iterdir())
                if mutation == "missing":
                    object_path.unlink()
                    self.assertEqual(
                        quiet_call(
                            coverage_tools.seal_layer_evidence,
                            argparse.Namespace(artifact_dir=str(root), layer="cpp"),
                        ),
                        1,
                    )
                else:
                    object_path.write_text("forged object\n", encoding="utf-8")
                    self.reseal(root, "cpp")
                self.assertEqual(self.aggregate(root), 1)

    def test_missing_layer_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            (root / "sql/layer-summary.json").unlink()
            self.assertEqual(self.aggregate(root), 1)

    def test_unknown_layer_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            rogue = root / "rogue/layer-summary.json"
            rogue.parent.mkdir()
            rogue.write_text(
                (root / "rust/layer-summary.json").read_text(), encoding="utf-8"
            )
            self.assertEqual(self.aggregate(root), 1)

    def test_duplicate_layer_id_is_rejected(self) -> None:
        self.assert_mutation_fails(lambda value: value.update(layer_id="cpp"))

    def test_wrong_schema_is_rejected(self) -> None:
        self.assert_mutation_fails(lambda value: value.update(schema_version=1))

    def test_weakened_threshold_is_rejected(self) -> None:
        self.assert_mutation_fails(lambda value: value.update(threshold_percent=89.0))

    def test_malformed_threshold_is_rejected_without_losing_aggregate(self) -> None:
        self.assert_mutation_fails(lambda value: value.update(threshold_percent=None))

    def test_inconsistent_arithmetic_is_rejected(self) -> None:
        self.assert_mutation_fails(lambda value: value.update(uncovered_units=9))

    def test_inconsistent_percent_is_rejected(self) -> None:
        self.assert_mutation_fails(lambda value: value.update(percent=99.0))

    def test_forged_passed_flag_is_rejected(self) -> None:
        self.assert_mutation_fails(lambda value: value.update(passed=False))

    def test_nonzero_execution_is_rejected(self) -> None:
        self.assert_mutation_fails(lambda value: value["execution"].update(exit_code=9))

    def test_incomplete_stage_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            stage = json.loads((root / "rust/stage-status.json").read_text())
            stage["complete"] = False
            (root / "rust/stage-status.json").write_text(json.dumps(stage))
            self.assertEqual(self.aggregate(root), 1)

    def test_missing_required_stage_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            stage_path = root / "rust/stage-status.json"
            stage = json.loads(stage_path.read_text())
            del stage["stages"]["pgrx_tests"]
            stage_path.write_text(json.dumps(stage), encoding="utf-8")
            self.assertEqual(self.aggregate(root), 1)

    def test_invalid_mapping_is_rejected(self) -> None:
        self.assert_mutation_fails(
            lambda value: value["mapping"].update(
                required_files=2, mapped_files=1, owned_files=1
            )
        )

    def test_unhashable_mapping_entry_is_rejected_without_crashing(self) -> None:
        self.assert_mutation_fails(
            lambda value: value["mapping"].update(
                missing_required_files=[{"not": "a path"}]
            )
        )

    def test_sql_manifest_failure_is_rejected(self) -> None:
        self.assert_mutation_fails(
            lambda value: value["manifest"].update(
                valid=False, hash_drift_files=["00_probe.sql"]
            ),
            layer="sql",
        )

    def test_self_consistent_forged_copied_manifest_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            manifest_path = root / "sql-semantic-assertions.json"
            manifest = json.loads(manifest_path.read_text())
            manifest["baseline_assertions"] = 1
            manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
            forged_hash = coverage_tools.sha256(manifest_path)
            self.mutate_summary(
                root,
                "sql",
                lambda value: value["manifest"].update(sha256=forged_hash),
            )
            inventory_path = root / "sql/assertion-inventory.json"
            inventory = json.loads(inventory_path.read_text())
            inventory["manifest_sha256"] = forged_hash
            inventory_path.write_text(json.dumps(inventory), encoding="utf-8")
            self.assertEqual(self.aggregate(root), 1)

    def test_impossible_sql_summary_is_rejected(self) -> None:
        self.assert_mutation_fails(
            lambda value: value["manifest"].update(completed_files=53),
            layer="sql",
        )

    def test_sql_summary_cannot_claim_zero_executed_manifest_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            self.mutate_summary(
                root,
                "sql",
                lambda value: value["manifest"].update(
                    test_files=0, passed_test_files=0, completed_files=0
                ),
            )
            inventory_path = root / "sql/assertion-inventory.json"
            inventory = json.loads(inventory_path.read_text())
            inventory.update(passed_files=0, completed_files=0)
            inventory_path.write_text(json.dumps(inventory), encoding="utf-8")
            self.assertEqual(self.aggregate(root), 1)

    def test_inconsistent_inventory_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            inventory = json.loads((root / "sql/assertion-inventory.json").read_text())
            inventory["successful_assertion_ids"] = ["one"]
            (root / "sql/assertion-inventory.json").write_text(json.dumps(inventory))
            self.assertEqual(self.aggregate(root), 1)

    def test_missing_gpu_oom_evidence_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            evidence = json.loads(
                (root / "cpp/gpu-correctness-evidence.json").read_text()
            )
            evidence["oom_invariant_passed"] = False
            (root / "cpp/gpu-correctness-evidence.json").write_text(
                json.dumps(evidence)
            )
            self.assertEqual(self.aggregate(root), 1)

    def test_gpu_evidence_must_match_retained_ctest_log(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            (root / "cpp/ctest.log").write_text(
                "Test #42: test_oom_invariant .... Not Run (Disabled)\n",
                encoding="utf-8",
            )
            self.reseal(root, "cpp")
            self.assertEqual(self.aggregate(root), 1)

    def test_missing_per_test_gpu_log_is_rejected_after_reseal(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            victim = next((root / "cpp/per-test-logs").glob("test_device-*.log"))
            victim.unlink()
            self.reseal(root, "cpp")
            self.assertEqual(self.aggregate(root), 1)

    def test_empty_or_mislabeled_per_test_gpu_log_is_rejected_after_reseal(
        self,
    ) -> None:
        for mutation in ("empty", "mislabeled"):
            with (
                self.subTest(mutation=mutation),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                self.initialize_valid_gate(root)
                victim = next((root / "cpp/per-test-logs").glob("test_device-*.log"))
                if mutation == "empty":
                    victim.write_text("", encoding="utf-8")
                else:
                    victim.write_text(
                        victim.read_text(encoding="utf-8").replace(
                            "name=test_device", "name=test_wrong"
                        ),
                        encoding="utf-8",
                    )
                self.reseal(root, "cpp")
                self.assertEqual(self.aggregate(root), 1)

    def test_raw_evidence_absence_and_hash_mismatch_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            (root / "rust/profiles/fixture.profraw").unlink()
            self.assertEqual(self.aggregate(root), 1)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            with (root / "cpp/raw-lcov.info").open("a", encoding="utf-8") as handle:
                handle.write("TN:tampered\n")
            self.assertEqual(self.aggregate(root), 1)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            (root / "cpp/profiles/fixture.proftext").unlink()
            self.assertEqual(
                quiet_call(
                    coverage_tools.seal_layer_evidence,
                    argparse.Namespace(artifact_dir=str(root), layer="cpp"),
                ),
                1,
            )
            self.assertEqual(self.aggregate(root), 1)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            (root / "cpp/profiles/device.overflow").write_text(
                "counter overflow\n", encoding="utf-8"
            )
            self.assertEqual(
                quiet_call(
                    coverage_tools.seal_layer_evidence,
                    argparse.Namespace(artifact_dir=str(root), layer="cpp"),
                ),
                1,
            )
            self.assertEqual(self.aggregate(root), 1)

    def test_malformed_lcov_json_and_profdata_fail_after_reseal(self) -> None:
        mutations = (
            ("rust", "raw-lcov.info", "not lcov\n"),
            ("cpp", "raw-coverage.json", "{}\n"),
            ("rust", "coverage.profdata", "MALFORMED\n"),
        )
        for layer, relative, contents in mutations:
            with (
                self.subTest(layer=layer, relative=relative),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                self.initialize_valid_gate(root)
                (root / layer / relative).write_text(contents, encoding="utf-8")
                self.reseal(root, layer)
                self.assertEqual(self.aggregate(root), 1)

    def test_toolchain_major_mismatch_is_rejected_after_reseal(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            path = root / "cpp/toolchain.json"
            toolchain = coverage_tools.read_json(path)
            toolchain["tools"]["llvm_cov"]["major"] = 19
            coverage_tools.write_json(path, toolchain)
            self.reseal(root, "cpp")
            self.assertEqual(self.aggregate(root), 1)

    def test_dirty_deadbeef_and_scope_drift_provenance_are_rejected(self) -> None:
        for mutation in (
            "dirty",
            "deadbeef",
            "scope",
            "adaptivecpp_patch",
            "checkout_dirty",
        ):
            with (
                self.subTest(mutation=mutation),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                self.initialize_valid_gate(root)
                path = root / "provenance.json"
                provenance = coverage_tools.read_json(path)
                if mutation == "dirty":
                    provenance.update(tree="dirty", passed=False, errors=["dirty"])
                elif mutation == "deadbeef":
                    provenance["commit"] = "deadbeef" + "0" * 32
                elif mutation == "scope":
                    scope_path = root / "scope.json"
                    scope = coverage_tools.read_json(scope_path)
                    scope["minimum_percent"] = 91.0
                    coverage_tools.write_json(scope_path, scope)
                    provenance["scope_sha256"] = coverage_tools.sha256(scope_path)
                    for layer in ("rust", "cpp", "sql"):
                        evidence_path = root / layer / "raw-evidence.json"
                        evidence = coverage_tools.read_json(evidence_path)
                        evidence["scope_sha256"] = provenance["scope_sha256"]
                        coverage_tools.write_json(evidence_path, evidence)
                elif mutation == "adaptivecpp_patch":
                    (root / "adaptivecpp-sscp-host-coverage.patch").write_text(
                        "tampered\n", encoding="utf-8"
                    )
                else:
                    (root / "checkout/untracked.txt").write_text(
                        "dirty\n", encoding="utf-8"
                    )
                coverage_tools.write_json(path, provenance)
                self.assertEqual(self.aggregate(root), 1)

    def test_self_consistent_green_summary_cannot_override_raw_totals(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            path = root / "rust/layer-summary.json"
            summary = coverage_tools.read_json(path)
            summary.update(
                covered_units=1,
                total_units=1,
                uncovered_units=0,
                percent=100.0,
                covered_lines=1,
                line_count=1,
                uncovered_lines=0,
                line_percent=100.0,
                passed=True,
                errors=[],
            )
            summary["mapping"] = {
                "owned_files": 1,
                "required_files": 1,
                "mapped_files": 1,
                "missing_required_files": [],
                "unexpected_owned_report_files": [],
            }
            coverage_tools.write_json(path, summary)
            self.assertEqual(self.aggregate(root), 1)

    def test_nonfinite_summary_json_still_produces_gate_summary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            path = root / "rust/layer-summary.json"
            path.write_text(
                path.read_text().replace('"percent": 100.0', '"percent": NaN', 1),
                encoding="utf-8",
            )
            self.assertEqual(self.aggregate(root), 1)
            self.assertTrue((root / "gate-summary.json").is_file())


class ImmutableBaselineTests(unittest.TestCase):
    def audit(self, scope: pathlib.Path, baseline: pathlib.Path) -> int:
        return quiet_call(
            coverage_tools.audit_scope,
            argparse.Namespace(
                scope=str(scope),
                baseline=str(baseline),
                repo_root=str(REPO_ROOT),
            ),
        )

    def write_matching_baseline(
        self, root: pathlib.Path, scope: pathlib.Path
    ) -> pathlib.Path:
        path = root / "baseline.json"
        coverage_tools.write_json(
            path,
            coverage_tools.release_baseline_document(
                REPO_ROOT,
                scope,
                REPO_ROOT / "coverage/sql-semantic-assertions.json",
            ),
        )
        return path

    def test_release_baseline_update_requires_explicit_acknowledgement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            args = argparse.Namespace(
                repo_root=str(REPO_ROOT),
                scope=str(REPO_ROOT / "coverage/scope.json"),
                manifest=str(REPO_ROOT / "coverage/sql-semantic-assertions.json"),
                output=str(root / "baseline.json"),
                acknowledge_review_visible_update="no",
            )
            with self.assertRaises(coverage_tools.CoverageError):
                coverage_tools.update_release_baseline(args)
            args.acknowledge_review_visible_update = "UPDATE-RELEASE-BASELINE"
            self.assertEqual(
                quiet_call(coverage_tools.update_release_baseline, args), 0
            )
            self.assertEqual(
                coverage_tools.read_json(pathlib.Path(args.output)),
                coverage_tools.read_json(REPO_ROOT / "coverage/release-baseline.json"),
            )

    def test_rust_build_script_and_nonempty_owned_scope_are_pinned(self) -> None:
        mutations = ("remove_build", "exclude_all")
        for mutation in mutations:
            with (
                self.subTest(mutation=mutation),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                scope = coverage_tools.read_json(REPO_ROOT / "coverage/scope.json")
                if mutation == "remove_build":
                    scope["layers"]["rust"]["roots"].remove("pg_accel/build.rs")
                else:
                    scope["layers"]["rust"]["exclude"] = ["**/*.rs"]
                scope_path = root / "scope.json"
                coverage_tools.write_json(scope_path, scope)
                if mutation == "remove_build":
                    baseline = self.write_matching_baseline(root, scope_path)
                else:
                    baseline = REPO_ROOT / "coverage/release-baseline.json"
                with self.assertRaises(coverage_tools.CoverageError):
                    self.audit(scope_path, pathlib.Path(baseline))

    def test_cpp_headers_only_scope_is_rejected_even_with_matching_baseline(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            scope = coverage_tools.read_json(REPO_ROOT / "coverage/scope.json")
            scope["layers"]["cpp"]["exclude"] = ["pgaccel-kernels/src/**"]
            scope_path = root / "scope.json"
            coverage_tools.write_json(scope_path, scope)
            baseline = self.write_matching_baseline(root, scope_path)
            with self.assertRaises(coverage_tools.CoverageError):
                self.audit(scope_path, baseline)

    def test_cpp_include_root_removal_is_rejected_with_matching_baseline(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            scope = coverage_tools.read_json(REPO_ROOT / "coverage/scope.json")
            scope["layers"]["cpp"]["roots"].remove("pgaccel-kernels/include")
            scope_path = root / "scope.json"
            coverage_tools.write_json(scope_path, scope)
            baseline = self.write_matching_baseline(root, scope_path)
            with self.assertRaises(coverage_tools.CoverageError):
                self.audit(scope_path, baseline)

    def test_cpp_executable_header_membership_cannot_be_removed(self) -> None:
        for field in ("executable_headers", "required_mapping_files"):
            with (
                self.subTest(field=field),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                baseline = coverage_tools.read_json(
                    REPO_ROOT / "coverage/release-baseline.json"
                )
                header = baseline["cpp"]["executable_headers"][0]
                if field == "executable_headers":
                    baseline["cpp"][field].remove(header)
                else:
                    baseline["cpp"][field].remove(header)
                baseline_path = root / "baseline.json"
                coverage_tools.write_json(baseline_path, baseline)
                with self.assertRaises(coverage_tools.CoverageError):
                    self.audit(REPO_ROOT / "coverage/scope.json", baseline_path)

    def test_cpp_header_file_removal_fails_with_matching_regenerated_baseline(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            original_inventory = coverage_tools.source_inventory
            removed = "pgaccel-kernels/include/alloc_helper.h"

            def inventory_without_header(repo_root, scope):
                included, required = original_inventory(repo_root, scope)
                if "pgaccel-kernels/include" in scope.get("roots", []):
                    included.discard(removed)
                    required.discard(removed)
                return included, required

            with mock.patch.object(
                coverage_tools,
                "source_inventory",
                side_effect=inventory_without_header,
            ):
                baseline = self.write_matching_baseline(
                    root, REPO_ROOT / "coverage/scope.json"
                )
                with self.assertRaises(coverage_tools.CoverageError):
                    self.audit(REPO_ROOT / "coverage/scope.json", baseline)

    def test_cpp_executable_header_forms_are_detected(self) -> None:
        executable = (
            "struct Probe { Probe() : value{} { value = 42; } int value; };\n",
            "struct Probe { ~Probe() { cleanup(); } };\n",
            "struct Probe { int operator()(int x) const { return x; } };\n",
            "template <typename T> T convert(T value) { return value; }\n",
            "inline auto callback = [] { return 42; };\n",
            "#define PGACCEL_PROBE(value) do { consume(value); } while (0)\n",
        )
        declarations_only = (
            "struct Probe { Probe(); ~Probe(); int value; };\n",
            "int declared_only(int value);\n",
            "inline constexpr int values[] {1, 2, 3};\n",
            '// fake() { return 1; }\nconst char* text = "fake() {";\n',
            "#define PGACCEL_SIZE(value) sizeof(value)\nstruct Plain { int value; };\n",
        )
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for index, source in enumerate(executable):
                path = root / f"executable_{index}.hpp"
                path.write_text(source, encoding="utf-8")
                self.assertTrue(
                    coverage_tools.has_executable_mapping_candidate(path), source
                )
            for index, source in enumerate(declarations_only):
                path = root / f"declaration_{index}.hpp"
                path.write_text(source, encoding="utf-8")
                self.assertFalse(
                    coverage_tools.has_executable_mapping_candidate(path), source
                )

    def test_new_constructor_header_fails_after_matching_baseline_refresh(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            original_inventory = coverage_tools.source_inventory
            added = "pgaccel-kernels/include/new_constructor.hpp"

            def inventory_with_constructor(repo_root, scope):
                included, required = original_inventory(repo_root, scope)
                if "pgaccel-kernels/include" in scope.get("roots", []):
                    included.add(added)
                    required.add(added)
                return included, required

            with mock.patch.object(
                coverage_tools,
                "source_inventory",
                side_effect=inventory_with_constructor,
            ):
                baseline = self.write_matching_baseline(
                    root, REPO_ROOT / "coverage/scope.json"
                )
                document = coverage_tools.read_json(baseline)
                self.assertIn(added, document["cpp"]["executable_headers"])
                self.assertIn(added, document["cpp"]["required_mapping_files"])
                with self.assertRaises(coverage_tools.CoverageError):
                    self.audit(REPO_ROOT / "coverage/scope.json", baseline)

    def test_ctest_sql_filename_and_sql_id_baseline_mutations_are_rejected(
        self,
    ) -> None:
        mutations = (
            ("cpp", "ctest_names"),
            ("sql", "files"),
            ("sql", "assertion_ids"),
        )
        for section, field in mutations:
            with (
                self.subTest(section=section, field=field),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                baseline = coverage_tools.read_json(
                    REPO_ROOT / "coverage/release-baseline.json"
                )
                baseline[section][field].pop()
                baseline_path = root / "baseline.json"
                coverage_tools.write_json(baseline_path, baseline)
                with self.assertRaises(coverage_tools.CoverageError):
                    self.audit(REPO_ROOT / "coverage/scope.json", baseline_path)


class RustCompilerMappingTests(unittest.TestCase):
    def test_dep_info_classifies_zero_region_and_test_only_sources(self) -> None:
        required = {"src/live.rs", "src/constants.rs", "src/test_stub.rs"}
        production = {"src/live.rs", "src/constants.rs"}
        configuration = required
        classified, pending = coverage_tools.classify_rust_unmapped(
            required, {"src/live.rs"}, production, configuration
        )
        self.assertEqual(pending, [])
        self.assertEqual(
            classified,
            [
                {
                    "path": "src/constants.rs",
                    "reason": "compiler_dependency_without_llvm_coverage_region",
                },
                {
                    "path": "src/test_stub.rs",
                    "reason": "non_production_configuration_only",
                },
            ],
        )

    def test_hidden_executable_source_without_compiler_evidence_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            hidden = pathlib.Path(directory) / "hidden.rs"
            hidden.write_text("pub fn hidden() -> bool { true }\n", encoding="utf-8")
            self.assertTrue(coverage_tools.has_executable_mapping_candidate(hidden))
            with self.assertRaisesRegex(
                coverage_tools.CoverageError,
                "absent from compiler dependency evidence",
            ):
                coverage_tools.classify_rust_unmapped(
                    {"src/hidden.rs"}, set(), set(), set()
                )

    def test_compiler_observed_mapping_cannot_disappear_from_retained_objects(
        self,
    ) -> None:
        with self.assertRaisesRegex(
            coverage_tools.CoverageError, "compiler-observed source mappings"
        ):
            coverage_tools.require_retained_compiler_mappings(
                {"src/live.rs"}, set()
            )

    def test_dep_info_inventory_is_compiler_derived(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "src/live.rs"
            source.parent.mkdir()
            source.write_text("pub fn live() {}\n", encoding="utf-8")
            dep_info = root / "target/debug/live.d"
            dep_info.parent.mkdir(parents=True)
            dep_info.write_text(
                f"{root / 'target/debug/live'}: {source}\n", encoding="utf-8"
            )
            entries = coverage_tools.collect_rust_dep_info(
                root / "target", root, {"src/live.rs"}
            )
            self.assertEqual(entries, [(dep_info.resolve(), ["src/live.rs"])])


class ArtifactAndToolchainTests(unittest.TestCase):
    def test_initial_artifacts_are_schema_valid_and_red(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            status = quiet_call(
                coverage_tools.init_artifacts,
                argparse.Namespace(
                    artifact_dir=str(root),
                    rust_threshold=90.0,
                    cpp_threshold=90.0,
                    sql_threshold=90.0,
                ),
            )
            self.assertEqual(status, 0)
            for layer in ("rust", "cpp", "sql"):
                summary = json.loads((root / layer / "layer-summary.json").read_text())
                self.assertEqual(summary["schema_version"], 2)
                self.assertFalse(summary["passed"])
                self.assertEqual(summary["execution"]["status"], "not_run")
            self.assertTrue(
                (root / "sql-reachability/reachability-summary.json").is_file()
            )

    def test_gpu_filter_writes_nonempty_exact_result_envelope(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / "test_empty.log"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(REPO_ROOT / "scripts/filter_gpu_output.py"),
                    "--label",
                    "test_empty",
                    "--log",
                    str(log),
                    "--",
                    "/usr/bin/true",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0)
            self.assertEqual(
                log.read_text(encoding="utf-8"),
                gpu_test_log_text("test_empty", ""),
            )

    def test_llvm_major_parser_accepts_clang_and_llvm(self) -> None:
        self.assertEqual(
            coverage_tools.extract_llvm_major("Apple clang version 20.1.0"), 20
        )
        self.assertEqual(coverage_tools.extract_llvm_major("LLVM version 20.1.8"), 20)
        with self.assertRaises(coverage_tools.CoverageError):
            coverage_tools.extract_llvm_major("unknown tool")

    def test_toolchain_rejects_profdata_major_mismatch(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tools = {}
            for name, output in (
                ("clang", "clang version 20.1.0"),
                ("llvm-cov", "LLVM version 20.1.0"),
                ("llvm-profdata", "LLVM version 19.1.0"),
            ):
                path = root / name
                path.write_text(f"#!/bin/sh\necho '{output}'\n", encoding="utf-8")
                path.chmod(0o755)
                tools[name] = str(path)
            status = quiet_call(
                coverage_tools.validate_toolchain,
                argparse.Namespace(
                    clang=tools["clang"],
                    llvm_cov=tools["llvm-cov"],
                    llvm_profdata=tools["llvm-profdata"],
                    output=str(root / "toolchain.json"),
                ),
            )
            self.assertEqual(status, 1)

    def test_device_profile_audit_rejects_intrinsic_leak(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            object_root = root / "build/CMakeFiles/pgaccel_kernels_shared.dir/src"
            object_root.mkdir(parents=True)
            objects = []
            for index in range(coverage_tools.BASELINE_CPP_SOURCES):
                path = object_root / f"kernel_{index:02d}.cpp.o"
                path.write_bytes(f"host-object-{index}\n".encode("ascii"))
                objects.append(str(path))
            args = argparse.Namespace(
                object=objects,
                object_root=str(object_root.parent),
                object_dir=str(root / "cpp/device-objects"),
                output=str(root / "cpp/device-profile-audit.json"),
            )

            self.assertEqual(quiet_call(coverage_tools.audit_device_profile_intrinsics, args), 0)
            pathlib.Path(objects[7]).write_bytes(
                b"device bitcode llvm.instrprof.increment leaked\n"
            )
            self.assertEqual(quiet_call(coverage_tools.audit_device_profile_intrinsics, args), 1)
            evidence = coverage_tools.read_json(pathlib.Path(args.output))
            self.assertFalse(evidence["passed"])
            self.assertEqual(evidence["objects"][7]["intrinsic_occurrences"], 1)

    def test_adaptivecpp_patch_cannot_drop_nonempty_orphan_mappings(self) -> None:
        patch = (
            REPO_ROOT / "patches/adaptivecpp/sscp-host-coverage.patch"
        ).read_text(encoding="utf-8")
        self.assertEqual(coverage_tools.adaptivecpp_coverage_patch_errors(patch), [])

        missing_restore = patch.replace(
            "restoreHostCoverageMappingNames(M, HasCoverageMappingNames)",
            "dropHostCoverageMappingNames(M)",
        )
        self.assertTrue(
            coverage_tools.adaptivecpp_coverage_patch_errors(missing_restore)
        )
        dropped_record = patch + "\n+  __covrec_nonempty->eraseFromParent();\n"
        self.assertTrue(
            coverage_tools.adaptivecpp_coverage_patch_errors(dropped_record)
        )
        dropped_device_counters = patch.replace(
            "lowerDeviceProfileInstrumentation(*DeviceModule);",
            "stripHostProfileInstrumentation(*DeviceModule);",
        )
        self.assertTrue(
            coverage_tools.adaptivecpp_coverage_patch_errors(dropped_device_counters)
        )
        for original, replacement in (
            (
                '"acpp.metal.device.profile.batch"',
                '"acpp.metal.device.profile.step"',
            ),
            ("EntryBuilder.CreateAlloca(", "EntryBuilder.CreateCall("),
            ("Builder.CreateICmpULT(Sum, Old)", "Builder.CreateICmpEQ(Sum, Old)"),
            (
                "BatchInputs.push_back(llvm::ConstantInt::get(I32, Slot + 1))",
                "BatchInputs.push_back(llvm::ConstantInt::get(I32, Slot))",
            ),
            ('os << " [[buffer(30)]]"', 'os << " [[buffer(29)]]"'),
            ("metal_device_profile_buffer_index = 30", "metal_device_profile_buffer_index = 29"),
            ('Name.consume_front("\\1")', 'Name.consume_front("_")'),
            (
                "if (!any_nonzero && !overflow) return;",
                "if (!any_nonzero) return;",
            ),
            ("std::_Exit(EXIT_FAILURE);", "return;"),
            (
                "if (common::filesystem::atomic_write(path, data)) return;",
                "common::filesystem::atomic_write(path, data); return;",
            ),
            (
                'fail_device_profile_flush("invalid device profile counter buffer")',
                "return",
            ),
            (
                "if (!std::filesystem::is_directory(output_dir, ec) || ec)",
                "if (false)",
            ),
            ('"device profile overflow marker"', '"device profile warning"'),
        ):
            with self.subTest(missing_invariant=original):
                mutated = patch.replace(original, replacement)
                self.assertNotEqual(mutated, patch)
                self.assertTrue(
                    coverage_tools.adaptivecpp_coverage_patch_errors(mutated)
                )

    def test_device_profile_overflow_only_fixture_is_hostile(self) -> None:
        fixture = (
            REPO_ROOT
            / "scripts/tests/fixtures/acpp_device_profile_overflow_only.cpp"
        ).read_text(encoding="utf-8")
        self.assertIn("no_profile_instrument_function", fixture)
        self.assertIn('asm("llvm.instrprof.increment.step")', fixture)
        self.assertIn("UINT64_C(0x100000000)", fixture)
        self.assertIn("metal_overflow_only_probe", fixture)
        self.assertIn("metal_profile_flush_probe", fixture)
        self.assertIn('mode == "ordinary"', fixture)

        dormancy_fixture = (
            REPO_ROOT / "scripts/tests/fixtures/acpp_device_profile_dormancy.cpp"
        ).read_text(encoding="utf-8")
        self.assertIn("DeviceProfileDormancyKernel", dormancy_fixture)
        self.assertNotIn("fprofile-instr-generate", dormancy_fixture)

        runner = (
            REPO_ROOT / "scripts/tests/run_acpp_device_profile_overflow_only.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("mktemp -d", runner)
        self.assertIn('ACPP_METAL_DEVICE_PROFILE_DIR="$output_path"', runner)
        self.assertIn('"$overflow_count" != 1', runner)
        self.assertIn('"$proftext_count" != 0', runner)
        self.assertIn("expect_flush_failure regular-file", runner)
        self.assertIn("expect_flush_failure unwritable-overflow", runner)
        self.assertIn("expect_flush_failure unwritable-proftext", runner)
        self.assertIn('"$status" == 0 || "$retained_proftext" != 0', runner)
        self.assertIn('"$acpp" -O2 "$dormancy_fixture"', runner)
        self.assertIn("normal-build device profile dormancy: PASS (files=0)", runner)

        gate = (REPO_ROOT / "scripts/coverage_gate.sh").read_text(encoding="utf-8")
        self.assertIn("run_acpp_device_profile_overflow_only.sh", gate)
        self.assertIn("record_stage cpp device_profile_overflow_only 0", gate)
        self.assertIn("record_stage cpp device_profile_overflow_only 1", gate)
        self.assertIn("execution_status=1", gate)

    def test_device_profile_overflow_only_runner_requires_exact_artifacts(self) -> None:
        runner = REPO_ROOT / "scripts/tests/run_acpp_device_profile_overflow_only.sh"
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fake_acpp = root / "acpp"

            def install_fake_probe(probe: str) -> None:
                fake_acpp.write_text(
                    "#!/usr/bin/env python3\n"
                    "import pathlib, sys\n"
                    "output = pathlib.Path(sys.argv[sys.argv.index('-o') + 1])\n"
                    f"profile_probe = {probe!r}\n"
                    "dormancy_probe = '#!/usr/bin/env bash\\nexit 0\\n'\n"
                    "payload = dormancy_probe if any("
                    "'acpp_device_profile_dormancy.cpp' in arg for arg in sys.argv"
                    ") else profile_probe\n"
                    "output.write_text(payload, encoding='utf-8')\n"
                    "output.chmod(0o755)\n",
                    encoding="utf-8",
                )
                fake_acpp.chmod(0o755)

            environment = os.environ.copy()
            environment["ACPP"] = str(fake_acpp)
            environment.pop("ACPP_TEST_DYLD_LIBRARY_PATH", None)
            install_fake_probe(
                "#!/usr/bin/env bash\n"
                "if [[ \"$ACPP_METAL_DEVICE_PROFILE_DIR\" == */success/profiles ]]; then\n"
                "  printf 'overflow\\n' > "
                '"$ACPP_METAL_DEVICE_PROFILE_DIR/device.overflow"\n'
                "  exit 0\n"
                "fi\n"
                "exit 9\n"
            )
            passed = subprocess.run(
                [str(runner)], check=False, capture_output=True, text=True,
                env=environment,
            )
            self.assertEqual(passed.returncode, 0, passed.stderr)
            self.assertIn("overflow=1 proftext=0", passed.stdout)

            install_fake_probe(
                "#!/usr/bin/env bash\n"
                "if [[ \"$ACPP_METAL_DEVICE_PROFILE_DIR\" == */success/profiles ]]; then\n"
                "  printf 'overflow\\n' > "
                '"$ACPP_METAL_DEVICE_PROFILE_DIR/device.overflow"\n'
                "  printf 'profile\\n' > "
                '"$ACPP_METAL_DEVICE_PROFILE_DIR/device.proftext"\n'
                "  exit 0\n"
                "fi\n"
                "exit 9\n"
            )
            rejected = subprocess.run(
                [str(runner)], check=False, capture_output=True, text=True,
                env=environment,
            )
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("overflow=1 proftext=1", rejected.stderr)

    def test_gpu_evidence_requires_oom_test_to_report_passed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            log = root / "ctest.log"
            output = root / "evidence.json"
            per_test = root / "per-test"
            per_test.mkdir()
            names = [f"test_{index:02d}" for index in range(28)] + [
                "test_oom_invariant"
            ]
            families = [
                "reduce_f64",
                "sort_f64",
                "hashagg_f64",
                "spatial_f64",
                "h3_f64",
            ]
            baseline = root / "baseline.json"
            coverage_tools.write_json(
                baseline,
                {
                    "cpp": {
                        "ctest_names": names,
                        "ctest_evidence": {
                            name: (
                                "device-family-dispatch-oom"
                                if name == "test_oom_invariant"
                                else "execution"
                            )
                            for name in names
                        },
                        "oom_families": families,
                    }
                },
            )
            valid_oom_body = ""
            for name in names:
                text = gpu_test_log_text(name, f"pass {name}\n")
                if name == "test_oom_invariant":
                    body = (
                        'PGACCEL_DEVICE_PROOF device="fixture" backend="cuda" '
                        "compute_units=1 max_alloc_bytes=100 real_device=1\n"
                        + "\n".join(
                            "PGACCEL_OOM_FAMILY "
                            f"family={family} result=PASS dispatches=1 "
                            "peak_rss_bytes=100 rss_baseline_bytes=90 "
                            "rss_delta_bytes=10 rss_limit_bytes=300"
                            for family in families
                        )
                        + "\nPGACCEL_OOM_INVARIANT result=PASS families=5 "
                        "max_alloc_bytes=100 input_doubles=25 rss_limit_bytes=300\n"
                    )
                    valid_oom_body = body
                    text = gpu_test_log_text(name, body)
                (per_test / f"{name}-fixture.log").write_text(text, encoding="utf-8")
            args = argparse.Namespace(
                execution_status=0,
                ctest_log=str(log),
                per_test_log_dir=str(per_test),
                baseline=str(baseline),
                output=str(output),
            )
            log.write_text(
                "\n".join(
                    f"Test #{index}: {name} .... "
                    + (
                        "Not Run (Disabled)"
                        if name == "test_oom_invariant"
                        else "Passed 0.01 sec"
                    )
                    for index, name in enumerate(names, start=1)
                )
                + "\n",
                encoding="utf-8",
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            valid_ctest = ctest_pass_log(names)
            log.write_text(valid_ctest, encoding="utf-8")
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 0)

            first_name, second_name = names[:2]
            first_log = per_test / f"{first_name}-fixture.log"
            second_log = per_test / f"{second_name}-fixture.log"
            valid_first = first_log.read_text(encoding="utf-8")
            valid_second = second_log.read_text(encoding="utf-8")
            first_log.write_text(gpu_test_log_text(first_name, ""), encoding="utf-8")
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            replayed_body = "replayed body\n"
            first_log.write_text(
                gpu_test_log_text(first_name, replayed_body), encoding="utf-8"
            )
            second_log.write_text(
                gpu_test_log_text(second_name, replayed_body), encoding="utf-8"
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            first_log.write_text(valid_first, encoding="utf-8")
            second_log.write_text(valid_second, encoding="utf-8")

            extra_log = per_test / "unexpected-fixture.log"
            extra_log.write_text(
                gpu_test_log_text("unexpected", "unexpected body\n"),
                encoding="utf-8",
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            extra_log.unlink()
            nested = per_test / "nested"
            nested.mkdir()
            nested_extra = nested / "test_00-replayed.log"
            nested_extra.write_text(
                gpu_test_log_text("test_00", "nested replay\n"), encoding="utf-8"
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            nested_extra.unlink()
            nested.rmdir()
            log.write_text(
                valid_ctest.replace(
                    "100% tests passed, 0 tests failed out of 29",
                    "100% tests passed, 0 tests failed out of 28",
                ),
                encoding="utf-8",
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            log.write_text(
                valid_ctest + "50% tests passed, 1 tests failed out of 29\n",
                encoding="utf-8",
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            log.write_text(
                valid_ctest.replace("1/29 Test #1", "2/29 Test #1", 1),
                encoding="utf-8",
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            log.write_text(
                valid_ctest + " 50% tests passed, 1 tests failed out of 29\n",
                encoding="utf-8",
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            log.write_text(valid_ctest, encoding="utf-8")

            first_log.write_text(
                gpu_test_log_text(
                    first_name, f"pass {first_name}\n", exit_code=1, result="FAIL"
                ),
                encoding="utf-8",
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            first_log.write_text(valid_first, encoding="utf-8")

            oom_log = per_test / "test_oom_invariant-fixture.log"
            self.assertTrue(valid_oom_body)

            def write_oom_body(value: str) -> None:
                oom_log.write_text(
                    gpu_test_log_text("test_oom_invariant", value),
                    encoding="utf-8",
                )

            for old, new in (
                ("input_doubles=25", "input_doubles=24"),
                ("rss_limit_bytes=300", "rss_limit_bytes=299"),
                (
                    "max_alloc_bytes=100 real_device=1",
                    "max_alloc_bytes=99 real_device=1",
                ),
                ("compute_units=1", "compute_units=4294967296"),
                (
                    "max_alloc_bytes=100 real_device=1",
                    "max_alloc_bytes=18446744073709551616 real_device=1",
                ),
                (
                    "dispatches=1",
                    "dispatches=18446744073709551616",
                ),
                ("families=5", "families=18446744073709551616"),
                ("input_doubles=25", "input_doubles=18446744073709551616"),
                (
                    "peak_rss_bytes=100 rss_baseline_bytes=90 "
                    "rss_delta_bytes=10 rss_limit_bytes=300",
                    "peak_rss_bytes=301 rss_baseline_bytes=291 "
                    "rss_delta_bytes=10 rss_limit_bytes=300",
                ),
                (
                    "peak_rss_bytes=100 rss_baseline_bytes=90 rss_delta_bytes=10",
                    "peak_rss_bytes=100 rss_baseline_bytes=101 rss_delta_bytes=0",
                ),
                ("rss_baseline_bytes=90", "rss_baseline_bytes=0"),
                ("rss_delta_bytes=10", "rss_delta_bytes=9"),
                ('backend="cuda"', 'backend="definitely-not-cuda-host"'),
            ):
                with self.subTest(arithmetic=f"{old}->{new}"):
                    write_oom_body(valid_oom_body.replace(old, new, 1))
                    self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            write_oom_body(valid_oom_body.replace("dispatches=1", "dispatches=0", 1))
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            write_oom_body("PGACCEL_UNSUPPORTED\n" + valid_oom_body)
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            write_oom_body(valid_oom_body)
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 0)


if __name__ == "__main__":
    unittest.main()
