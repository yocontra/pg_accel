import json
import os
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
HARNESS = REPO_ROOT / "scripts" / "coverage_live_rust.sh"
COVERAGE_GATE = REPO_ROOT / "scripts" / "coverage_gate.sh"
TEST_CONNECTION = "local"
TEST_ARTIFACT_ROOT = "/tmp/pgaccel-coverage-live-test"
PHASE9_CONTRACTS = {
    "window_full_output_decline": "no_gpu_resident_pipeline",
    "window_row_number": "no_gpu_resident_pipeline",
    "window_rank": "no_gpu_resident_pipeline",
    "window_dense_rank": "no_gpu_resident_pipeline",
    "window_running_sum": "no_gpu_resident_pipeline",
    "window_analytics": "no_gpu_resident_pipeline",
    "window_reducing_decline": "no_gpu_resident_pipeline",
    "semi_join_null_decline": "no_gpu_resident_pipeline",
    "in_join_null_decline": "no_gpu_resident_pipeline",
    "anti_join_null_decline": "no_gpu_resident_pipeline",
    "not_in_join_null_decline": "shape_sublink",
    "aggregate_semantic_modifier_decline": "shape_aggregate_modifier",
    "aggregate_ordered_set_decline": "shape_aggregate_modifier",
    "numeric_agg_decline": "shape_numeric_accumulator_unavailable",
    "avg_nonfloat_decline": "shape_numeric_accumulator_unavailable",
    "setop_intersect_decline": "setop_no_gpu_kernel",
    "recursive_union_decline": "recursiveunion_no_gpu_kernel",
    "mergejoin_decline": "mergejoin_no_gpu_kernel",
    "gpu_sort_multikey": "sort_multikey_no_gpu_kernel",
    "gpu_nlj_between": "shape_non_equality_join",
}


def call_library(command: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["PGACCEL_COVERAGE_LIVE_LIBRARY_ONLY"] = "1"
    env["PGACCEL_COVERAGE_EXPECTED_CONNECTION"] = TEST_CONNECTION
    env["PGACCEL_COVERAGE_EXPECTED_ARTIFACT_ROOT"] = TEST_ARTIFACT_ROOT
    return subprocess.run(
        [
            "bash",
            "-c",
            f'source "$1"; shift; {command}',
            "coverage-live-test",
            str(HARNESS),
        ],
        cwd=REPO_ROOT,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def live_evidence_validator_source() -> str:
    source = HARNESS.read_text(encoding="utf-8")
    marker = source.index("# Independently consume the durable outputs")
    start = source.index("import json\n", marker)
    end = source.index("\nPY\n", start)
    return source[start:end]


def write_json(path: pathlib.Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value), encoding="utf-8")


def correctness_artifact(name: str, rows: int) -> dict[str, object]:
    return {
        "schema_version": 1,
        "workload": name,
        "rows": rows,
        "status": "pass",
        "order_sensitive": False,
        "accel_rows": 1,
        "baseline_rows": 1,
        "accel_minus_baseline_count": 0,
        "baseline_minus_accel_count": 0,
        "sample_limit": 20,
        "accel_minus_baseline_samples": [],
        "baseline_minus_accel_samples": [],
        "accel_query_sql": "SELECT 1",
        "baseline_query_sql": "SELECT 1",
        "error": None,
    }


def build_live_evidence_fixture(root: pathlib.Path) -> tuple[list[str], dict[str, pathlib.Path]]:
    expected_sha = "a" * 64
    paths: dict[str, pathlib.Path] = {}

    def result_row(
        name: str,
        rows: int,
        *,
        selected: bool = False,
        reason: str = "test_planner_decline",
        iterations: int = 1,
        native_pairing: bool = False,
        artifact_reuse: bool = False,
    ) -> dict[str, object]:
        row: dict[str, object] = {
            "name": name,
            "rows": rows,
            "iterations": [{} for _ in range(iterations)],
            "plan_snippet": "Custom Scan" if selected else "Seq Scan",
            "correctness_diff_artifact": f"correctness/{name}.json",
            "planner_declined": not selected,
            "plan_selected": selected,
            "gpu_kernel_dispatched": selected,
            "function_srf_kernel_dispatched": False,
            "dispatch_counter_captured": True,
            "gpu_kernel_execution_delta": 1 if selected else 0,
            "pg_accel_stock_exec_delta": 0,
            "accel_output_rows_consumed": rows,
            "native_decline_evidence": None
            if selected
            else {"reason": reason, "source": "planner_reported"},
        }
        if native_pairing:
            row["native_parity_pair_captures"] = [
                {
                    "sequence": [
                        "accel",
                        "disabled_postgresql",
                        "disabled_postgresql",
                        "accel",
                    ],
                    "accel_ms": [1.0, 1.0],
                    "parallel_ms": [1.0, 1.0],
                }
            ]
            row["planner_stage_captures"] = [
                {"error": None, "stages": [{}], "substages": [{}]}
            ]
        if artifact_reuse:
            dependency = {
                "relid": 42,
                "generation": 1,
                "global_generation": 1,
                "relfilenode": 7,
                "row_count": rows,
                "raw_bytes": rows * 8,
                "derived_bytes": rows * 4,
            }
            row.update(
                gpu_kernel_dispatched=False,
                custom_scan_selected_not_dispatched=True,
                gpu_kernel_execution_delta=0,
                artifact_lifecycle_probe={
                    "phase": "lifecycle",
                    "error": None,
                    "artifact": {
                        "hits": 0,
                        "builds": 1,
                        "rebuilds": 0,
                        "artifact_bytes_observed": rows * 4,
                    },
                    "dependencies_before": [{**dependency, "derived_bytes": 0}],
                    "dependencies_after": [dependency],
                    "gpu_kernel_executions": 1,
                    "output_rows_consumed": rows,
                    "stock_exec_count": 0,
                },
                artifact_steady_state_captures=[
                    {
                        "phase": "steady_state",
                        "pair_index": index,
                        "error": None,
                        "artifact": {
                            "hits": 1,
                            "builds": 0,
                            "rebuilds": 0,
                            "artifact_bytes_observed": rows * 4,
                        },
                        "refresh_us": 0,
                        "refreshed_relations": 0,
                        "refreshed_rows": 0,
                        "dependencies_before": [dependency],
                        "dependencies_after": [dependency],
                        "queries_accelerated": 1,
                        "gpu_kernel_executions": 0,
                        "output_rows_consumed": rows,
                        "stock_exec_count": 0,
                    }
                    for index in range(iterations)
                ],
            )
        return row

    def report(
        key: str,
        name: str,
        rows: int,
        *,
        selected: bool = False,
        reason: str = "test_planner_decline",
        iterations: int = 1,
        warmup: int = 0,
        native_pairing: bool = False,
        artifact_reuse: bool = False,
    ) -> pathlib.Path:
        path = root / key / "report.json"
        row = result_row(
            name,
            rows,
            selected=selected,
            reason=reason,
            iterations=iterations,
            native_pairing=native_pairing,
            artifact_reuse=artifact_reuse,
        )
        write_json(
            path.parent / str(row["correctness_diff_artifact"]),
            correctness_artifact(name, rows),
        )
        (path.parent / "plans.txt").write_text("captured plan\n", encoding="utf-8")
        write_json(
            path,
            {
                "crashes": [],
                "methodology": {
                    "cache_mode": "warm",
                    "timing_mode": "raw-wallclock",
                    "iterations": iterations,
                    "warmup": warmup,
                    "native_parity_pairing": native_pairing,
                    "native_parity_repetitions_per_arm": 2 if native_pairing else 1,
                },
                "workloads": [row],
            },
        )
        paths[key] = path
        return path

    selected = report("selected", "grouped_agg_int4", 1_000_000, selected=True)
    declined = report(
        "declined",
        "window_full_output_decline",
        10_000,
        native_pairing=True,
    )
    fp64 = report("fp64", "reduce_f64_minmax", 100_000)
    mixed = report("mixed", "mixed_join_agg_int4", 100_000, selected=True)
    ssbm = report("ssbm", "ssbm_resident_int4_star", 100_000, selected=True)
    hash_join = report("hash", "hash_join", 100_000, selected=True)
    h3 = report("h3", "h3_cell_to_parent", 100_000, selected=True)
    spatial_resident = report(
        "spatial_resident",
        "spatial_resident_agg_candidate",
        1_000_000,
        selected=True,
    )
    raster_resident = report(
        "raster_resident",
        "raster_resident_exact_reclass",
        10_000,
        selected=True,
        artifact_reuse=True,
    )
    spatial = report(
        "spatial",
        "spatial_mega_1kv",
        80_000,
        reason="generic_descriptor_capability",
    )
    raster_reclass = report(
        "raster_reclass",
        "raster_reclass",
        100,
        reason="shape_unsupported_rte",
    )

    phase9 = root / "phase9" / "report.json"
    phase9_rows = []
    for name, reason in PHASE9_CONTRACTS.items():
        row = result_row(name, 10_000, reason=reason)
        write_json(
            phase9.parent / str(row["correctness_diff_artifact"]),
            correctness_artifact(name, 10_000),
        )
        phase9_rows.append(row)
    (phase9.parent / "plans.txt").write_text("captured plans\n", encoding="utf-8")
    write_json(
        phase9,
        {
            "crashes": [],
            "methodology": {
                "cache_mode": "warm",
                "timing_mode": "raw-wallclock",
                "iterations": 1,
                "warmup": 0,
            },
            "workloads": phase9_rows,
        },
    )
    paths["phase9"] = phase9

    raster = report(
        "raster",
        "raster_ndvi",
        100,
        reason="shape_unsupported_rte",
        iterations=10,
        warmup=5,
    )
    calibration = root / "calibration" / "fp64_calibration_summary.json"
    write_json(calibration, {"sizes": [100_000], "multipliers": [16.0], "candidates": [{}]})
    output = root / "evidence-validation.json"
    paths["validation"] = output
    provenance = root / "selected" / "provenance.json"
    write_json(
        provenance,
        {
            "errors": [],
            "status": "pass",
            "expected_binary": {"sha256": expected_sha},
            "installed_binary": {"sha256": expected_sha},
            "loaded_binaries": [{"sha256": expected_sha}],
        },
    )

    args = [
        selected,
        declined,
        fp64,
        mixed,
        ssbm,
        hash_join,
        h3,
        spatial_resident,
        raster_resident,
        spatial,
        raster_reclass,
        phase9,
        raster,
        calibration,
        output,
        provenance,
    ]
    return [str(path) for path in args] + [expected_sha], paths


def run_live_evidence_validator(args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["python3", "-", *args],
        cwd=REPO_ROOT,
        env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
        input=live_evidence_validator_source(),
        text=True,
        capture_output=True,
        check=False,
    )


class CoverageLiveRustTests(unittest.TestCase):
    @staticmethod
    def fp64_command(
        multiplier: str = "16",
        artifact_name: str = "fp64-calibration",
    ) -> str:
        return (
            "assert_safe_bench_command fp64-calibrate "
            f"--connection {TEST_CONNECTION} --multipliers {multiplier} "
            "--max-size 100k --warmup 0 --seed 42 --capture-plans "
            "--timing raw --cache-mode warm --skip-guc-verify "
            f"--artifacts-dir {TEST_ARTIFACT_ROOT}/{artifact_name}"
        )

    @staticmethod
    def resume_command(source_name: str = "declined") -> str:
        return (
            "assert_safe_bench_command resume "
            f"--artifacts-dir {TEST_ARTIFACT_ROOT}/{source_name} "
            f"--connection {TEST_CONNECTION} "
            f"--output-dir {TEST_ARTIFACT_ROOT}/resume-output "
            "--format json --dry-run"
        )

    def test_shell_syntax(self) -> None:
        result = subprocess.run(
            ["bash", "-n", str(HARNESS)],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_live_shell_path_is_macos_bash_compatible(self) -> None:
        harness = HARNESS.read_text(encoding="utf-8")
        gate = COVERAGE_GATE.read_text(encoding="utf-8")

        self.assertNotRegex(harness, r"\$\{[^}]+,,\}")
        self.assertNotIn("BASHPID", gate)
        self.assertIn('database_name="pgaccel_cov_rust_pg${pg}_$$"', gate)

        uppercase = "A" * 64
        result = call_library(
            f'[ "$(normalize_sha256 {uppercase})" = "{uppercase.lower()}" ]'
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_provenance_environment_does_not_assign_readonly_shell_state(self) -> None:
        harness = HARNESS.read_text(encoding="utf-8")
        self.assertIn(
            'env COVERAGE_LIVE_SCHEMA_VERSION="$COVERAGE_LIVE_SCHEMA_VERSION" \\\n',
            harness,
        )

        result = call_library(
            'env COVERAGE_LIVE_SCHEMA_VERSION="$COVERAGE_LIVE_SCHEMA_VERSION" '
            "/bin/sh -c 'test \"$COVERAGE_LIVE_SCHEMA_VERSION\" = 1'"
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_help_needs_no_database(self) -> None:
        result = subprocess.run(
            ["bash", str(HARNESS), "--help"],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("performance", result.stdout)

    def test_missing_inputs_fail_before_execution(self) -> None:
        result = subprocess.run(
            ["bash", str(HARNESS)],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing required argument", result.stderr)

    def test_warm_raw_command_is_allowed(self) -> None:
        cells = (
            ("grouped_agg_int4", 1_000_000, "selected", ""),
            (
                "window_full_output_decline",
                10_000,
                "declined",
                "--capture-planner-stages --native-parity-pairing",
            ),
            ("reduce_f64_minmax", 100_000, "fp64-native", ""),
            ("mixed_join_agg_int4", 100_000, "mixed-resident", ""),
            ("ssbm_resident_int4_star", 100_000, "ssbm-resident", ""),
            ("hash_join", 100_000, "hash-join", ""),
            ("h3_cell_to_parent", 100_000, "h3-parent", ""),
            (
                "spatial_resident_agg_candidate",
                1_000_000,
                "spatial-resident",
                "",
            ),
            (
                "raster_resident_exact_reclass",
                10_000,
                "raster-resident",
                "",
            ),
            ("spatial_mega_1kv", 80_000, "spatial-mega", ""),
            ("raster_reclass", 100, "raster-reclass", ""),
        )
        for workload, rows, artifact_name, exact_options in cells:
            with self.subTest(workload=workload, rows=rows):
                result = call_library(
                    f"assert_safe_bench_command crash-repro --workload {workload} "
                    f"--rows {rows} --iterations 1 --warmup 0 --seed 42 "
                    f"--connection {TEST_CONNECTION} --format json --capture-plans "
                    f"--cache-mode warm --timing raw --skip-guc-verify {exact_options} "
                    f"--artifacts-dir {TEST_ARTIFACT_ROOT}/{artifact_name}"
                )
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_native_pairing_and_planner_capture_are_bound_to_one_exact_cell(self) -> None:
        exact = (
            "assert_safe_bench_command crash-repro "
            "--workload window_full_output_decline --rows 10000 "
            "--iterations 1 --warmup 0 --seed 42 "
            f"--connection {TEST_CONNECTION} --format json --capture-plans "
            "--cache-mode warm --timing raw --skip-guc-verify "
            "--capture-planner-stages --native-parity-pairing "
            f"--artifacts-dir {TEST_ARTIFACT_ROOT}/declined"
        )
        self.assertEqual(call_library(exact).returncode, 0)
        for command in (
            exact.replace("--capture-planner-stages ", ""),
            exact.replace("--native-parity-pairing ", ""),
            exact.replace(
                "window_full_output_decline --rows 10000",
                "grouped_agg_int4 --rows 1000000",
            ).replace("/declined", "/selected"),
            "assert_safe_bench_command run --dry-run --native-parity-pairing",
            "assert_safe_bench_command validate --capture-planner-stages",
        ):
            with self.subTest(command=command):
                result = call_library(command)
                self.assertNotEqual(result.returncode, 0)

    def test_crash_repro_budget_cannot_expand(self) -> None:
        base = (
            "assert_safe_bench_command crash-repro --workload mixed_join_agg_int4 "
            "--rows 100000 --iterations 1 --warmup 0 --seed 42 "
            f"--connection {TEST_CONNECTION} --format json --capture-plans "
            "--cache-mode warm --timing raw --skip-guc-verify "
            f"--artifacts-dir {TEST_ARTIFACT_ROOT}/mixed-resident"
        )
        commands = (
            base.replace("mixed_join_agg_int4", "gpu_expr_filter"),
            base.replace("--rows 100000", "--rows 100001"),
            base.replace("--iterations 1", "--iterations 2"),
            base.replace("--warmup 0", "--warmup 1"),
            base.replace("--seed 42", "--seed 43"),
            base.replace(f"--connection {TEST_CONNECTION} ", ""),
            base.replace("--format json", "--format csv"),
            base.replace("--capture-plans ", ""),
            base.replace("--cache-mode warm ", ""),
            base.replace("--timing raw ", ""),
            base.replace(f"--artifacts-dir {TEST_ARTIFACT_ROOT}/mixed-resident", ""),
            base.replace("--skip-guc-verify ", ""),
            base + " --category mixed",
            base + " --dry-run",
            base + " --capture-planner-stages",
            base + " --native-parity-pairing",
        )
        for command in commands:
            with self.subTest(command=command):
                result = call_library(command)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("live coverage crash-repro", result.stderr)

    def test_exact_bounded_setup_is_allowed(self) -> None:
        result = call_library(
            "assert_safe_bench_command setup --workload raster_ndvi --rows 100 "
            f"--seed 42 --connection {TEST_CONNECTION}"
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_setup_cannot_expand_beyond_the_pinned_raster_fixture(self) -> None:
        commands = (
            f"assert_safe_bench_command setup --rows 100 --seed 42 --connection {TEST_CONNECTION}",
            f"assert_safe_bench_command setup --workload raster_ndvi --rows 101 --seed 42 --connection {TEST_CONNECTION}",
            f"assert_safe_bench_command setup --workload raster_slope --rows 100 --seed 42 --connection {TEST_CONNECTION}",
            f"assert_safe_bench_command setup --workload raster_ndvi --rows 100 --seed 42 --category gpu_raster --connection {TEST_CONNECTION}",
        )
        for command in commands:
            with self.subTest(command=command):
                result = call_library(command)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("coverage setup", result.stderr)

    def test_exact_bounded_normal_run_is_allowed(self) -> None:
        result = call_library(
            "assert_safe_bench_command run --workload raster_ndvi "
            "--iterations 10 --warmup 5 --cache-mode warm --timing raw "
            f"--seed 42 --connection {TEST_CONNECTION} --format csv --capture-plans "
            f"--skip-guc-verify --artifacts-dir {TEST_ARTIFACT_ROOT}/normal-run-raster"
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_normal_run_budget_cannot_expand(self) -> None:
        base = (
            "assert_safe_bench_command run --workload raster_ndvi "
            "--iterations 10 --warmup 5 --cache-mode warm --timing raw "
            f"--seed 42 --connection {TEST_CONNECTION} --format csv --capture-plans "
            f"--skip-guc-verify --artifacts-dir {TEST_ARTIFACT_ROOT}/normal-run-raster"
        )
        commands = (
            base.replace("raster_ndvi", "grouped_agg_int4"),
            base.replace("--iterations 10", "--iterations 11"),
            base.replace("--warmup 5", "--warmup 6"),
            base.replace("--cache-mode warm ", ""),
            base.replace("--timing raw ", ""),
            base.replace(f"--artifacts-dir {TEST_ARTIFACT_ROOT}/normal-run-raster", ""),
            base.replace("--seed 42 ", ""),
            base.replace("--format csv ", ""),
            base.replace("--capture-plans ", ""),
            base.replace("--skip-guc-verify ", ""),
            base + " --category gpu_raster",
        )
        for command in commands:
            with self.subTest(command=command):
                result = call_library(command)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("live coverage run", result.stderr)

    def test_exact_bounded_fp64_commands_are_allowed(self) -> None:
        for command in (
            self.fp64_command(),
            self.fp64_command("0.5", "fp64-calibration-invalid"),
        ):
            with self.subTest(command=command):
                result = call_library(command)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_fp64_calibration_budget_and_context_cannot_expand(self) -> None:
        base = self.fp64_command()
        commands = (
            base.replace("--multipliers 16", "--multipliers 32"),
            base.replace("--multipliers 16 ", ""),
            base.replace("--max-size 100k", "--max-size 1B"),
            base.replace("--max-size 100k ", ""),
            base.replace("--warmup 0", "--warmup 1"),
            base.replace("--warmup 0 ", ""),
            base.replace("--seed 42", "--seed 43"),
            base.replace("--seed 42 ", ""),
            base.replace("--capture-plans ", ""),
            base.replace("--timing raw", "--timing both"),
            base.replace("--cache-mode warm", "--cache-mode cold"),
            base.replace("--skip-guc-verify ", ""),
            base.replace(f"--connection {TEST_CONNECTION}", "--connection external"),
            base.replace(
                f"--artifacts-dir {TEST_ARTIFACT_ROOT}/fp64-calibration",
                "--artifacts-dir /tmp/fp64-outside",
            ),
            self.fp64_command("0.5", "fp64-calibration"),
            self.fp64_command("16", "fp64-calibration-invalid"),
        )
        for command in commands:
            with self.subTest(command=command):
                result = call_library(command)
                self.assertNotEqual(result.returncode, 0)

    def test_exact_resume_dry_runs_are_allowed(self) -> None:
        for source in ("declined", "resume-missing"):
            with self.subTest(source=source):
                result = call_library(self.resume_command(source))
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_resume_cannot_execute_or_escape_harness_paths(self) -> None:
        base = self.resume_command()
        commands = (
            base.replace(" --dry-run", ""),
            base.replace("/declined ", "/selected "),
            base.replace(
                f"--artifacts-dir {TEST_ARTIFACT_ROOT}/declined",
                "--artifacts-dir /tmp/untrusted-resume",
            ),
            base.replace(
                f"--output-dir {TEST_ARTIFACT_ROOT}/resume-output",
                "--output-dir /tmp/untrusted-output",
            ),
            base.replace(f"--connection {TEST_CONNECTION}", "--connection external"),
            base.replace("--format json", "--format markdown"),
            base.replace(f"--output-dir {TEST_ARTIFACT_ROOT}/resume-output ", ""),
        )
        for command in commands:
            with self.subTest(command=command):
                result = call_library(command)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("live coverage resume", result.stderr)

    def test_stateful_commands_are_bound_to_the_harness_target_and_paths(self) -> None:
        exact_phase9 = (
            "assert_safe_bench_command phase9-gate "
            f"--connection {TEST_CONNECTION} "
            f"--artifacts-dir {TEST_ARTIFACT_ROOT}/phase9"
        )
        self.assertEqual(call_library(exact_phase9).returncode, 0)
        exact_provenance = (
            "assert_safe_bench_command provenance "
            f"--connection {TEST_CONNECTION}"
        )
        self.assertEqual(call_library(exact_provenance).returncode, 0)

        exact_setup = (
            "assert_safe_bench_command setup --workload raster_ndvi --rows 100 "
            f"--seed 42 --connection {TEST_CONNECTION}"
        )
        exact_run = (
            "assert_safe_bench_command run --workload raster_ndvi "
            "--iterations 10 --warmup 5 --cache-mode warm --timing raw --seed 42 "
            f"--connection {TEST_CONNECTION} --format csv --capture-plans "
            f"--skip-guc-verify --artifacts-dir {TEST_ARTIFACT_ROOT}/normal-run-raster"
        )
        exact_crash = (
            "assert_safe_bench_command crash-repro --workload grouped_agg_int4 "
            "--rows 1000000 --iterations 1 --warmup 0 --seed 42 "
            f"--connection {TEST_CONNECTION} --format json --capture-plans "
            f"--timing raw --cache-mode warm --skip-guc-verify "
            f"--artifacts-dir {TEST_ARTIFACT_ROOT}/selected"
        )
        commands = (
            exact_setup.replace(f"--connection {TEST_CONNECTION}", "--connection external"),
            exact_run.replace(f"--connection {TEST_CONNECTION}", "--connection external"),
            exact_run.replace(
                f"--artifacts-dir {TEST_ARTIFACT_ROOT}/normal-run-raster",
                "--artifacts-dir /tmp/outside-run",
            ),
            exact_crash.replace(f"--connection {TEST_CONNECTION}", "--connection external"),
            exact_crash.replace(
                f"--artifacts-dir {TEST_ARTIFACT_ROOT}/selected",
                "--artifacts-dir /tmp/outside-crash",
            ),
            exact_phase9.replace(f"--connection {TEST_CONNECTION}", "--connection external"),
            exact_phase9.replace(
                f"--artifacts-dir {TEST_ARTIFACT_ROOT}/phase9",
                "--artifacts-dir /tmp/outside-phase9",
            ),
            exact_provenance.replace(
                f"--connection {TEST_CONNECTION}", "--connection external"
            ),
        )
        for command in commands:
            with self.subTest(command=command):
                result = call_library(command)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("harness", result.stderr)

    def test_cold_and_both_cache_modes_are_rejected(self) -> None:
        for mode in ("cold", "both"):
            with self.subTest(mode=mode):
                result = call_library(
                    f"assert_safe_bench_command crash-repro --cache-mode {mode} --timing raw"
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("cache mode must be warm", result.stderr)

    def test_non_raw_timing_modes_are_rejected(self) -> None:
        for mode in ("explain", "both"):
            with self.subTest(mode=mode):
                result = call_library(
                    f"assert_safe_bench_command crash-repro --cache-mode warm --timing {mode}"
                )
                self.assertNotEqual(result.returncode, 0)

    def test_privileged_and_cache_commands_cannot_enter(self) -> None:
        for token in ("sudo", "osascript", "purge", "clear-jit", "gpu-test-cold"):
            with self.subTest(token=token):
                result = call_library(
                    f"assert_safe_bench_command run --dry-run {token}"
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("forbidden", result.stderr)

    def test_realistic_gucs_are_rejected_for_every_allowlisted_command(self) -> None:
        commands = (
            "--help",
            "setup",
            "run",
            "validate",
            "provenance",
            "crash-repro",
            "phase9-gate",
            "fp64-calibrate",
            "report",
            "resume",
        )
        for command in commands:
            for option in ("--realistic-gucs", "--realistic-gucs=true"):
                with self.subTest(command=command, option=option):
                    result = call_library(
                        f"assert_safe_bench_command {command} {option}"
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("forbidden", result.stderr)

    def test_unbounded_or_cold_internal_gates_are_not_allowlisted(self) -> None:
        for command in ("metal-ship-gate", "phase6-gate", "explain-audit"):
            with self.subTest(command=command):
                result = call_library(f"assert_safe_bench_command {command}")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("outside the coverage allowlist", result.stderr)

    def test_arbitrary_command_is_rejected(self) -> None:
        result = call_library("assert_safe_bench_command sh -c true")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outside the coverage allowlist", result.stderr)

    def test_unknown_options_and_positional_tokens_are_rejected(self) -> None:
        for suffix in ("--future-unbounded-sweep", "unexpected-positional"):
            with self.subTest(suffix=suffix):
                result = call_library(
                    f"assert_safe_bench_command run --dry-run {suffix}"
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("unrecognized benchmark option or positional token", result.stderr)

    def test_missing_profile_evidence_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = call_library(
                f'assert_profile_for_label missing "{directory}"'
            )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("produced no nonempty raw profile", result.stderr)

    def test_empty_required_evidence_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            empty = pathlib.Path(directory) / "empty.json"
            empty.touch()
            result = call_library(f'require_nonempty_file "{empty}"')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing or empty", result.stderr)

    def test_harness_declares_instrumented_timings_ineligible(self) -> None:
        source = HARNESS.read_text(encoding="utf-8")
        self.assertIn('"performance_evidence_eligible": False', source)
        self.assertIn('"cache_policy": "warm-only"', source)
        self.assertIn("all_outputs_consumed", source)
        self.assertIn("source-hashes.tsv", source)
        self.assertIn("profile-manifest.tsv", source)
        self.assertIn("PG_ACCEL_EXPECTED_DYLIB", source)
        self.assertIn("loaded_extension_hash_bound", source)

    def test_live_evidence_validator_accepts_exact_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            args, paths = build_live_evidence_fixture(pathlib.Path(directory))
            result = run_live_evidence_validator(args)
            validation = json.loads(paths["validation"].read_text(encoding="utf-8"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            validation["resident_artifact_reuse_cells"],
            ["raster_resident_exact_reclass@10000"],
        )

    def test_raster_artifact_reuse_probe_requires_expected_gate_failure(self) -> None:
        source = HARNESS.read_text(encoding="utf-8")
        self.assertIn("run_bench raster_resident_crash_repro 1 crash-repro", source)

    def test_live_evidence_validator_rejects_false_error_and_wrong_identity_correctness(self) -> None:
        cases = (
            ("nonempty_diff", "selected", {"accel_minus_baseline_count": 1}),
            ("failed_status", "mixed", {"status": "fail"}),
            ("error_status", "raster", {"status": "error", "error": "oracle failed"}),
            ("wrong_workload", "declined", {"workload": "wrong_workload"}),
            ("wrong_scale", "fp64", {"rows": 999}),
            ("unequal_rows", "ssbm", {"baseline_rows": 2}),
        )
        for label, key, mutation in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                args, paths = build_live_evidence_fixture(pathlib.Path(directory))
                report = json.loads(paths[key].read_text(encoding="utf-8"))
                relative = report["workloads"][0]["correctness_diff_artifact"]
                artifact_path = paths[key].parent / relative
                artifact = json.loads(artifact_path.read_text(encoding="utf-8"))
                artifact.update(mutation)
                write_json(artifact_path, artifact)
                result = run_live_evidence_validator(args)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("correctness diff", result.stderr)

        with tempfile.TemporaryDirectory() as directory:
            args, paths = build_live_evidence_fixture(pathlib.Path(directory))
            report = json.loads(paths["phase9"].read_text(encoding="utf-8"))
            relative = report["workloads"][-1]["correctness_diff_artifact"]
            artifact_path = paths["phase9"].parent / relative
            artifact = json.loads(artifact_path.read_text(encoding="utf-8"))
            artifact.update(status="error", error="phase9 oracle failed")
            write_json(artifact_path, artifact)
            result = run_live_evidence_validator(args)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("correctness diff", result.stderr)

    def test_live_evidence_validator_rejects_selected_stock_fallback(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            args, paths = build_live_evidence_fixture(pathlib.Path(directory))
            report = json.loads(paths["selected"].read_text(encoding="utf-8"))
            report["workloads"][0]["pg_accel_stock_exec_delta"] = 1
            write_json(paths["selected"], report)
            result = run_live_evidence_validator(args)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("stock executor fallback was observed", result.stderr)

    def test_live_evidence_validator_rejects_false_raster_artifact_reuse(self) -> None:
        mutations = {
            "fresh_dispatch": lambda row: row.update(
                gpu_kernel_dispatched=True,
                custom_scan_selected_not_dispatched=False,
                gpu_kernel_execution_delta=1,
            ),
            "lifecycle_dispatch": lambda row: row["artifact_lifecycle_probe"].update(
                gpu_kernel_executions=0
            ),
            "steady_rebuild": lambda row: row["artifact_steady_state_captures"][0][
                "artifact"
            ].update(builds=1),
            "steady_dispatch": lambda row: row["artifact_steady_state_captures"][0].update(
                gpu_kernel_executions=1
            ),
            "unstable_dependency": lambda row: row["artifact_steady_state_captures"][0].update(
                dependencies_after=[]
            ),
        }
        for label, mutate in mutations.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                args, paths = build_live_evidence_fixture(pathlib.Path(directory))
                report = json.loads(paths["raster_resident"].read_text(encoding="utf-8"))
                mutate(report["workloads"][0])
                write_json(paths["raster_resident"], report)
                result = run_live_evidence_validator(args)
                self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_live_evidence_validator_rejects_unconfirmed_decline_sources(self) -> None:
        for key in ("declined", "fp64", "spatial", "raster_reclass", "raster"):
            with self.subTest(key=key), tempfile.TemporaryDirectory() as directory:
                args, paths = build_live_evidence_fixture(pathlib.Path(directory))
                report = json.loads(paths[key].read_text(encoding="utf-8"))
                report["workloads"][0]["native_decline_evidence"]["source"] = (
                    "expected_unconfirmed"
                )
                write_json(paths[key], report)
                result = run_live_evidence_validator(args)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("source is not planner_reported", result.stderr)

    def test_live_evidence_validator_rejects_fabricated_phase9_evidence(self) -> None:
        mutations = {
            "identity": lambda report, row: row.update(
                name=report["workloads"][0]["name"]
            ),
            "source": lambda _report, row: row["native_decline_evidence"].update(
                source="expected_unconfirmed"
            ),
            "reason": lambda _report, row: row["native_decline_evidence"].update(
                reason="fabricated_reason"
            ),
            "selection": lambda _report, row: row.update(plan_selected=True),
            "counter_capture": lambda _report, row: row.update(
                dispatch_counter_captured=False
            ),
            "kernel_delta": lambda _report, row: row.update(gpu_kernel_execution_delta=1),
            "function_dispatch": lambda _report, row: row.update(
                function_srf_kernel_dispatched=True
            ),
            "stock_fallback": lambda _report, row: row.update(pg_accel_stock_exec_delta=1),
            "measured_output": lambda _report, row: row.update(iterations=[]),
            "plan": lambda _report, row: row.update(plan_snippet=None),
            "correctness": lambda _report, row: row.update(
                correctness_diff_artifact="correctness/missing.json"
            ),
        }
        for label, mutate in mutations.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                args, paths = build_live_evidence_fixture(pathlib.Path(directory))
                report = json.loads(paths["phase9"].read_text(encoding="utf-8"))
                mutate(report, report["workloads"][-1])
                write_json(paths["phase9"], report)
                result = run_live_evidence_validator(args)
                self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_live_plan_covers_required_cli_paths(self) -> None:
        source = HARNESS.read_text(encoding="utf-8")
        for runner, label in (
            ("run_bench", "workload_list"),
            ("run_bench", "validate_fp64_registry"),
            ("run_bench", "validate_all_bounded"),
            ("run_bench", "workload_matrix_dry_run"),
            ("run_bench", "provenance_success"),
            ("run_bench", "provenance_failure"),
            ("run_bench", "bounded_setup"),
            ("run_bench", "selected_crash_repro"),
            ("run_bench", "declined_crash_repro"),
            ("run_bench", "fp64_native_crash_repro"),
            ("run_bench", "mixed_resident_crash_repro"),
            ("run_bench", "ssbm_resident_crash_repro"),
            ("run_bench", "hash_join_crash_repro"),
            ("run_bench", "h3_parent_crash_repro"),
            ("run_bench", "spatial_resident_crash_repro"),
            ("run_bench", "raster_resident_crash_repro"),
            ("run_bench", "spatial_mega_decline"),
            ("run_bench", "raster_reclass_decline"),
            ("run_bench", "phase9_bounded"),
            ("run_bench", "normal_run_raster"),
            ("run_bench_input", "report_from_artifact"),
            ("run_bench_input", "report_json_from_artifact"),
            ("run_bench_input", "report_csv_from_artifact"),
            ("run_bench_input", "report_failure"),
            ("run_bench", "resume_empty"),
            ("run_bench", "resume_missing_evidence"),
            ("run_bench", "fp64_calibration"),
            ("run_bench", "fp64_invalid_multiplier"),
        ):
            with self.subTest(label=label):
                self.assertIn(f"{runner} {label}", source)

    def test_gate_runs_live_cli_after_pgrx_and_before_profile_collection(self) -> None:
        source = COVERAGE_GATE.read_text(encoding="utf-8")
        invocation = "run_live_rust_coverage_harness \\\n"
        pgrx_test = source.index("cargo pgrx test --package pg_accel")
        live_run = source.index(invocation, pgrx_test)
        profile_copy = source.index(
            'copy_profiles "$build_dir" "$profile_dir"', live_run
        )

        self.assertLess(pgrx_test, live_run)
        self.assertLess(live_run, profile_copy)
        self.assertIn("record_stage rust live_cli 0", source[live_run:profile_copy])
        self.assertIn("record_stage rust live_cli 1", source[live_run:profile_copy])

    def test_gate_binds_live_cli_to_exact_instrumented_candidate(self) -> None:
        source = COVERAGE_GATE.read_text(encoding="utf-8")
        invocation = "run_live_rust_coverage_harness \\\n"
        function_start = source.index("run_live_rust_coverage_harness()")
        function_end = source.index("\nrust_coverage()", function_start)
        function = source[function_start:function_end]

        for required in (
            'bench_bin="$build_dir/debug/pg_accel_bench"',
            'live_profile_dir="$build_dir/live-cli-profiles"',
            '--candidate-sha "$git_commit"',
            '--source-tree "$git_source_tree"',
            '--object-sha256 "$production_object_sha"',
            'database_name="pgaccel_cov_rust_pg${pg}_$$"',
            '"$(sha256_file "$bench_bin")" != "$production_object_sha"',
        ):
            with self.subTest(required=required):
                self.assertIn(required, function)
        for required in (
            'just install-pg-accel "$pg"',
            'cargo pgrx stop --package pg_accel "pg$pg"',
            'PG_ACCEL_EXPECTED_DYLIB="$built_extension"',
            'pgaccel-live-server-%p-%m.profraw',
            'record_stage rust live_extension_install 0',
            'live-extension-objects.tsv',
            'live-server-profile-manifest.tsv',
        ):
            with self.subTest(required=required):
                self.assertIn(required, function)
        for forbidden in ("sudo", "osascript", "purge"):
            with self.subTest(forbidden=forbidden):
                self.assertNotIn(forbidden, function)

        build_start = source.index('cargo build --workspace --locked')
        live_run = source.index(invocation, build_start)
        lifecycle = source[build_start:live_run]
        self.assertIn('production_bench_sha="$(sha256_file', lifecycle)
        self.assertIn('"$output_dir" "$build_dir" "$production_bench_sha"', source[live_run:])

    def test_live_database_stages_are_required_rust_coverage_stages(self) -> None:
        result = subprocess.run(
            [
                "python3",
                "-c",
                (
                    "import scripts.coverage_tools as c; "
                    "required={'live_extension_install','live_cli'}; "
                    "raise SystemExit(0 if required <= c.REQUIRED_STAGES['rust'] else 1)"
                ),
            ],
            cwd=REPO_ROOT,
            env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
