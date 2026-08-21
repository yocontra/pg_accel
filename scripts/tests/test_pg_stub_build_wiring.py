import pathlib
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]


class PgStubBuildWiringTests(unittest.TestCase):
    def test_build_script_always_defines_compile_time_include(self) -> None:
        source = (REPO_ROOT / "pg_accel" / "build.rs").read_text(encoding="utf-8")
        env_marker = 'println!("cargo::rustc-env=PG_STUBS_GENERATED={}"'
        discovery_marker = "let Some(bindir) = Command::new(&pg_config_path)"

        self.assertIn("cargo::rerun-if-env-changed=PG_CONFIG", source)
        self.assertIn("cargo::rerun-if-env-changed=PGRX_PG_CONFIG_PATH", source)
        self.assertIn("write fallback pg_stubs_generated.rs", source)
        self.assertLess(source.index(env_marker), source.index(discovery_marker))
        selection = source[
            source.index("let pg_config_path"):source.index(discovery_marker)
        ]
        self.assertLess(
            selection.index('std::env::var("PG_CONFIG")'),
            selection.index('std::env::var("PGRX_PG_CONFIG_PATH")'),
        )

    def test_stubs_cover_macos_and_linux_symbol_rules(self) -> None:
        build_source = (REPO_ROOT / "pg_accel" / "build.rs").read_text(
            encoding="utf-8"
        )
        lib_source = (REPO_ROOT / "pg_accel" / "src" / "lib.rs").read_text(
            encoding="utf-8"
        )

        platform_cfg = 'any(target_os = "macos", target_os = "linux")'
        self.assertGreaterEqual(build_source.count(platform_cfg), 2)
        self.assertIn(platform_cfg, lib_source)
        self.assertIn('if cfg!(target_os = "macos")', build_source)
        self.assertIn('if cfg!(target_os = "linux")', build_source)
        self.assertIn('nm_command.arg("-D")', build_source)
        self.assertIn('"B" | "C" | "D" | "G" | "R" | "S" | "V"', build_source)
        self.assertIn('"_start"', build_source)

    def test_linux_ci_executes_standalone_tests_with_selected_pg_config(self) -> None:
        justfile = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")
        workflow = (REPO_ROOT / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn('test-standalone pg="":', justfile)
        self.assertIn(
            'PG_CONFIG="$pg_config" PGRX_PG_CONFIG_PATH="$pg_config"', justfile
        )
        self.assertIn("cargo test -p pg_accel --locked --lib", justfile)
        self.assertIn("check-matrix test-standalone deny", justfile)
        self.assertIn("just test-standalone ${{ matrix.pg }}", workflow)
        self.assertNotIn("cargo test --workspace --no-default-features", workflow)

    def test_compile_recipes_pass_the_selected_pgrx_pg_config(self) -> None:
        justfile = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")

        self.assertGreaterEqual(
            justfile.count('pg_config="$(pg_accel_pg_config_for_pg "$pg")"'), 4
        )
        exact_env = 'PG_CONFIG="$pg_config" PGRX_PG_CONFIG_PATH="$pg_config"'
        self.assertGreaterEqual(justfile.count(exact_env), 7)
        self.assertIn("cargo clippy --workspace", justfile)
        self.assertGreaterEqual(
            justfile.count("cargo check --workspace"), 2
        )
        self.assertIn('RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}"', justfile)

    def test_auxiliary_test_and_hook_paths_keep_exact_postgres_selection(self) -> None:
        justfile = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")
        hooks = (REPO_ROOT / ".pre-commit-config.yaml").read_text(encoding="utf-8")

        self.assertIn('just test-standalone "$pg"', justfile)
        self.assertIn('--features "pg$pg" historical_crash', justfile)
        self.assertNotIn("entry: cargo check --workspace", hooks)
        self.assertNotIn("entry: cargo clippy --workspace", hooks)
        self.assertIn("entry: just check 18", hooks)
        self.assertIn("entry: just lint 18", hooks)

    def test_no_gpu_runner_skips_only_an_explicit_no_device_prerequisite(self) -> None:
        source = (
            REPO_ROOT / "pg_accel" / "src" / "engine" / "residency" / "store.rs"
        ).read_text(encoding="utf-8")
        device_manager = (
            REPO_ROOT / "pgaccel-kernels" / "src" / "device_manager.cpp"
        ).read_text(encoding="utf-8")

        self.assertIn("fn resident_device_test_available() -> bool", source)
        self.assertIn("PgaccelStatus::ErrorNoDevice => false", source)
        self.assertIn(
            'other => panic!("resident device prerequisite probe failed with {other:?}")',
            source,
        )
        self.assertEqual(source.count("if !resident_device_test_available()"), 2)
        self.assertGreaterEqual(
            device_manager.count("init_status = PGACCEL_ERROR_NO_DEVICE"), 2
        )
        self.assertIn("return init_status;", device_manager)


if __name__ == "__main__":
    unittest.main()
