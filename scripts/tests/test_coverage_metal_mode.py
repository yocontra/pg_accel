import json
import os
import pathlib
import subprocess
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "scripts" / "coverage_metal_mode.sh"


class CoverageMetalModeTests(unittest.TestCase):
    @staticmethod
    def write_command(directory: pathlib.Path, name: str, body: str) -> None:
        path = directory / name
        path.write_text(f"#!/usr/bin/env bash\nset -euo pipefail\n{body}\n", encoding="utf-8")
        path.chmod(0o755)

    def run_mode(
        self, root: pathlib.Path, mode: str | None, *, paravirtual: bool = True
    ) -> subprocess.CompletedProcess[str]:
        commands = root / "bin"
        commands.mkdir()
        self.write_command(
            commands,
            "uname",
            'case "$1" in -s) echo Darwin ;; -m) echo arm64 ;; *) echo mock ;; esac',
        )
        self.write_command(
            commands,
            "sysctl",
            'case "$2" in machdep.cpu.brand_string) echo "Apple M1 (Virtual)" ;; '
            'hw.logicalcpu) echo 3 ;; hw.memsize) echo 7516192768 ;; *) exit 1 ;; esac',
        )
        gpu = "Apple Paravirtual device" if paravirtual else "Apple M2 Max"
        self.write_command(commands, "system_profiler", f'echo "Chipset Model: {gpu}"')
        env = os.environ.copy()
        env["PATH"] = f"{commands}:{env['PATH']}"
        if mode is None:
            env.pop("PGACCEL_HOSTED_METAL_COMPATIBILITY", None)
        else:
            env["PGACCEL_HOSTED_METAL_COMPATIBILITY"] = mode
        return subprocess.run(
            ["bash", str(SCRIPT), str(root / "mode.json")],
            cwd=REPO_ROOT,
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )

    def test_default_mode_records_full_device_execution(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            result = self.run_mode(root, None)
            payload = json.loads((root / "mode.json").read_text(encoding="utf-8"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(payload["mode"], "full_device")
        self.assertFalse(payload["host_reference_common_extended"])

    def test_exact_virtual_m1_enables_bounded_compatibility_mode(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            result = self.run_mode(root, "1")
            payload = json.loads((root / "mode.json").read_text(encoding="utf-8"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(payload["mode"], "hosted_virtual_m1_compatibility")
        self.assertTrue(payload["gpu_basic_tier"])
        self.assertTrue(payload["host_reference_common_extended"])
        self.assertFalse(payload["performance_evidence_eligible"])

    def test_wrong_gpu_and_invalid_mode_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            result = self.run_mode(pathlib.Path(temporary), "1", paravirtual=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Apple Paravirtual device", result.stderr)
        with tempfile.TemporaryDirectory() as temporary:
            result = self.run_mode(pathlib.Path(temporary), "yes")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must be 0 or 1", result.stderr)

    def test_host_reference_is_cpp_only_and_absent_from_production_target(self) -> None:
        cmake = (REPO_ROOT / "pgaccel-kernels/CMakeLists.txt").read_text(
            encoding="utf-8"
        )
        source = (REPO_ROOT / "pgaccel-kernels/src/expr_eval.cpp").read_text(
            encoding="utf-8"
        )
        header = (REPO_ROOT / "pgaccel-kernels/include/pgaccel_expr.h").read_text(
            encoding="utf-8"
        )
        test_source = (
            REPO_ROOT / "pgaccel-kernels/test/test_expr_vm_matrix.cpp"
        ).read_text(encoding="utf-8")
        self.assertEqual(
            cmake.count(
                "target_compile_definitions(pgaccel_kernels_shared PRIVATE "
                "PGACCEL_TEST_HOOKS)"
            ),
            1,
        )
        self.assertNotIn(
            "target_compile_definitions(pgaccel_kernels PRIVATE PGACCEL_TEST_HOOKS)",
            cmake,
        )
        self.assertIn(
            '}  // extern "C"\n\n#if defined(PGACCEL_TEST_HOOKS)\n', source
        )
        self.assertIn("namespace pgaccel_test {", source)
        self.assertNotIn("expr_eval_predicate_host", header)
        self.assertIn("pgaccel_test::expr_eval_predicate_host", test_source)
        self.assertIn('CHECK("GPU predicate matches host reference"', test_source)
        for required in (
            "exact_hosted_virtual_m1",
            'sysctlbyname("machdep.cpu.brand_string"',
            'std::strcmp(cpu_brand, "Apple M1 (Virtual)")',
            'std::strcmp(device.device_name, "Apple Paravirtual device")',
            "device.compute_units == 1",
        ):
            self.assertIn(required, test_source)


if __name__ == "__main__":
    unittest.main()
