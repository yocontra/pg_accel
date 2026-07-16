#!/usr/bin/env python3
"""Build and validate a runtime-complete, relocatable pgrx package."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import os
import pathlib
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
from collections.abc import Iterable


class PackageError(RuntimeError):
    pass


def run(command: list[str], *, env: dict[str, str] | None = None) -> str:
    try:
        result = subprocess.run(
            command,
            check=True,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        output = getattr(error, "stdout", "") or ""
        raise PackageError(f"command failed: {' '.join(command)}\n{output}") from error
    return result.stdout


def discover_extension(package_root: pathlib.Path, system: str) -> pathlib.Path:
    suffix = ".dylib" if system == "Darwin" else ".so"
    candidates = sorted(package_root.rglob(f"pg_accel{suffix}"))
    candidates = [path for path in candidates if path.is_file() and not path.is_symlink()]
    if len(candidates) != 1:
        found = ", ".join(str(path) for path in candidates) or "none"
        raise PackageError(
            f"expected exactly one packaged pg_accel{suffix}, found: {found}"
        )
    return candidates[0]


def _copy_entry(source: pathlib.Path, destination: pathlib.Path) -> None:
    if source.is_symlink():
        target = os.readlink(source)
        if os.path.isabs(target):
            raise PackageError(f"runtime symlink has an absolute target: {source} -> {target}")
        destination.symlink_to(target)
    else:
        shutil.copy2(source, destination)


def _runtime_libraries(acpp_lib: pathlib.Path, system: str) -> list[pathlib.Path]:
    suffix = ".dylib" if system == "Darwin" else ".so"
    libraries: list[pathlib.Path] = []
    for stem in ("libacpp-rt", "libacpp-common"):
        matches = sorted(
            path
            for path in acpp_lib.glob(f"{stem}{suffix}*")
            if path.is_file() or path.is_symlink()
        )
        if not matches:
            raise PackageError(f"AdaptiveCpp runtime library is missing: {stem}{suffix}*")
        libraries.extend(matches)
    return libraries


def bundle_runtime(
    extension: pathlib.Path, acpp_prefix: pathlib.Path, system: str
) -> pathlib.Path:
    acpp_lib = acpp_prefix / "lib"
    hipsycl = acpp_lib / "hipSYCL"
    if not hipsycl.is_dir():
        raise PackageError(f"AdaptiveCpp runtime subtree is missing: {hipsycl}")

    runtime = extension.parent / "pg_accel-runtime"
    temporary = extension.parent / ".pg_accel-runtime.tmp"
    shutil.rmtree(temporary, ignore_errors=True)
    shutil.rmtree(runtime, ignore_errors=True)
    (temporary / "lib").mkdir(parents=True)

    for source in _runtime_libraries(acpp_lib, system):
        _copy_entry(source, temporary / "lib" / source.name)

    def ignore_unsupported_backend(directory: str, names: list[str]) -> set[str]:
        if system == "Darwin" and pathlib.Path(directory) == hipsycl:
            return {name for name in names if name.startswith("librt-backend-omp.")}
        return set()

    shutil.copytree(
        hipsycl,
        temporary / "lib" / "hipSYCL",
        symlinks=True,
        ignore=ignore_unsupported_backend,
    )

    if system == "Darwin":
        helper = acpp_prefix / "bin" / "acpp-metal-archive-build"
        if not helper.is_file():
            raise PackageError(f"AdaptiveCpp Metal archive helper is missing: {helper}")
        (temporary / "bin").mkdir()
        shutil.copy2(helper, temporary / "bin" / helper.name)

    temporary.rename(runtime)
    return runtime


def _assert_runtime_layout(runtime: pathlib.Path, system: str) -> None:
    suffix = ".dylib" if system == "Darwin" else ".so"
    for stem in ("libacpp-rt", "libacpp-common"):
        if not list((runtime / "lib").glob(f"{stem}{suffix}*")):
            raise PackageError(f"packaged runtime is missing {stem}{suffix}")

    hipsycl = runtime / "lib" / "hipSYCL"
    bitcode = hipsycl / "bitcode"
    if not bitcode.is_dir() or not any(bitcode.glob("*.bc")):
        raise PackageError("packaged runtime has no AdaptiveCpp SSCP bitcode")

    if system == "Darwin":
        required = (
            runtime / "bin" / "acpp-metal-archive-build",
            hipsycl / "librt-backend-metal.dylib",
            hipsycl / "bitcode" / "libkernel-sscp-metal-full.bc",
            hipsycl / "llvm-to-backend" / "libllvm-to-backend.dylib",
            hipsycl / "llvm-to-backend" / "libllvm-to-metal.dylib",
        )
        missing = [str(path) for path in required if not path.is_file()]
        if missing:
            raise PackageError(f"packaged Metal runtime is incomplete: {', '.join(missing)}")
        if list(hipsycl.glob("librt-backend-omp.*")):
            raise PackageError("packaged Metal runtime must not contain the OMP backend")
        if not os.access(runtime / "bin" / "acpp-metal-archive-build", os.X_OK):
            raise PackageError("packaged Metal archive helper is not executable")
    elif system == "Linux":
        base_compiler = hipsycl / "llvm-to-backend" / "libllvm-to-backend.so"
        backend_specs = {
            "omp": ("host", "host"),
            "cuda": ("ptx", "ptx"),
            "hip": ("amdgpu", "amdgpu-amdhsa"),
            "ocl": ("spirv", "spirv"),
            "ze": ("spirv", "spirv"),
            "vk": ("clspv", "spirv"),
        }
        complete_backend = False
        for backend, (compiler, bitcode_target) in backend_specs.items():
            plugin = hipsycl / f"librt-backend-{backend}.so"
            target_compiler = hipsycl / "llvm-to-backend" / f"libllvm-to-{compiler}.so"
            full_bitcode = hipsycl / "bitcode" / f"libkernel-sscp-{bitcode_target}-full.bc"
            if plugin.is_file() and target_compiler.is_file() and full_bitcode.is_file():
                complete_backend = True
                break
        if not base_compiler.is_file() or not complete_backend:
            raise PackageError(
                "packaged Linux runtime requires a backend plugin with its matching "
                "base/target compiler plugins and full SSCP bitcode image"
            )

    for path in runtime.rglob("*"):
        if not path.is_symlink():
            continue
        target = os.readlink(path)
        if os.path.isabs(target) or not path.resolve().is_relative_to(runtime.resolve()):
            raise PackageError(f"non-relocatable runtime symlink: {path} -> {target}")
        if not path.exists():
            raise PackageError(f"dangling runtime symlink: {path} -> {target}")


def _private_absolute(value: str) -> bool:
    return value.startswith(
        ("/Users/", "/home/", "/private/tmp/", "/tmp/", "/var/folders/")
    ) or "/.codex/worktrees/" in value


def validate_load_value(value: str, kind: str, system: str) -> None:
    if not value.startswith("/"):
        if system == "Darwin":
            if kind == "LC_RPATH" and not value.startswith("@loader_path"):
                raise PackageError(f"non-relocatable relative {kind} remains: {value}")
            if kind in {"dependency", "LC_ID_DYLIB"} and not value.startswith(
                ("@rpath/", "@loader_path/")
            ):
                raise PackageError(f"non-relocatable relative {kind} remains: {value}")
        elif kind in {"RPATH", "RUNPATH"}:
            if not value.startswith(("$ORIGIN", "${ORIGIN}")):
                raise PackageError(f"non-relocatable relative {kind} remains: {value}")
        elif kind in {"NEEDED", "SONAME"} and (
            "/" in value or value in {".", "..", ""}
        ):
            raise PackageError(f"non-relocatable relative {kind} remains: {value}")
        return
    if kind == "LC_ID_DYLIB":
        raise PackageError(f"absolute LC_ID_DYLIB remains in package: {value}")
    if _private_absolute(value):
        raise PackageError(f"private absolute {kind} remains in package: {value}")
    if system == "Darwin":
        allowed = (
            "/usr/lib/",
            "/System/Library/",
            "/Library/Apple/System/Library/",
            "/opt/homebrew/opt/llvm@20/lib/",
        )
        if value != "/opt/homebrew/opt/llvm@20/lib" and not value.startswith(allowed):
            raise PackageError(f"unexpected absolute {kind} remains in package: {value}")
    else:
        raise PackageError(f"absolute ELF {kind} remains in package: {value}")


def _is_macho(path: pathlib.Path) -> bool:
    if not path.is_file() or path.is_symlink():
        return False
    with path.open("rb") as stream:
        magic = stream.read(4)
    return magic in {
        b"\xfe\xed\xfa\xce",
        b"\xce\xfa\xed\xfe",
        b"\xfe\xed\xfa\xcf",
        b"\xcf\xfa\xed\xfe",
        b"\xca\xfe\xba\xbe",
        b"\xbe\xba\xfe\xca",
        b"\xca\xfe\xba\xbf",
        b"\xbf\xba\xfe\xca",
    }


def _mac_id(path: pathlib.Path) -> str | None:
    lines = [line.strip() for line in run(["otool", "-D", str(path)]).splitlines()]
    return lines[1] if len(lines) > 1 else None


def _mac_dependencies(path: pathlib.Path) -> list[str]:
    lines = run(["otool", "-L", str(path)]).splitlines()[1:]
    return [line.strip().split(" (", 1)[0] for line in lines if line.strip()]


def _mac_rpaths(path: pathlib.Path) -> list[str]:
    output = run(["otool", "-l", str(path)])
    return re.findall(r"\n\s*cmd LC_RPATH\n\s*cmdsize \d+\n\s*path (.+?) \(offset", output)


def _expand_relative_loader_path(
    value: str, binary: pathlib.Path, allowed_root: pathlib.Path
) -> pathlib.Path:
    if not value.startswith("@loader_path"):
        raise PackageError(f"unsupported loader-relative path: {value}")
    suffix = value.removeprefix("@loader_path").lstrip("/")
    resolved = (binary.parent / suffix).resolve()
    if not resolved.is_relative_to(allowed_root.resolve()):
        raise PackageError(f"loader path escapes packaged runtime: {binary}: {value}")
    return resolved


def _validate_macos_closure(
    binary: pathlib.Path, package_lib: pathlib.Path, identifier: str | None
) -> None:
    rpaths = _mac_rpaths(binary)
    expanded_rpaths: list[pathlib.Path] = []
    for rpath in rpaths:
        validate_load_value(rpath, "LC_RPATH", "Darwin")
        if rpath.startswith("@loader_path"):
            expanded = _expand_relative_loader_path(rpath, binary, package_lib)
            if not expanded.is_dir():
                raise PackageError(f"LC_RPATH does not resolve to a packaged directory: {rpath}")
            expanded_rpaths.append(expanded)

    for dependency in _mac_dependencies(binary):
        if dependency == identifier:
            continue
        validate_load_value(dependency, "dependency", "Darwin")
        if dependency.startswith("@rpath/"):
            name = dependency.removeprefix("@rpath/")
            if "/" in name or name in {"", ".", ".."}:
                raise PackageError(f"invalid @rpath dependency: {dependency}")
            if not any((directory / name).is_file() for directory in expanded_rpaths):
                raise PackageError(f"unresolved packaged @rpath dependency: {binary}: {dependency}")
        elif dependency.startswith("@loader_path/"):
            resolved = _expand_relative_loader_path(dependency, binary, package_lib)
            if not resolved.is_file():
                raise PackageError(
                    f"unresolved packaged @loader_path dependency: {binary}: {dependency}"
                )


def validate_macos(extension: pathlib.Path, runtime: pathlib.Path) -> None:
    expected_rpath = "@loader_path/pg_accel-runtime/lib"
    if _mac_id(extension) != "@rpath/pg_accel.dylib":
        raise PackageError("packaged extension has a non-relocatable LC_ID_DYLIB")
    if _mac_rpaths(extension) != [expected_rpath]:
        raise PackageError(
            f"packaged extension LC_RPATH must be exactly {expected_rpath}"
        )
    dependencies = _mac_dependencies(extension)
    for required in ("@rpath/libacpp-rt.dylib", "@rpath/libacpp-common.dylib"):
        if required not in dependencies:
            raise PackageError(f"packaged extension is missing dependency {required}")

    binaries = [extension, *sorted(path for path in runtime.rglob("*") if _is_macho(path))]
    for binary in binaries:
        identifier = _mac_id(binary)
        if identifier is not None:
            validate_load_value(identifier, "LC_ID_DYLIB", "Darwin")
        _validate_macos_closure(binary, extension.parent, identifier)
        run(["codesign", "--verify", "--strict", str(binary)])


def _is_elf(path: pathlib.Path) -> bool:
    if not path.is_file() or path.is_symlink():
        return False
    with path.open("rb") as stream:
        return stream.read(4) == b"\x7fELF"


def _elf_dynamic(path: pathlib.Path) -> dict[str, list[str]]:
    output = run(["readelf", "-d", str(path)])
    values: dict[str, list[str]] = {"NEEDED": [], "RPATH": [], "RUNPATH": [], "SONAME": []}
    for line in output.splitlines():
        match = re.search(r"\((NEEDED|RPATH|RUNPATH|SONAME)\).*\[(.*?)\]", line)
        if match:
            values[match.group(1)].append(match.group(2))
    return values


def validate_linux(extension: pathlib.Path, runtime: pathlib.Path) -> None:
    dynamic = _elf_dynamic(extension)
    paths = dynamic["RUNPATH"] or dynamic["RPATH"]
    expected = "$ORIGIN/pg_accel-runtime/lib"
    if paths != [expected]:
        raise PackageError(f"packaged extension ELF RUNPATH must be exactly {expected}")
    needed = dynamic["NEEDED"]
    for required in ("libacpp-rt.so", "libacpp-common.so"):
        if not any(value == required or value.startswith(f"{required}.") for value in needed):
            raise PackageError(f"packaged extension is missing dependency {required}")

    binaries = [extension, *sorted(path for path in runtime.rglob("*") if _is_elf(path))]
    packaged_names = {path.name for path in runtime.rglob("*") if path.is_file()}
    for binary in binaries:
        for kind, values in _elf_dynamic(binary).items():
            for value in values:
                components = value.split(":") if kind in {"RPATH", "RUNPATH"} else [value]
                for component in components:
                    validate_load_value(component, kind, "Linux")
                    if kind in {"RPATH", "RUNPATH"}:
                        token = "${ORIGIN}" if component.startswith("${ORIGIN}") else "$ORIGIN"
                        suffix = component.removeprefix(token).lstrip("/")
                        resolved = (binary.parent / suffix).resolve()
                        if not resolved.is_relative_to(extension.parent.resolve()):
                            raise PackageError(
                                f"ELF loader path escapes packaged runtime: {binary}: {component}"
                            )
                        if not resolved.is_dir():
                            raise PackageError(
                                f"ELF loader path does not resolve: {binary}: {component}"
                            )
                    elif kind == "NEEDED" and component.startswith(
                        ("libacpp-", "librt-backend-", "libllvm-to-")
                    ):
                        if component not in packaged_names:
                            raise PackageError(
                                f"unresolved packaged ELF dependency: {binary}: {component}"
                            )


def validate_package(
    package_root: pathlib.Path,
    extension: pathlib.Path,
    runtime: pathlib.Path,
    system: str,
) -> None:
    for metadata in (
        "LICENSE",
        "NOTICE",
        ".acpp-version",
        "pg_accel-acpp-provenance.txt",
    ):
        if not (package_root / metadata).is_file():
            raise PackageError(f"package metadata is missing: {metadata}")
    _assert_runtime_layout(runtime, system)
    if system == "Darwin":
        validate_macos(extension, runtime)
    elif system == "Linux":
        validate_linux(extension, runtime)
    else:
        raise PackageError(f"unsupported package platform: {system}")


def copy_sanitized_provenance(
    acpp_prefix: pathlib.Path, package_root: pathlib.Path, required_sha: str
) -> pathlib.Path:
    source = acpp_prefix / "pg_accel-acpp-provenance.txt"
    if not source.is_file():
        raise PackageError(f"AdaptiveCpp setup provenance is missing: {source}")
    text = source.read_text(encoding="utf-8")
    required_fields = (
        "backend=",
        "targets=",
        f"acpp_required_sha={required_sha}",
        f"acpp_head={required_sha}",
        "soft_fp64_required_tag=",
        "soft_fp64_head=",
        "cmake_args=",
        "acpp_git_status_start",
        "acpp_git_status_end",
    )
    missing = [field for field in required_fields if field not in text]
    if missing:
        raise PackageError(f"AdaptiveCpp setup provenance is incomplete: {', '.join(missing)}")

    sanitized = text.replace(str(acpp_prefix), "${ACPP_PREFIX}")
    home = str(pathlib.Path.home())
    if home != "/":
        sanitized = sanitized.replace(home, "${HOME}")
    for line in sanitized.splitlines():
        if "=" not in line:
            continue
        value = line.split("=", 1)[1]
        private_markers = (
            "/Users/",
            "/home/",
            "/private/tmp/",
            "/tmp/",
            "/var/folders/",
        )
        if any(marker in value for marker in private_markers) or "/.codex/worktrees/" in value:
            raise PackageError(f"private path remains in AdaptiveCpp provenance: {line}")

    destination = package_root / "pg_accel-acpp-provenance.txt"
    destination.write_text(sanitized, encoding="utf-8")
    return destination


def _manifest_files(package_root: pathlib.Path) -> list[pathlib.Path]:
    manifest = package_root / "SHA256SUMS"
    files = sorted(
        path
        for path in package_root.rglob("*")
        if path != manifest and (path.is_file() or path.is_symlink())
    )
    for path in files:
        relative = path.relative_to(package_root).as_posix()
        if "\n" in relative or "\r" in relative:
            raise PackageError(f"package path cannot be represented in SHA256SUMS: {relative!r}")
    return files


def _sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_checksums(package_root: pathlib.Path) -> pathlib.Path:
    manifest = package_root / "SHA256SUMS"
    lines = [
        f"{_sha256(path)}  {path.relative_to(package_root).as_posix()}"
        for path in _manifest_files(package_root)
    ]
    if not lines:
        raise PackageError("refusing to write an empty package checksum manifest")
    manifest.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return manifest


def validate_checksums(package_root: pathlib.Path) -> None:
    manifest = package_root / "SHA256SUMS"
    if not manifest.is_file() or manifest.is_symlink():
        raise PackageError("package checksum manifest is missing or is a symlink")
    expected_lines = [
        f"{_sha256(path)}  {path.relative_to(package_root).as_posix()}"
        for path in _manifest_files(package_root)
    ]
    actual_lines = manifest.read_text(encoding="utf-8").splitlines()
    if actual_lines != expected_lines:
        raise PackageError("package SHA256SUMS is incomplete, stale, or non-deterministic")


def _archive_paths(package_root: pathlib.Path) -> list[pathlib.Path]:
    return [package_root, *sorted(package_root.rglob("*"))]


def validate_release_archive(
    package_root: pathlib.Path, archive: pathlib.Path, outer_checksum: pathlib.Path
) -> None:
    expected_outer = f"{_sha256(archive)}  {archive.name}\n"
    if outer_checksum.read_text(encoding="utf-8") != expected_outer:
        raise PackageError("release archive checksum is missing or stale")

    expected: dict[str, tuple[int, int, str]] = {}
    for path in _archive_paths(package_root):
        relative = path.relative_to(package_root)
        name = package_root.name if relative == pathlib.Path(".") else (
            pathlib.PurePosixPath(package_root.name) / relative.as_posix()
        ).as_posix()
        metadata = path.lstat()
        expected[name] = (
            stat.S_IFMT(metadata.st_mode),
            stat.S_IMODE(metadata.st_mode),
            os.readlink(path) if path.is_symlink() else "",
        )

    with tarfile.open(archive, "r:gz") as tar:
        members = {member.name: member for member in tar.getmembers()}
    if set(members) != set(expected):
        raise PackageError("release archive contents differ from the validated package tree")
    for name, (file_type, mode, linkname) in expected.items():
        member = members[name]
        member_type = (
            stat.S_IFLNK
            if member.issym()
            else stat.S_IFDIR
            if member.isdir()
            else stat.S_IFREG
            if member.isfile()
            else 0
        )
        if member_type != file_type or member.mode != mode:
            raise PackageError(f"release archive changed file type or mode: {name}")
        if member.issym() and member.linkname != linkname:
            raise PackageError(f"release archive changed symlink target: {name}")


def create_release_archive(
    package_root: pathlib.Path,
    pg: str,
    *,
    system: str | None = None,
    machine: str | None = None,
) -> tuple[pathlib.Path, pathlib.Path]:
    system_name = system or platform.system()
    platform_name = {"Darwin": "macos", "Linux": "linux"}.get(system_name)
    if platform_name is None:
        raise PackageError(f"unsupported archive platform: {system_name}")
    architecture = (machine or platform.machine()).lower()
    if not re.fullmatch(r"[a-z0-9_+-]+", architecture):
        raise PackageError(f"unsafe archive architecture name: {architecture!r}")

    for stale in package_root.parent.glob(f"pg_accel-pg{pg}-*.tar.gz*"):
        if stale.is_file() or stale.is_symlink():
            stale.unlink()
    archive = package_root.parent / f"pg_accel-pg{pg}-{platform_name}-{architecture}.tar.gz"
    outer_checksum = archive.with_name(f"{archive.name}.sha256")
    archive.unlink(missing_ok=True)
    outer_checksum.unlink(missing_ok=True)

    with archive.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as tar:
                for path in _archive_paths(package_root):
                    relative = path.relative_to(package_root)
                    arcname = package_root.name if relative == pathlib.Path(".") else (
                        pathlib.PurePosixPath(package_root.name) / relative.as_posix()
                    ).as_posix()
                    info = tar.gettarinfo(str(path), arcname=arcname)
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    info.mtime = 0
                    info.pax_headers = {}
                    if info.isfile():
                        with path.open("rb") as source:
                            tar.addfile(info, source)
                    else:
                        tar.addfile(info)

    outer_checksum.write_text(
        f"{_sha256(archive)}  {archive.name}\n", encoding="utf-8"
    )
    validate_release_archive(package_root, archive, outer_checksum)
    return archive, outer_checksum


def _target_dir(repo_root: pathlib.Path) -> pathlib.Path:
    configured = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    return configured if configured.is_absolute() else repo_root / configured


def build_package(
    args: argparse.Namespace,
) -> tuple[pathlib.Path, pathlib.Path, pathlib.Path]:
    repo_root = pathlib.Path(__file__).resolve().parent.parent
    acpp_prefix = args.acpp_prefix.resolve()
    if not acpp_prefix.is_dir():
        raise PackageError(f"AdaptiveCpp prefix does not exist: {acpp_prefix}")

    environment = os.environ.copy()
    environment["PGACCEL_PACKAGE_RELOCATABLE"] = "1"
    environment["ACPP_PREFIX"] = str(acpp_prefix)
    command = [
        "cargo",
        "pgrx",
        "package",
        "--package",
        "pg_accel",
        "--pg-config",
        str(args.pg_config),
        "--no-default-features",
        "--features",
        f"pg{args.pg}",
    ]
    package_root = _target_dir(repo_root) / "release" / f"pg_accel-pg{args.pg}"
    shutil.rmtree(package_root, ignore_errors=True)
    for stale in package_root.parent.glob(f"pg_accel-pg{args.pg}-*.tar.gz*"):
        if stale.is_file() or stale.is_symlink():
            stale.unlink()
    subprocess.run(command, cwd=repo_root, env=environment, check=True)

    if not package_root.is_dir():
        raise PackageError(f"cargo pgrx did not create expected package: {package_root}")
    for metadata in ("LICENSE", "NOTICE", ".acpp-version"):
        shutil.copy2(repo_root / metadata, package_root / metadata)
    required_sha = (repo_root / ".acpp-version").read_text(encoding="utf-8").strip()
    copy_sanitized_provenance(acpp_prefix, package_root, required_sha)

    system = platform.system()
    extension = discover_extension(package_root, system)
    runtime = bundle_runtime(extension, acpp_prefix, system)
    validate_package(package_root, extension, runtime, system)
    write_checksums(package_root)
    validate_checksums(package_root)
    archive, outer_checksum = create_release_archive(package_root, args.pg, system=system)
    return package_root, archive, outer_checksum


def parse_args(argv: Iterable[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pg", required=True, choices=("18", "19"))
    parser.add_argument("--pg-config", required=True, type=pathlib.Path)
    parser.add_argument("--acpp-prefix", required=True, type=pathlib.Path)
    return parser.parse_args(argv)


def main(argv: Iterable[str] | None = None) -> int:
    try:
        package, archive, outer_checksum = build_package(parse_args(argv))
    except (PackageError, OSError, subprocess.CalledProcessError) as error:
        print(f"error: relocatable package failed validation: {error}", file=sys.stderr)
        return 1
    print(f"relocatable package validated: {package}")
    print(f"release archive validated: {archive}")
    print(f"release archive checksum: {outer_checksum}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
