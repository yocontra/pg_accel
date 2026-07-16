#!/usr/bin/env python3
"""Build and validate a runtime-complete, relocatable pgrx package."""

from __future__ import annotations

import argparse
import hashlib
import os
import pathlib
import platform
import re
import shutil
import subprocess
import sys
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
            hipsycl / "llvm-to-backend" / "libllvm-to-metal.dylib",
        )
        missing = [str(path) for path in required if not path.is_file()]
        if missing:
            raise PackageError(f"packaged Metal runtime is incomplete: {', '.join(missing)}")
        if list(hipsycl.glob("librt-backend-omp.*")):
            raise PackageError("packaged Metal runtime must not contain the OMP backend")

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
        if kind in {"LC_RPATH", "RPATH", "RUNPATH"}:
            allowed_prefixes = (
                ("@loader_path", "@executable_path")
                if system == "Darwin"
                else ("$ORIGIN", "${ORIGIN}")
            )
            if not value.startswith(allowed_prefixes):
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
        for dependency in _mac_dependencies(binary):
            validate_load_value(dependency, "dependency", "Darwin")
        for rpath in _mac_rpaths(binary):
            validate_load_value(rpath, "LC_RPATH", "Darwin")
    run(["codesign", "--verify", "--strict", str(extension)])


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
    for binary in binaries:
        for kind, values in _elf_dynamic(binary).items():
            for value in values:
                components = value.split(":") if kind in {"RPATH", "RUNPATH"} else [value]
                for component in components:
                    validate_load_value(component, kind, "Linux")


def validate_package(
    package_root: pathlib.Path,
    extension: pathlib.Path,
    runtime: pathlib.Path,
    system: str,
) -> None:
    for metadata in ("LICENSE", "NOTICE", ".acpp-version"):
        if not (package_root / metadata).is_file():
            raise PackageError(f"package metadata is missing: {metadata}")
    _assert_runtime_layout(runtime, system)
    if system == "Darwin":
        validate_macos(extension, runtime)
    elif system == "Linux":
        validate_linux(extension, runtime)
    else:
        raise PackageError(f"unsupported package platform: {system}")


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


def _target_dir(repo_root: pathlib.Path) -> pathlib.Path:
    configured = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    return configured if configured.is_absolute() else repo_root / configured


def build_package(args: argparse.Namespace) -> pathlib.Path:
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
    subprocess.run(command, cwd=repo_root, env=environment, check=True)

    package_root = _target_dir(repo_root) / "release" / f"pg_accel-pg{args.pg}"
    if not package_root.is_dir():
        raise PackageError(f"cargo pgrx did not create expected package: {package_root}")
    for metadata in ("LICENSE", "NOTICE", ".acpp-version"):
        shutil.copy2(repo_root / metadata, package_root / metadata)

    system = platform.system()
    extension = discover_extension(package_root, system)
    runtime = bundle_runtime(extension, acpp_prefix, system)
    validate_package(package_root, extension, runtime, system)
    write_checksums(package_root)
    validate_checksums(package_root)
    return package_root


def parse_args(argv: Iterable[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pg", required=True, choices=("18", "19"))
    parser.add_argument("--pg-config", required=True, type=pathlib.Path)
    parser.add_argument("--acpp-prefix", required=True, type=pathlib.Path)
    return parser.parse_args(argv)


def main(argv: Iterable[str] | None = None) -> int:
    try:
        package = build_package(parse_args(argv))
    except (PackageError, OSError, subprocess.CalledProcessError) as error:
        print(f"error: relocatable package failed validation: {error}", file=sys.stderr)
        return 1
    print(f"relocatable package validated: {package}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
