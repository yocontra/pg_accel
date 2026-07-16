from __future__ import annotations

import importlib.util
import pathlib
import tempfile
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
MODULE_PATH = REPO_ROOT / "scripts" / "package_extension.py"
SPEC = importlib.util.spec_from_file_location("package_extension", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
package_extension = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package_extension)


class PackageExtensionTests(unittest.TestCase):
    def test_discovers_extension_under_arbitrary_pg_prefix(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            extension = root / "custom" / "postgres" / "lib" / "pg_accel.dylib"
            extension.parent.mkdir(parents=True)
            extension.write_bytes(b"extension")
            self.assertEqual(
                package_extension.discover_extension(root, "Darwin"), extension
            )

    def test_extension_discovery_fails_closed_on_ambiguity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for prefix in ("one", "two"):
                extension = root / prefix / "lib" / "pg_accel.so"
                extension.parent.mkdir(parents=True)
                extension.write_bytes(b"extension")
            with self.assertRaisesRegex(
                package_extension.PackageError, "expected exactly one"
            ):
                package_extension.discover_extension(root, "Linux")

    def test_private_and_unexpected_absolute_load_paths_fail_closed(self) -> None:
        for value in (
            "/Users/builder/project/libacpp-rt.dylib",
            "/private/tmp/build/libacpp-common.dylib",
            "/unknown/prefix/libdependency.dylib",
        ):
            with self.subTest(value=value), self.assertRaises(
                package_extension.PackageError
            ):
                package_extension.validate_load_value(value, "dependency", "Darwin")

        package_extension.validate_load_value(
            "/opt/homebrew/opt/llvm@20/lib/libLLVM.dylib", "dependency", "Darwin"
        )
        package_extension.validate_load_value(
            "@loader_path/../lib/libacpp-rt.dylib", "dependency", "Darwin"
        )
        package_extension.validate_load_value(
            "$ORIGIN/../lib", "RUNPATH", "Linux"
        )
        with self.assertRaisesRegex(package_extension.PackageError, "non-relocatable"):
            package_extension.validate_load_value("relative/lib", "RUNPATH", "Linux")
        with self.assertRaisesRegex(package_extension.PackageError, "LC_ID_DYLIB"):
            package_extension.validate_load_value(
                "/opt/homebrew/opt/llvm@20/lib/libLLVM.dylib",
                "LC_ID_DYLIB",
                "Darwin",
            )

    def test_metal_bundle_preserves_prefix_and_excludes_omp_backend(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            prefix = root / "acpp"
            acpp_lib = prefix / "lib"
            hipsycl = acpp_lib / "hipSYCL"
            llvm_backend = hipsycl / "llvm-to-backend"
            bitcode = hipsycl / "bitcode"
            for path in (llvm_backend, bitcode, prefix / "bin"):
                path.mkdir(parents=True)
            for name in ("libacpp-rt.dylib", "libacpp-common.dylib"):
                (acpp_lib / name).write_bytes(b"dylib")
            (hipsycl / "librt-backend-metal.dylib").write_bytes(b"metal")
            (hipsycl / "librt-backend-omp.dylib").write_bytes(b"omp")
            (llvm_backend / "libllvm-to-metal.dylib").write_bytes(b"llvm")
            (bitcode / "libkernel-sscp-metal-core.bc").write_bytes(b"bitcode")
            (prefix / "bin" / "acpp-metal-archive-build").write_bytes(b"helper")

            extension = root / "package" / "lib" / "pg_accel.dylib"
            extension.parent.mkdir(parents=True)
            extension.write_bytes(b"extension")
            runtime = package_extension.bundle_runtime(extension, prefix, "Darwin")
            package_extension._assert_runtime_layout(runtime, "Darwin")

            self.assertTrue(
                (runtime / "bin" / "acpp-metal-archive-build").is_file()
            )
            self.assertFalse(
                (runtime / "lib" / "hipSYCL" / "librt-backend-omp.dylib").exists()
            )

    def test_all_release_package_entry_points_use_central_helper(self) -> None:
        justfile = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")
        package = justfile[
            justfile.index('package pg="":') : justfile.index("package-matrix:")
        ]
        matrix = justfile[
            justfile.index("package-matrix:") : justfile.index(
                "install-pg-accel", justfile.index("package-matrix:")
            )
        ]
        workflow = (REPO_ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        invocation = "python3 scripts/package_extension.py"
        self.assertIn(invocation, package)
        self.assertIn(invocation, matrix)
        self.assertIn(invocation, workflow)

    def test_checksum_manifest_is_sorted_complete_and_fails_when_stale(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "z").write_bytes(b"last")
            (root / "nested").mkdir()
            (root / "nested" / "a").write_bytes(b"first")

            manifest = package_extension.write_checksums(root)
            package_extension.validate_checksums(root)
            paths = [line.split("  ", 1)[1] for line in manifest.read_text().splitlines()]
            self.assertEqual(paths, sorted(paths))
            self.assertNotIn("SHA256SUMS", paths)

            (root / "z").write_bytes(b"changed")
            with self.assertRaisesRegex(package_extension.PackageError, "stale"):
                package_extension.validate_checksums(root)


if __name__ == "__main__":
    unittest.main()
