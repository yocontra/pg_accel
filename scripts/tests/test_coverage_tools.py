from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import json
import pathlib
import tempfile
import unittest


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


class CoverageToolsTests(unittest.TestCase):
    def write_scope(self, root: pathlib.Path) -> pathlib.Path:
        scope = {
            "schema_version": 1,
            "minimum_line_percent": 90.0,
            "layers": {
                "rust": {
                    "description": "test scope",
                    "roots": ["crate/src"],
                    "extensions": [".rs"],
                    "required_extensions": [".rs"],
                    "exclude": ["**/tests.rs"],
                }
            },
        }
        path = root / "scope.json"
        path.write_text(json.dumps(scope), encoding="utf-8")
        return path

    def write_llvm_json(
        self, root: pathlib.Path, entries: list[tuple[pathlib.Path, int, int]]
    ) -> pathlib.Path:
        document = {
            "data": [
                {
                    "files": [
                        {
                            "filename": str(path),
                            "summary": {
                                "lines": {
                                    "count": count,
                                    "covered": covered,
                                    "percent": covered * 100.0 / count,
                                }
                            },
                        }
                        for path, count, covered in entries
                    ]
                }
            ]
        }
        path = root / "raw.json"
        path.write_text(json.dumps(document), encoding="utf-8")
        return path

    def summarize_args(
        self, root: pathlib.Path, scope: pathlib.Path, report: pathlib.Path
    ) -> argparse.Namespace:
        return argparse.Namespace(
            repo_root=str(root),
            scope=str(scope),
            layer="rust",
            threshold=90.0,
            execution_status=0,
            input=str(report),
            output_dir=str(root / "output"),
        )

    def test_summarize_fails_below_fixed_threshold(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "crate/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("fn covered() {}\n", encoding="utf-8")
            scope = self.write_scope(root)
            report = self.write_llvm_json(root, [(source, 10, 8)])

            status = quiet_call(
                coverage_tools.summarize_layer,
                self.summarize_args(root, scope, report),
            )

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(summary["line_percent"], 80.0)
            self.assertFalse(summary["passed"])

    def test_missing_required_source_mapping_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            mapped = root / "crate/src/lib.rs"
            missing = root / "crate/src/ffi.rs"
            mapped.parent.mkdir(parents=True)
            mapped.write_text("fn mapped() {}\n", encoding="utf-8")
            missing.write_text("fn missing() {}\n", encoding="utf-8")
            scope = self.write_scope(root)
            report = self.write_llvm_json(root, [(mapped, 10, 10)])

            status = quiet_call(
                coverage_tools.summarize_layer,
                self.summarize_args(root, scope, report),
            )

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(summary["missing_required_files"], ["crate/src/ffi.rs"])

    def test_threshold_cannot_be_lowered(self) -> None:
        with self.assertRaises(coverage_tools.CoverageError):
            coverage_tools.validate_threshold(89.99)
        with self.assertRaises(coverage_tools.CoverageError):
            coverage_tools.validate_threshold(float("nan"))

    def test_execution_failure_forces_gate_red_at_full_coverage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "crate/src/lib.rs"
            source.parent.mkdir(parents=True)
            source.write_text("fn covered() {}\n", encoding="utf-8")
            scope = self.write_scope(root)
            report = self.write_llvm_json(root, [(source, 10, 10)])
            args = self.summarize_args(root, scope, report)
            args.execution_status = 7

            status = quiet_call(coverage_tools.summarize_layer, args)

            self.assertEqual(status, 1)
            summary = json.loads((root / "output/layer-summary.json").read_text())
            self.assertEqual(summary["line_percent"], 100.0)
            self.assertEqual(summary["execution_status"], 7)
            self.assertFalse(summary["passed"])

    def test_sql_inventory_binds_markers_to_successful_logs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests = root / "tests"
            results = root / "results"
            logs = results / "logs"
            tests.mkdir()
            logs.mkdir(parents=True)
            (tests / "00_probe.sql").write_text(
                "SELECT 1;\n\\echo 'PASS: probe executed'\n", encoding="utf-8"
            )
            (logs / "00_probe.sql.log").write_text(
                "1\nPASS: probe executed\n", encoding="utf-8"
            )
            (results / "results.tsv").write_text(
                "file\tstatus\texit_code\tlog\n"
                "00_probe.sql\tpass\t0\tlogs/00_probe.sql.log\n",
                encoding="utf-8",
            )
            output = root / "inventory.json"
            args = argparse.Namespace(
                tests_dir=str(tests),
                results=str(results / "results.tsv"),
                output=str(output),
            )

            status = quiet_call(coverage_tools.sql_inventory, args)

            self.assertEqual(status, 0)
            inventory = json.loads(output.read_text())
            self.assertTrue(inventory["complete"])
            self.assertEqual(inventory["declared_behavior_probes"], 1)
            self.assertEqual(inventory["covered_behavior_probes"], 1)

    def test_sql_inventory_does_not_credit_marker_from_failed_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests = root / "tests"
            results = root / "results"
            logs = results / "logs"
            tests.mkdir()
            logs.mkdir(parents=True)
            (tests / "00_probe.sql").write_text(
                "\\echo 'PASS: early marker'\nSELECT 1 / 0;\n", encoding="utf-8"
            )
            (logs / "00_probe.sql.log").write_text(
                "PASS: early marker\nERROR: division by zero\n", encoding="utf-8"
            )
            (results / "results.tsv").write_text(
                "file\tstatus\texit_code\tlog\n"
                "00_probe.sql\tfail\t3\tlogs/00_probe.sql.log\n",
                encoding="utf-8",
            )
            output = root / "inventory.json"
            args = argparse.Namespace(
                tests_dir=str(tests),
                results=str(results / "results.tsv"),
                output=str(output),
            )

            status = quiet_call(coverage_tools.sql_inventory, args)

            self.assertEqual(status, 1)
            inventory = json.loads(output.read_text())
            self.assertFalse(inventory["complete"])
            self.assertEqual(inventory["covered_behavior_probes"], 0)

    def test_sql_inventory_rejects_log_path_substitution(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            tests = root / "tests"
            results = root / "results"
            tests.mkdir()
            results.mkdir()
            (tests / "00_probe.sql").write_text(
                "\\echo 'PASS: probe executed'\n", encoding="utf-8"
            )
            substituted_log = root / "substituted.log"
            substituted_log.write_text("PASS: probe executed\n", encoding="utf-8")
            (results / "results.tsv").write_text(
                "file\tstatus\texit_code\tlog\n"
                "00_probe.sql\tpass\t0\t../substituted.log\n",
                encoding="utf-8",
            )
            output = root / "inventory.json"

            status = quiet_call(
                coverage_tools.sql_inventory,
                argparse.Namespace(
                    tests_dir=str(tests),
                    results=str(results / "results.tsv"),
                    output=str(output),
                ),
            )

            self.assertEqual(status, 1)
            inventory = json.loads(output.read_text())
            self.assertFalse(inventory["complete"])
            self.assertEqual(inventory["covered_behavior_probes"], 0)

    def test_aggregate_fails_when_any_layer_summary_is_missing(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            artifacts = pathlib.Path(directory)
            for layer in ("rust", "cpp"):
                path = artifacts / layer / "layer-summary.json"
                path.parent.mkdir(parents=True)
                path.write_text(
                    json.dumps(
                        {
                            "layer": layer,
                            "covered_lines": 90,
                            "line_count": 100,
                            "line_percent": 90.0,
                            "threshold_percent": 90.0,
                            "passed": True,
                        }
                    ),
                    encoding="utf-8",
                )

            status = quiet_call(
                coverage_tools.aggregate,
                argparse.Namespace(artifact_dir=str(artifacts)),
            )

            self.assertEqual(status, 1)
            summary = json.loads((artifacts / "gate-summary.json").read_text())
            self.assertFalse(summary["passed"])
            self.assertTrue(
                any("missing sql layer" in error for error in summary["errors"])
            )


if __name__ == "__main__":
    unittest.main()
