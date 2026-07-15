from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import json
import pathlib
import tempfile
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "coverage_tools.py"
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
                    "exclude_cfg_test_items": True,
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
        self, root: pathlib.Path, records: dict[pathlib.Path, dict[int, int]]
    ) -> pathlib.Path:
        lines = ["TN:"]
        for source, hits in records.items():
            lines.append(f"SF:{source}")
            lines.extend(f"DA:{line},{count}" for line, count in sorted(hits.items()))
            lines.append("end_of_record")
        path = root / "raw.info"
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

    def test_huge_inline_cfg_test_module_is_excluded_structurally(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "crate/src/lib.rs"
            source.parent.mkdir(parents=True)
            huge = "\n".join(
                f"fn generated_{i}() {{ assert_eq!({i}, {i}); }}" for i in range(6000)
            )
            text = (
                "fn production() -> usize { 1 }\n"
                "#[cfg(test)]\n"
                "mod tests {\n"
                '  const BRACES: &str = r###" }}} /* not syntax */ "###;\n'
                "  /* nested comment { /* } */ } */\n"
                f"{huge}\n"
                "}\n"
                "fn after() -> usize { 2 }\n"
            )
            source.write_text(text, encoding="utf-8")
            ranges = coverage_tools.rust_cfg_test_ranges(text)
            self.assertEqual(len(ranges), 1)
            self.assertEqual(ranges[0][0], 2)
            self.assertGreater(ranges[0][1], 6000)
            final_line = text.count("\n")
            scope = self.write_scope(root)
            report = self.write_lcov(
                root,
                {source: {1: 1, 2: 0, 3: 0, 1000: 0, final_line: 1}},
            )

            status = quiet_call(
                coverage_tools.summarize_layer,
                self.rust_args(root, scope, report),
            )

            self.assertEqual(status, 0)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(summary["line_count"], 2)
            self.assertEqual(summary["covered_lines"], 2)
            self.assertEqual(summary["excluded_cfg_test_lines"], 3)

    def test_cfg_parser_does_not_hide_not_or_optional_test_code(self) -> None:
        text = (
            "#[cfg(not(test))]\nfn production() {}\n"
            '#[cfg(any(test, feature = "fixture"))]\nfn maybe_production() {}\n'
            '#[cfg(all(test, feature = "fixture"))]\nmod only_test { fn helper() {} }\n'
        )
        ranges = coverage_tools.rust_cfg_test_ranges(text)
        self.assertEqual(ranges, [(5, 6)])

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
            "DO $$ BEGIN\n"
            "  IF 1 <> 1 THEN RAISE EXCEPTION 'bad'; END IF;\n"
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
                "DO $$ BEGIN\n"
                "  IF 1 <> 1 THEN RAISE EXCEPTION 'bad'; END IF;\n"
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
                "DO $$ BEGIN\n"
                "  IF 1 <> 1 THEN RAISE EXCEPTION 'bad'; END IF;\n"
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
        for layer in ("rust", "cpp"):
            summary = coverage_tools.initial_layer_summary(layer, 90.0)
            summary.update(
                {
                    "covered_units": 90,
                    "total_units": 100,
                    "uncovered_units": 10,
                    "percent": 90.0,
                    "covered_lines": 90,
                    "line_count": 100,
                    "uncovered_lines": 10,
                    "line_percent": 90.0,
                    "excluded_cfg_test_lines": 0,
                    "mapping": {
                        "owned_files": 1,
                        "required_files": 1,
                        "mapped_files": 1,
                        "missing_required_files": [],
                        "unexpected_owned_report_files": [],
                    },
                    "execution": {
                        "status": "complete",
                        "exit_code": 0,
                        "stages_complete": True,
                    },
                    "errors": [],
                    "passed": True,
                }
            )
            coverage_tools.write_json(root / layer / "layer-summary.json", summary)
            self.write_complete_stage(root, layer)
        sql = coverage_tools.initial_layer_summary("sql", 90.0)
        manifest = coverage_tools.initial_manifest_state()
        manifest.update(
            {
                "valid": True,
                "sha256": "a" * 64,
                "declared_files": 52,
                "declared_assertions": 285,
                "completed_files": 52,
                "passed_test_files": 52,
                "test_files": 52,
            }
        )
        sql.update(
            {
                "covered_units": 285,
                "total_units": 285,
                "uncovered_units": 0,
                "percent": 100.0,
                "covered_assertions": 285,
                "assertion_count": 285,
                "uncovered_assertions": 0,
                "assertion_percent": 100.0,
                "manifest": manifest,
                "execution": {
                    "status": "complete",
                    "exit_code": 0,
                    "stages_complete": True,
                },
                "errors": [],
                "passed": True,
            }
        )
        coverage_tools.write_json(root / "sql/layer-summary.json", sql)
        self.write_complete_stage(root, "sql")
        manifest_entries = []
        successful_ids = []
        file_rows = []
        logs_dir = root / "sql/test-run/logs"
        logs_dir.mkdir(parents=True)
        for file_index in range(52):
            stem = f"{file_index:02d}_fixture"
            name = f"{stem}.sql"
            assertion_count = 6 if file_index < 25 else 5
            identifiers = [
                f"{stem}.assert_{assertion_index:03d}"
                for assertion_index in range(1, assertion_count + 1)
            ]
            successful_ids.extend(identifiers)
            manifest_entries.append(
                {
                    "file": name,
                    "sha256": "b" * 64,
                    "completion_id": stem,
                    "assertions": [
                        {
                            "id": identifier,
                            "source_line": line_number,
                            "emission": "echo",
                        }
                        for line_number, identifier in enumerate(identifiers, start=1)
                    ],
                }
            )
            log_path = logs_dir / f"{name}.log"
            log_path.write_text(
                "\n".join(
                    [
                        *(f"PGACCEL_ASSERT_OK:{value}" for value in identifiers),
                        f"PGACCEL_FILE_OK:{stem}",
                    ]
                )
                + "\n",
                encoding="utf-8",
            )
            file_rows.append(
                {
                    "file": name,
                    "status": "pass",
                    "exit_code": 0,
                    "observed_assertions": assertion_count,
                    "completion_markers": 1,
                    "log": f"logs/{name}.log",
                    "log_sha256": coverage_tools.sha256(log_path),
                }
            )
        self.assertEqual(len(successful_ids), 285)
        coverage_tools.write_json(
            root / "sql-semantic-assertions.json",
            {
                "schema_version": 2,
                "kind": "sql-semantic-assertion-manifest",
                "test_root": "sql/tests",
                "baseline_files": 52,
                "baseline_assertions": 285,
                "declared_files": 52,
                "declared_assertions": 285,
                "files": manifest_entries,
            },
        )
        copied_hash = coverage_tools.sha256(root / "sql-semantic-assertions.json")
        sql_path = root / "sql/layer-summary.json"
        sql_document = json.loads(sql_path.read_text())
        sql_document["manifest"]["sha256"] = copied_hash
        sql_path.write_text(json.dumps(sql_document), encoding="utf-8")
        coverage_tools.write_json(
            root / "sql/assertion-inventory.json",
            {
                "schema_version": 2,
                "kind": "sql-semantic-assertion-inventory",
                "manifest_sha256": copied_hash,
                "declared_assertions": 285,
                "successful_assertions": 285,
                "assertion_percent": 100.0,
                "declared_files": 52,
                "passed_files": 52,
                "completed_files": 52,
                "successful_assertion_ids": sorted(successful_ids),
                "errors": [],
                "complete": True,
                "files": file_rows,
            },
        )
        ctest_log = root / "cpp/ctest.log"
        ctest_log.write_text(
            "Test #42: test_oom_invariant .... Passed 441.50 sec\n",
            encoding="utf-8",
        )
        coverage_tools.write_json(
            root / "cpp/gpu-correctness-evidence.json",
            {
                "schema_version": 2,
                "kind": "gpu-correctness-evidence",
                "status": "complete",
                "execution_status": 0,
                "ctest_log": "ctest.log",
                "ctest_log_sha256": coverage_tools.sha256(ctest_log),
                "oom_invariant_required": True,
                "oom_invariant_observed": True,
                "oom_invariant_passed": True,
                "passed": True,
            },
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
        return quiet_call(
            coverage_tools.aggregate,
            argparse.Namespace(artifact_dir=str(root)),
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
            self.assertEqual(self.aggregate(root), 1)

    def test_nonfinite_summary_json_still_produces_gate_summary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            self.initialize_valid_gate(root)
            path = root / "rust/layer-summary.json"
            path.write_text(
                path.read_text().replace('"percent": 90.0', '"percent": NaN'),
                encoding="utf-8",
            )
            self.assertEqual(self.aggregate(root), 1)
            self.assertTrue((root / "gate-summary.json").is_file())


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

    def test_gpu_evidence_requires_oom_test_to_report_passed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            log = root / "ctest.log"
            output = root / "evidence.json"
            args = argparse.Namespace(
                execution_status=0, ctest_log=str(log), output=str(output)
            )
            log.write_text(
                "Test #42: test_oom_invariant .... Not Run (Disabled)\n",
                encoding="utf-8",
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 1)
            log.write_text(
                "Test #42: test_oom_invariant .... Passed 441.50 sec\n",
                encoding="utf-8",
            )
            self.assertEqual(quiet_call(coverage_tools.gpu_evidence, args), 0)


if __name__ == "__main__":
    unittest.main()
