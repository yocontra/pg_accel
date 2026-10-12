from __future__ import annotations

import pathlib
import subprocess
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "upstream_pg_tests.sh"
JUSTFILE = REPO_ROOT / "Justfile"
WORKFLOW = REPO_ROOT / ".github" / "workflows" / "ci.yml"


class UpstreamPostgresGateContractTests(unittest.TestCase):
    def test_shell_is_syntactically_valid(self) -> None:
        subprocess.run(["bash", "-n", str(SCRIPT)], check=True)

    def test_gate_runs_both_suites_in_both_modes_and_seals_evidence(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        for fragment in (
            "for mode in pristine loaded",
            'run_suite "$mode" regression',
            'run_suite "$mode" isolation',
            "shared_preload_libraries = 'pg_accel'",
            "pg_accel.enabled = on",
            "pg_accel.gpu_enabled = on",
            "postgres_tarball_sha256=",
            "pg_accel_source_manifest_sha256=",
            "git-diff.patch",
            "git-status.txt",
            "source-files.sha256",
            "pg_accel_module_sha256=",
            "regression.diffs",
            "SHA256SUMS",
        ):
            self.assertIn(fragment, source)

    def test_gate_refuses_to_overwrite_an_evidence_directory(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        self.assertIn('if [ -e "$artifact_dir" ]', source)
        self.assertIn("artifact directory already exists", source)

    def test_just_and_ci_wire_the_real_gate(self) -> None:
        justfile = JUSTFILE.read_text(encoding="utf-8")
        workflow = WORKFLOW.read_text(encoding="utf-8")
        self.assertIn("upstream-pg-tests pg=", justfile)
        self.assertIn("upstream-pg-tests-audit", justfile)
        self.assertIn("just upstream-pg-tests", workflow)
        self.assertIn("upstream-postgresql-pg${{ matrix.pg }}", workflow)

    def test_catalog_lifecycle_uses_an_owned_database_and_is_in_ci(self) -> None:
        script = REPO_ROOT / "scripts" / "extension_catalog_integration.sh"
        subprocess.run(["bash", "-n", str(script)], check=True)
        source = script.read_text(encoding="utf-8")
        self.assertIn('test_db="pg_accel_catalog_$$_$(date +%s)"', source)
        self.assertIn('if [ "$created" = 1 ]', source)
        self.assertIn('TEMPLATE template0', source)
        self.assertNotIn('WITH (FORCE)', source)
        fixture = (REPO_ROOT / "sql" / "extension_catalog_contract.sql").read_text(encoding="utf-8")
        for marker in ("preload_only", "create_rollback", "installed", "drop_rollback", "drop_recreate"):
            self.assertIn(f"PGACCEL_CATALOG_OK:{marker}", fixture)
        self.assertIn("force_generic_plan", fixture)
        self.assertIn("EXECUTE _catalog_prepared", fixture)
        self.assertIn("parallel_dense_integer", fixture)
        self.assertIn("extension-catalog-test", JUSTFILE.read_text(encoding="utf-8"))
        self.assertIn("just extension-catalog-test", WORKFLOW.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
