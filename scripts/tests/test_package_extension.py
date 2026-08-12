from __future__ import annotations

import importlib.util
import os
import pathlib
import platform
import re
import shutil
import stat
import tarfile
import tempfile
import unittest
from unittest import mock


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
MODULE_PATH = REPO_ROOT / "scripts" / "package_extension.py"
SPEC = importlib.util.spec_from_file_location("package_extension", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
package_extension = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package_extension)
INSTALLER_PATH = REPO_ROOT / "scripts" / "install_package.py"
INSTALLER_SPEC = importlib.util.spec_from_file_location(
    "install_package", INSTALLER_PATH
)
assert INSTALLER_SPEC is not None and INSTALLER_SPEC.loader is not None
install_package = importlib.util.module_from_spec(INSTALLER_SPEC)
INSTALLER_SPEC.loader.exec_module(install_package)


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

    def test_normalizes_private_pgrx_prefix_to_stable_topology(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory) / "pg_accel-pg18"
            prefix = root / "Users" / "builder" / "work" / "postgres"
            library = prefix / "lib" / "pg_accel.dylib"
            share = prefix / "share" / "extension"
            library.parent.mkdir(parents=True)
            share.mkdir(parents=True)
            library.write_bytes(b"extension")
            (share / "pg_accel.control").write_text("control", encoding="utf-8")
            (share / "pg_accel--0.1.0.sql").write_text("sql", encoding="utf-8")

            normalized = package_extension.normalize_package_tree(root, "Darwin")
            self.assertEqual(normalized, root / "lib" / "pg_accel.dylib")
            self.assertTrue((root / "share" / "extension" / "pg_accel.control").is_file())
            relative_parts = {
                part.casefold()
                for path in root.rglob("*")
                for part in path.relative_to(root).parts
            }
            self.assertFalse({"users", "home", "runner", "worktrees"} & relative_parts)

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

    def test_homebrew_absolute_load_allowlist_is_kind_specific(self) -> None:
        for kind, value in (
            ("dependency", "/opt/homebrew/opt/llvm@20/lib/libLLVM.dylib"),
            ("LC_RPATH", "/opt/homebrew/opt/llvm@20/lib"),
            ("dependency", "/opt/homebrew/opt/libomp/lib/libomp.dylib"),
        ):
            package_extension.validate_load_value(value, kind, "Darwin")

        for kind, value in (
            ("dependency", "/opt/homebrew/opt/llvm@20/lib"),
            ("dependency", "/opt/homebrew/opt/llvm@20/lib/libLLVM.20.dylib"),
            ("dependency", "/opt/homebrew/opt/llvm@20/lib/libclang.dylib"),
            ("LC_RPATH", "/opt/homebrew/opt/llvm@20/lib/"),
            ("LC_RPATH", "/opt/homebrew/opt/llvm@20/lib/libLLVM.dylib"),
            ("dependency", "/opt/homebrew/opt/libomp/lib/libomp.5.dylib"),
            ("dependency", "/opt/homebrew/opt/libomp/lib/libunexpected.dylib"),
            ("dependency", "/opt/homebrew/opt/libomp/lib"),
            ("LC_RPATH", "/opt/homebrew/opt/libomp/lib"),
            ("LC_RPATH", "/opt/homebrew/opt/libomp/lib/libomp.dylib"),
            ("LC_RPATH", "/opt/homebrew/opt/libomp/lib/"),
        ):
            with self.subTest(kind=kind, value=value), self.assertRaisesRegex(
                package_extension.PackageError, "unexpected absolute"
            ):
                package_extension.validate_load_value(value, kind, "Darwin")

    def test_system_absolute_load_allowlist_is_dependency_only(self) -> None:
        for value in (
            "/usr/lib/libSystem.B.dylib",
            "/System/Library/Frameworks/Metal.framework/Versions/A/Metal",
            "/Library/Apple/System/Library/Frameworks/Foundation.framework/Foundation",
        ):
            package_extension.validate_load_value(value, "dependency", "Darwin")
            with self.subTest(value=value), self.assertRaisesRegex(
                package_extension.PackageError, "unexpected absolute"
            ):
                package_extension.validate_load_value(value, "LC_RPATH", "Darwin")

    def test_absolute_load_paths_reject_dot_and_traversal_components(self) -> None:
        for value in (
            "/opt/homebrew/opt/llvm@20/lib/../evil.dylib",
            "/opt/homebrew/opt/llvm@20/lib/./libLLVM.dylib",
            "/usr/lib/../evil.dylib",
            "/System/Library/Frameworks/../evil.dylib",
        ):
            with self.subTest(value=value), self.assertRaisesRegex(
                package_extension.PackageError, "dot path component"
            ):
                package_extension.validate_load_value(value, "dependency", "Darwin")

    def test_metal_bundle_preserves_prefix_and_requires_omp_backend(self) -> None:
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
            self.assertTrue(
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
        self.assertIn("_remove_generated_path(package_root)", helper)

    def test_linux_release_lane_builds_installs_and_loads_each_package(self) -> None:
        workflow = (REPO_ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        linux_job = workflow[
            workflow.index("  linux-package:") : workflow.index("  metal-coverage:")
        ]
        release_job = workflow[workflow.index("  release:") :]

        for required in (
            "runs-on: ubuntu-latest",
            "pg: [18, 19]",
            "libclang-dev",
            "ACPP_BACKEND=generic ./scripts/setup_acpp.sh",
            "just audit-cpu-cheats",
            "python3 scripts/package_extension.py",
            "sha256sum -c \"$archive_name.sha256\"",
            "sha256sum -c SHA256SUMS",
            '"$package_root/install.py"',
            '--destdir "$stage"',
            "dynamic_library_path",
            "extension_control_path",
            r"\$system:%s",
            "shared_preload_libraries = 'pg_accel'",
            "CREATE EXTENSION pg_accel;",
            "pg_accel_stats()",
            "target/release/pg_accel-pg${{ matrix.pg }}-linux-*.tar.gz",
        ):
            with self.subTest(required=required):
                self.assertIn(required, linux_job)
        self.assertIn("needs: [build, linux-package, metal-coverage]", release_job)

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
                        "soft_fp64_required_tag=v2.0.1",
                        "soft_fp64_desc=v2.0.1",
                        f"soft_fp64_head={'b' * 40}",
                        "soft_fp64_package_version=2.0.1",
                        "soft_fp64_cmake_args=-DSOFT_FP_BUILD_FP128=OFF",
                        "soft_fp_package_dir=/opt/soft-fp/lib/cmake/soft_fp",
                        "soft_fp64_git_status_start",
                        "soft_fp64_git_status_end",
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
            runtime_bin = package / "lib" / "pg_accel-runtime" / "bin"
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
                unpacked / "lib" / "pg_accel-runtime" / "bin"
                / "acpp-metal-archive-build"
            )
            self.assertTrue(stat.S_IMODE(unpacked_helper.stat().st_mode) & stat.S_IXUSR)
            unpacked_link = (
                unpacked / "lib" / "pg_accel-runtime" / "lib"
                / "libacpp-rt.so"
            )
            self.assertTrue(unpacked_link.is_symlink())
            self.assertEqual(os.readlink(unpacked_link), "libacpp-rt.so.1")
            package_extension.validate_checksums(unpacked)

            with tarfile.open(archive, "r:gz") as tar:
                private = {"users", "home", "runner", "worktrees", ".codex"}
                self.assertFalse(
                    any(
                        part.casefold() in private
                        for member in tar.getmembers()
                        for part in pathlib.PurePosixPath(member.name).parts
                    )
                )

    def test_installer_verifies_and_maps_normalized_payload_through_pg_config(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            package = root / "pg_accel-pg18"
            runtime_bin = package / "lib" / "pg_accel-runtime" / "bin"
            runtime_lib = runtime_bin.parent / "lib"
            extension_share = package / "share" / "extension"
            for path in (runtime_bin, runtime_lib, extension_share):
                path.mkdir(parents=True, exist_ok=True)
            suffix = ".dylib" if platform.system() == "Darwin" else ".so"
            extension_name = f"pg_accel{suffix}"
            (package / "lib" / extension_name).write_bytes(b"extension")
            helper = runtime_bin / "acpp-metal-archive-build"
            helper.write_bytes(b"helper")
            helper.chmod(0o755)
            (runtime_lib / "libacpp-rt.dylib").write_bytes(b"runtime")
            (runtime_lib / "libacpp-common.dylib").symlink_to("libacpp-rt.dylib")
            (extension_share / "pg_accel.control").write_text(
                "module_pathname = 'pg_accel'\n", encoding="utf-8"
            )
            (extension_share / "pg_accel--0.1.0.sql").write_text(
                "SELECT 1;\n", encoding="utf-8"
            )
            for name, content in (
                (".acpp-version", "pin\n"),
                ("ARCH", f"{platform.machine().lower()}\n"),
                ("LICENSE", "license\n"),
                ("NOTICE", "notice\n"),
                ("PG_MAJOR", "18\n"),
                (
                    "PLATFORM",
                    f"{'macos' if platform.system() == 'Darwin' else 'linux'}\n",
                ),
                ("pg_accel-acpp-provenance.txt", "backend=metal\n"),
            ):
                (package / name).write_text(content, encoding="utf-8")
            shutil.copy2(INSTALLER_PATH, package / "install.py")
            (package / "install.py").chmod(0o755)
            package_extension.write_checksums(package)

            pg_config = root / "pg_config"
            pg_config.write_text(
                "#!/bin/sh\n"
                "case \"$1\" in\n"
                "  --version) echo 'PostgreSQL 18.4' ;;\n"
                "  --pkglibdir) echo '/opt/pg18/lib' ;;\n"
                "  --sharedir) echo '/opt/pg18/share' ;;\n"
                "  *) exit 2 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            pg_config.chmod(0o755)
            destdir = root / "stage"
            old_runtime = destdir / "opt" / "pg18" / "lib" / "pg_accel-runtime"
            old_runtime.mkdir(parents=True)
            (old_runtime / "librt-backend-omp.stale").write_bytes(b"stale")
            pkglibdir, share_dir = install_package.install(
                package, pg_config, destdir
            )
            self.assertEqual(
                pkglibdir, destdir.resolve() / "opt" / "pg18" / "lib"
            )
            self.assertEqual(
                share_dir,
                destdir.resolve() / "opt" / "pg18" / "share" / "extension",
            )
            self.assertEqual((pkglibdir / extension_name).read_bytes(), b"extension")
            self.assertFalse(
                (pkglibdir / "pg_accel-runtime" / "librt-backend-omp.stale").exists()
            )
            if platform.system() == "Darwin":
                self.assertTrue(
                    os.access(
                        pkglibdir
                        / "pg_accel-runtime"
                        / "bin"
                        / "acpp-metal-archive-build",
                        os.X_OK,
                    )
                )
            self.assertTrue(
                (pkglibdir / "pg_accel-runtime" / "lib" / "libacpp-common.dylib").is_symlink()
            )
            self.assertTrue((share_dir / "pg_accel--0.1.0.sql").is_file())

            (package / "PG_MAJOR").write_text("19\n", encoding="ascii")
            package_extension.write_checksums(package)
            with self.assertRaisesRegex(install_package.InstallError, "requires PostgreSQL"):
                install_package.install(package, pg_config, destdir)

            (package / "PG_MAJOR").write_text("18\n", encoding="ascii")
            (extension_share / "pg_accel-evil.sql").write_text(
                "SELECT 2;\n", encoding="utf-8"
            )
            package_extension.write_checksums(package)
            with self.assertRaisesRegex(install_package.InstallError, "unexpected payload"):
                install_package.install(package, pg_config, destdir)

    def test_installer_rejects_missing_darwin_prerequisite_before_writes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            package = root / "package"
            package.mkdir()
            (package / "PLATFORM").write_text("macos\n", encoding="ascii")
            (package / "ARCH").write_text("arm64\n", encoding="ascii")
            package_extension.write_checksums(package)

            present = root / "libLLVM.dylib"
            present.write_bytes(b"llvm")
            missing = root / "libomp.dylib"
            destdir = root / "stage"
            with (
                mock.patch.object(install_package.platform, "system", return_value="Darwin"),
                mock.patch.object(install_package.platform, "machine", return_value="arm64"),
                mock.patch.object(
                    install_package,
                    "DARWIN_RUNTIME_PREREQUISITES",
                    (present, missing),
                ),
                self.assertRaisesRegex(
                    install_package.InstallError, re.escape(str(missing))
                ),
            ):
                install_package.install(package, root / "missing-pg-config", destdir)

            self.assertFalse(destdir.exists())

    def test_installer_rejects_escaping_and_multiline_pg_config_paths(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            package = root / "package"
            package.mkdir()
            (package / "PG_MAJOR").write_text("18\n", encoding="ascii")
            pg_config = root / "pg_config"
            pg_config.write_text(
                "#!/bin/sh\n"
                "case \"$1\" in\n"
                "  --version) echo 'PostgreSQL 18.4' ;;\n"
                "  --pkglibdir) echo '/opt/pg18/../../escape' ;;\n"
                "  --sharedir) echo '/opt/pg18/share' ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            pg_config.chmod(0o755)
            destdir = root / "stage"
            with self.assertRaisesRegex(install_package.InstallError, "absolute"):
                install_package.resolve_install_dirs(package, pg_config, destdir)
            self.assertFalse((root / "escape").exists())

            pg_config.write_text(
                "#!/bin/sh\n"
                "case \"$1\" in\n"
                "  --version) echo 'PostgreSQL 18.4' ;;\n"
                "  --pkglibdir) printf '/opt/pg18/lib\\n/escape\\n' ;;\n"
                "  --sharedir) echo '/opt/pg18/share' ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(install_package.InstallError, "multiline"):
                install_package.resolve_install_dirs(package, pg_config, destdir)

    def test_installer_transaction_rolls_back_all_committed_payloads(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source_tree = root / "source-runtime"
            source_tree.mkdir()
            (source_tree / "new").write_bytes(b"new-runtime")
            source_file = root / "source-extension"
            source_file.write_bytes(b"new-extension")
            destination_tree = root / "dest-runtime"
            destination_tree.mkdir()
            (destination_tree / "old").write_bytes(b"old-runtime")
            destination_file = root / "dest-extension"
            destination_file.write_bytes(b"old-extension")

            real_replace = os.replace
            calls = 0

            def fail_fourth_replace(source: object, destination: object) -> None:
                nonlocal calls
                calls += 1
                if calls == 4:
                    raise OSError("synthetic commit failure")
                real_replace(source, destination)

            with mock.patch.object(
                install_package.os, "replace", side_effect=fail_fourth_replace
            ):
                with self.assertRaisesRegex(OSError, "synthetic commit failure"):
                    install_package._install_transaction(
                        [
                            (source_tree, destination_tree, True),
                            (source_file, destination_file, False),
                        ]
                    )
            self.assertEqual(
                (destination_tree / "old").read_bytes(), b"old-runtime"
            )
            self.assertEqual(destination_file.read_bytes(), b"old-extension")


if __name__ == "__main__":
    unittest.main()
