#!/usr/bin/env python3
"""Install a validated pg_accel release package using a target pg_config."""

from __future__ import annotations

import argparse
import hashlib
import os
import pathlib
import platform
import re
import shutil
import stat
import subprocess
import sys
from collections.abc import Iterable


class InstallError(RuntimeError):
    pass


def _sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _package_files(package_root: pathlib.Path) -> list[pathlib.Path]:
    if package_root.is_symlink() or not package_root.is_dir():
        raise InstallError("package root must be a real directory")
    manifest = package_root / "SHA256SUMS"
    files: list[pathlib.Path] = []
    for path in package_root.rglob("*"):
        mode = path.lstat().st_mode
        if stat.S_ISDIR(mode):
            continue
        if stat.S_ISLNK(mode):
            resolved = path.resolve()
            if not resolved.is_relative_to(package_root.resolve()) or not resolved.is_file():
                raise InstallError(f"unsafe package symlink: {path.relative_to(package_root)}")
            files.append(path)
            continue
        if not stat.S_ISREG(mode):
            raise InstallError(f"package contains a special file: {path.relative_to(package_root)}")
        if path != manifest:
            files.append(path)
    return sorted(files)


def verify_checksums(package_root: pathlib.Path) -> None:
    manifest = package_root / "SHA256SUMS"
    if not manifest.is_file() or manifest.is_symlink():
        raise InstallError("SHA256SUMS is missing or is a symlink")

    expected: dict[str, str] = {}
    for line in manifest.read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  (.+)", line)
        if match is None:
            raise InstallError(f"invalid SHA256SUMS line: {line!r}")
        relative = pathlib.PurePosixPath(match.group(2))
        if relative.is_absolute() or ".." in relative.parts or "." in relative.parts:
            raise InstallError(f"unsafe SHA256SUMS path: {relative}")
        name = relative.as_posix()
        if name == "SHA256SUMS" or name in expected:
            raise InstallError(f"duplicate or recursive SHA256SUMS path: {name}")
        expected[name] = match.group(1)

    actual_paths = _package_files(package_root)
    actual_names = {path.relative_to(package_root).as_posix() for path in actual_paths}
    if set(expected) != actual_names:
        raise InstallError("SHA256SUMS does not cover the exact package file set")
    for path in actual_paths:
        relative = path.relative_to(package_root).as_posix()
        if _sha256(path) != expected[relative]:
            raise InstallError(f"checksum mismatch: {relative}")


def _pg_config(pg_config: pathlib.Path, option: str) -> str:
    try:
        result = subprocess.run(
            [str(pg_config), option],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise InstallError(f"pg_config {option} failed: {error}") from error
    if "\0" in result.stdout or len(result.stdout.splitlines()) != 1:
        raise InstallError(f"pg_config {option} returned malformed multiline output")
    value = result.stdout.strip()
    if not value or "\n" in value or "\r" in value:
        raise InstallError(f"pg_config {option} returned an empty value")
    return value


def resolve_install_dirs(
    package_root: pathlib.Path, pg_config: pathlib.Path, destdir: pathlib.Path | None
) -> tuple[pathlib.Path, pathlib.Path]:
    expected_major = (package_root / "PG_MAJOR").read_text(encoding="ascii").strip()
    if expected_major not in {"18", "19"}:
        raise InstallError(f"invalid package PG_MAJOR: {expected_major!r}")
    version = _pg_config(pg_config, "--version")
    match = re.fullmatch(r"PostgreSQL (\d+)[A-Za-z0-9.+_-]*", version)
    if match is None or match.group(1) != expected_major:
        found = match.group(1) if match else version
        raise InstallError(
            f"package requires PostgreSQL {expected_major}, pg_config reports {found}"
        )

    pkglibdir = pathlib.Path(_pg_config(pg_config, "--pkglibdir"))
    extension_dir = pathlib.Path(_pg_config(pg_config, "--sharedir")) / "extension"
    raw_paths = (str(pkglibdir), str(extension_dir))
    if not pkglibdir.is_absolute() or not extension_dir.is_absolute() or any(
        component in {".", ".."}
        for value in raw_paths
        for component in value.split("/")
    ):
        raise InstallError("pg_config install directories must be absolute")
    if destdir is None:
        return pkglibdir, extension_dir

    root = destdir.resolve()
    mapped = (
        root / pkglibdir.relative_to(pkglibdir.anchor),
        root / extension_dir.relative_to(extension_dir.anchor),
    )
    resolved = tuple(path.resolve() for path in mapped)
    if any(not path.is_relative_to(root) for path in resolved):
        raise InstallError("DESTDIR mapping escapes the requested staging root")
    return resolved


def _remove_path(path: pathlib.Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink()
    elif path.exists():
        shutil.rmtree(path)


def _install_transaction(
    payloads: list[tuple[pathlib.Path, pathlib.Path, bool]],
) -> None:
    staged: list[tuple[pathlib.Path, pathlib.Path, pathlib.Path]] = []
    committed: list[tuple[pathlib.Path, pathlib.Path, bool]] = []
    try:
        for source, destination, is_tree in payloads:
            destination.parent.mkdir(parents=True, exist_ok=True)
            new_path = destination.parent / f".{destination.name}.pgaccel-new-{os.getpid()}"
            old_path = destination.parent / f".{destination.name}.pgaccel-old-{os.getpid()}"
            _remove_path(new_path)
            _remove_path(old_path)
            staged.append((new_path, destination, old_path))
            if is_tree:
                shutil.copytree(source, new_path, symlinks=True)
            else:
                shutil.copy2(source, new_path)

        for new_path, destination, old_path in staged:
            had_old = destination.exists() or destination.is_symlink()
            if had_old:
                os.replace(destination, old_path)
            try:
                os.replace(new_path, destination)
            except BaseException:
                if had_old:
                    os.replace(old_path, destination)
                raise
            committed.append((destination, old_path, had_old))

    except BaseException:
        for destination, old_path, had_old in reversed(committed):
            _remove_path(destination)
            if had_old and (old_path.exists() or old_path.is_symlink()):
                os.replace(old_path, destination)
        for new_path, _, old_path in staged:
            _remove_path(new_path)
            if old_path.exists() or old_path.is_symlink():
                _remove_path(old_path)
        raise
    for _, old_path, had_old in committed:
        if had_old:
            _remove_path(old_path)


def install(
    package_root: pathlib.Path, pg_config: pathlib.Path, destdir: pathlib.Path | None
) -> tuple[pathlib.Path, pathlib.Path]:
    if package_root.is_symlink():
        raise InstallError("package root must not be a symlink")
    package_root = package_root.resolve()
    verify_checksums(package_root)
    system = platform.system()
    expected_platform = {"Darwin": "macos", "Linux": "linux"}.get(system)
    package_platform = (package_root / "PLATFORM").read_text(encoding="ascii").strip()
    package_arch = (package_root / "ARCH").read_text(encoding="ascii").strip()
    if expected_platform is None or package_platform != expected_platform:
        raise InstallError(
            f"package platform {package_platform!r} does not match host {system!r}"
        )
    if package_arch != platform.machine().lower():
        raise InstallError(
            f"package architecture {package_arch!r} does not match host "
            f"{platform.machine().lower()!r}"
        )
    pkglibdir, extension_dir = resolve_install_dirs(package_root, pg_config, destdir)

    library_dir = package_root / "lib"
    runtime = library_dir / "pg_accel-runtime"
    if library_dir.is_symlink() or not library_dir.is_dir():
        raise InstallError("package lib must be a real directory")
    if runtime.is_symlink() or not runtime.is_dir():
        raise InstallError("pg_accel-runtime must be a real directory")
    if system == "Darwin":
        helper = runtime / "bin" / "acpp-metal-archive-build"
        if helper.is_symlink() or not helper.is_file() or not os.access(helper, os.X_OK):
            raise InstallError("packaged Metal archive helper is missing or not executable")
    expected_suffix = ".dylib" if system == "Darwin" else ".so"
    extensions = [
        path
        for path in library_dir.glob("pg_accel.*")
        if path.name == f"pg_accel{expected_suffix}"
        and path.is_file()
        and not path.is_symlink()
    ]
    if len(extensions) != 1 or not runtime.is_dir():
        raise InstallError("package lib must contain one extension and pg_accel-runtime")
    if {path.name for path in library_dir.iterdir()} != {
        extensions[0].name,
        "pg_accel-runtime",
    }:
        raise InstallError("package lib contains an unexpected install payload")

    source_extension_dir = package_root / "share" / "extension"
    control = source_extension_dir / "pg_accel.control"
    sql_name = re.compile(
        r"pg_accel--\d+\.\d+\.\d+(?:--\d+\.\d+\.\d+)?\.sql"
    )
    if source_extension_dir.is_symlink() or not source_extension_dir.is_dir():
        raise InstallError("package share/extension must be a real directory")
    sql_files = sorted(
        path
        for path in source_extension_dir.iterdir()
        if sql_name.fullmatch(path.name) and path.is_file() and not path.is_symlink()
    )
    if control.is_symlink() or not control.is_file() or not sql_files:
        raise InstallError("package share/extension payload is incomplete")
    expected_share = {"pg_accel.control", *(path.name for path in sql_files)}
    if {path.name for path in source_extension_dir.iterdir()} != expected_share:
        raise InstallError("package share/extension contains an unexpected payload")

    payloads = [
        (runtime, pkglibdir / runtime.name, True),
        (extensions[0], pkglibdir / extensions[0].name, False),
        *((source, extension_dir / source.name, False) for source in (control, *sql_files)),
    ]
    _install_transaction(payloads)
    return pkglibdir, extension_dir


def parse_args(argv: Iterable[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pg-config", required=True, type=pathlib.Path)
    parser.add_argument(
        "--package-root",
        type=pathlib.Path,
        default=pathlib.Path(__file__).resolve().parent,
    )
    parser.add_argument("--destdir", type=pathlib.Path)
    return parser.parse_args(argv)


def main(argv: Iterable[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        pkglibdir, extension_dir = install(
            args.package_root, args.pg_config, args.destdir
        )
    except (InstallError, OSError) as error:
        print(f"error: pg_accel package installation failed: {error}", file=sys.stderr)
        return 1
    print(f"installed pg_accel library payload: {pkglibdir}")
    print(f"installed pg_accel extension metadata: {extension_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
