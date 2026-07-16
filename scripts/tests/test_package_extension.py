from __future__ import annotations

import importlib.util
import os
import pathlib
import stat
import tarfile
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
            (llvm_backend / "libllvm-to-backend.dylib").write_bytes(b"llvm-base")
            (llvm_backend / "libllvm-to-metal.dylib").write_bytes(b"llvm-metal")
            (bitcode / "libkernel-sscp-metal-full.bc").write_bytes(b"bitcode")
            helper = prefix / "bin" / "acpp-metal-archive-build"
            helper.write_bytes(b"helper")
            helper.chmod(0o755)

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

    def test_linux_layout_requires_backend_compiler_and_full_bitcode(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            runtime = pathlib.Path(directory) / "pg_accel-runtime"
            hipsycl = runtime / "lib" / "hipSYCL"
            compiler = hipsycl / "llvm-to-backend"
            bitcode = hipsycl / "bitcode"
            for path in (compiler, bitcode):
                path.mkdir(parents=True)
            for name in ("libacpp-rt.so", "libacpp-common.so"):
                (runtime / "lib" / name).write_bytes(b"elf")
            (hipsycl / "librt-backend-omp.so").write_bytes(b"elf")
            (compiler / "libllvm-to-backend.so").write_bytes(b"elf")
            (bitcode / "libkernel-sscp-host-full.bc").write_bytes(b"bitcode")

            with self.assertRaisesRegex(package_extension.PackageError, "target compiler"):
                package_extension._assert_runtime_layout(runtime, "Linux")
            (compiler / "libllvm-to-host.so").write_bytes(b"elf")
            package_extension._assert_runtime_layout(runtime, "Linux")

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
        self.assertIn("*.tar.gz.sha256", workflow)
        self.assertNotIn(
            "path: target/release/pg_accel-pg${{ matrix.pg }}/", workflow
        )
        self.assertNotIn("release-artifacts/pg_accel-pg*/**", workflow)
        self.assertIn("release-artifacts/pg_accel-pg*/*.tar.gz", workflow)
        helper = MODULE_PATH.read_text(encoding="utf-8")
        self.assertIn("shutil.rmtree(package_root, ignore_errors=True)", helper)

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

    def test_sanitized_provenance_keeps_required_setup_identity(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            prefix = root / "home" / ".pgaccel" / "acpp" / "metal"
            prefix.mkdir(parents=True)
            sha = "a" * 40
            (prefix / "pg_accel-acpp-provenance.txt").write_text(
                "\n".join(
                    (
                        "backend=metal",
                        "targets=generic",
                        f"acpp_required_sha={sha}",
                        f"acpp_head={sha}",
                        "soft_fp64_required_tag=v1.3.0",
                        f"soft_fp64_head={'b' * 40}",
                        f"cmake_args=-DCMAKE_INSTALL_PREFIX={prefix}",
                        "acpp_git_status_start",
                        "acpp_git_status_end",
                    )
                )
                + "\n",
                encoding="utf-8",
            )
            package = root / "package"
            package.mkdir()
            destination = package_extension.copy_sanitized_provenance(
                prefix, package, sha
            )
            text = destination.read_text(encoding="utf-8")
            self.assertIn("${ACPP_PREFIX}", text)
            self.assertNotIn(str(prefix), text)

    def test_archive_preserves_hidden_files_modes_symlinks_and_checksums(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            package = root / "pg_accel-pg18"
            runtime_bin = package / "prefix" / "lib" / "pg_accel-runtime" / "bin"
            runtime_lib = runtime_bin.parent / "lib"
            runtime_bin.mkdir(parents=True)
            runtime_lib.mkdir()
            (package / ".acpp-version").write_text("pin\n", encoding="utf-8")
            helper = runtime_bin / "acpp-metal-archive-build"
            helper.write_bytes(b"helper")
            helper.chmod(0o755)
            payload = runtime_lib / "libacpp-rt.so.1"
            payload.write_bytes(b"runtime")
            (runtime_lib / "libacpp-rt.so").symlink_to(payload.name)
            package_extension.write_checksums(package)
            stale = root / "pg_accel-pg18-old-platform.tar.gz"
            stale.write_bytes(b"stale")

            archive, outer = package_extension.create_release_archive(
                package, "18", system="Darwin", machine="arm64"
            )
            self.assertFalse(stale.exists())
            first_bytes = archive.read_bytes()
            package_extension.create_release_archive(
                package, "18", system="Darwin", machine="arm64"
            )
            self.assertEqual(first_bytes, archive.read_bytes())
            package_extension.validate_release_archive(package, archive, outer)

            extracted = root / "extracted"
            extracted.mkdir()
            with tarfile.open(archive, "r:gz") as tar:
                tar.extractall(extracted, filter="fully_trusted")
            unpacked = extracted / package.name
            self.assertTrue((unpacked / ".acpp-version").is_file())
            unpacked_helper = (
                unpacked / "prefix" / "lib" / "pg_accel-runtime" / "bin"
                / "acpp-metal-archive-build"
            )
            self.assertTrue(stat.S_IMODE(unpacked_helper.stat().st_mode) & stat.S_IXUSR)
            unpacked_link = (
                unpacked / "prefix" / "lib" / "pg_accel-runtime" / "lib"
                / "libacpp-rt.so"
            )
            self.assertTrue(unpacked_link.is_symlink())
            self.assertEqual(os.readlink(unpacked_link), "libacpp-rt.so.1")
            package_extension.validate_checksums(unpacked)


if __name__ == "__main__":
    unittest.main()
