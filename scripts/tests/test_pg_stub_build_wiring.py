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

    def test_compile_recipes_pass_the_selected_pgrx_pg_config(self) -> None:
        justfile = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")

        self.assertGreaterEqual(
            justfile.count('pg_config="$(pg_accel_pg_config_for_pg "$pg")"'), 3
        )
        self.assertIn('PG_CONFIG="$pg_config" cargo clippy --workspace', justfile)
        self.assertGreaterEqual(
            justfile.count('PG_CONFIG="$pg_config" cargo check --workspace'), 2
        )


if __name__ == "__main__":
    unittest.main()
