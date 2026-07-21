import os
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
HARNESS = REPO_ROOT / "scripts" / "coverage_live_rust.sh"
COVERAGE_GATE = REPO_ROOT / "scripts" / "coverage_gate.sh"


def call_library(command: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["PGACCEL_COVERAGE_LIVE_LIBRARY_ONLY"] = "1"
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


class CoverageLiveRustTests(unittest.TestCase):
    def test_shell_syntax(self) -> None:
        result = subprocess.run(
            ["bash", "-n", str(HARNESS)],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
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
        result = call_library(
            "assert_safe_bench_command crash-repro --workload gpu_expr_filter "
            "--cache-mode warm --timing raw"
        )
        self.assertEqual(result.returncode, 0, result.stderr)

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

    def test_unbounded_or_cold_internal_gates_are_not_allowlisted(self) -> None:
        for command in ("metal-ship-gate", "phase6-gate"):
            with self.subTest(command=command):
                result = call_library(f"assert_safe_bench_command {command}")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("outside the coverage allowlist", result.stderr)

    def test_arbitrary_command_is_rejected(self) -> None:
        result = call_library("assert_safe_bench_command sh -c true")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("outside the coverage allowlist", result.stderr)

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

    def test_live_plan_covers_required_cli_paths(self) -> None:
        source = HARNESS.read_text(encoding="utf-8")
        for runner, label in (
            ("run_bench", "workload_list"),
            ("run_bench", "validate_fp64_registry"),
            ("run_bench", "provenance_success"),
            ("run_bench", "provenance_failure"),
            ("run_bench", "selected_crash_repro"),
            ("run_bench", "declined_crash_repro"),
            ("run_bench", "fp64_native_crash_repro"),
            ("run_bench", "phase9_bounded"),
            ("run_bench_input", "report_from_artifact"),
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
            'database_name="pgaccel_cov_rust_pg${pg}_${BASHPID}"',
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
