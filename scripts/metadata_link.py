#!/usr/bin/env python3
"""Bind independently replayed metadata code to actual linked export ownership.

Like the existing ABI audit, this trusts the installed compiler/linker/nm, not
arbitrary JSON assertions. Link receipts are freshness records, not signatures.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

from metadata_build import ROOTS, digest
from metadata_effects import Unproven


def read_json(path: pathlib.Path) -> dict:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise Unproven("metadata binding must be a JSON object")
    return value


def canonical(path: str, cwd: pathlib.Path) -> pathlib.Path:
    return (cwd / path).resolve()


def map_exports(text: str, cwd: pathlib.Path) -> dict[str, tuple[int, pathlib.Path]]:
    """Read live symbol ownership, never discarded symbols or name-only mentions."""
    exports: dict[str, tuple[int, pathlib.Path]] = {}

    def insert(name: str, address: int, owner: str):
        if name in exports:
            raise Unproven(f"duplicate metadata export in linker map: {name}")
        exports[name] = address, canonical(owner, cwd)

    if "# Object files:" in text and "# Symbols:" in text:
        objects: dict[int, str] = {}
        object_section = text.split("# Object files:", 1)[1].split("# Sections:", 1)[0]
        for match in re.finditer(r"^\[\s*(\d+)\]\s+(.+)$", object_section, re.M):
            number = int(match[1])
            if number in objects:
                raise Unproven("duplicate linker-map object index")
            objects[number] = match[2]
        symbols = text.split("# Symbols:", 1)[1].split("# Dead Stripped Symbols:", 1)[0]
        for match in re.finditer(
            r"^0x([\da-fA-F]+)\s+0x[\da-fA-F]+\s+\[\s*(\d+)\]\s+_(\w+)\s*$",
            symbols,
            re.M,
        ):
            if match[3] in ROOTS:
                owner = objects.get(int(match[2]))
                if owner is None:
                    raise Unproven("metadata export has no linker-map object")
                insert(match[3], int(match[1], 16), owner)
    else:
        ranges: list[tuple[int, int, str]] = []
        symbols: list[tuple[str, int]] = []
        if re.search(r"^\s*VMA\s+LMA\s+Size\s+Align\s+Out\s+In\s+Symbol", text, re.M):
            for match in re.finditer(
                r"^\s*([\da-fA-F]+)\s+[\da-fA-F]+\s+([\da-fA-F]+)\s+\d+\s+(.+?)\s*$",
                text,
                re.M,
            ):
                address, size, tail = int(match[1], 16), int(match[2], 16), match[3]
                owner = re.fullmatch(r"(.+):\(\.text[^)]*\)", tail)
                if owner:
                    ranges.append((address, address + size, owner[1]))
                elif tail in ROOTS:
                    symbols.append((tail, address))
        elif "Linker script and memory map" in text:
            live = text.split("Linker script and memory map", 1)[1]
            # GNU ld may wrap a long input-section name onto its own line.
            for match in re.finditer(
                r"^ \.text\S*\s+(0x[\da-fA-F]+)\s+(0x[\da-fA-F]+)\s+([^\n]+)$",
                live,
                re.M,
            ):
                start, size = int(match[1], 16), int(match[2], 16)
                ranges.append((start, start + size, match[3].strip()))
            for match in re.finditer(r"^\s+(0x[\da-fA-F]+)\s+(\w+)\s*$", live, re.M):
                if match[2] in ROOTS:
                    symbols.append((match[2], int(match[1], 16)))
        else:
            raise Unproven("unsupported or incomplete metadata linker map")
        for name, address in symbols:
            owners = [owner for start, end, owner in ranges if start <= address < end]
            if len(owners) != 1:
                raise Unproven(
                    "metadata map symbol lacks unique input-section ownership"
                )
            insert(name, address, owners[0])
    if set(exports) != set(ROOTS):
        raise Unproven("linker map does not contain both live metadata exports")
    return exports


def library_exports(library: pathlib.Path) -> dict[str, int]:
    nm = shutil.which("nm")
    if nm is None:
        raise Unproven("nm is required for metadata link ownership")
    result = subprocess.run(
        [nm, "-g", str(library)],
        text=True,
        capture_output=True,
        check=False,
        timeout=60,
    )
    if result.returncode:
        raise Unproven("could not read the library's metadata exports")
    exports: dict[str, int] = {}
    for line in result.stdout.splitlines():
        match = re.fullmatch(
            r"([\da-fA-F]+)\s+([A-Za-z])\s+_?(pgaccel_\w+)", line.strip()
        )
        if match and match[3] in ROOTS:
            if match[2] != "T" or match[3] in exports:
                raise Unproven("metadata export is not one strong text definition")
            exports[match[3]] = int(match[1], 16)
    if set(exports) != set(ROOTS):
        raise Unproven("library is missing strong metadata exports")
    return exports


def capture(
    library: pathlib.Path, linker_map: pathlib.Path, object_path: pathlib.Path
) -> dict:
    library, linker_map, object_path = (
        library.resolve(),
        linker_map.resolve(),
        object_path.resolve(),
    )
    cwd = pathlib.Path.cwd().resolve()
    artifacts = (library, linker_map, object_path)
    before = {path: digest(path.read_bytes()) for path in artifacts}
    receipt = object_path.with_name(object_path.name + ".metadata.json")
    compiled = read_json(receipt)
    if compiled.get("working_directory") != str(cwd) or compiled.get("object") != str(
        object_path
    ):
        raise Unproven("metadata object receipt belongs to another build")
    if compiled.get("object_sha256") != digest(object_path.read_bytes()):
        raise Unproven("metadata object changed before linking")
    ownership = map_exports(linker_map.read_text(encoding="utf-8"), cwd)
    addresses = library_exports(library)
    for name, (address, owner) in ownership.items():
        if owner != object_path or addresses[name] != address:
            raise Unproven(
                "linked metadata symbol is not supplied by the proved object"
            )
    after = {path: digest(path.read_bytes()) for path in artifacts}
    if before != after:
        raise Unproven("metadata link artifacts changed during ownership validation")
    return {
        "schema_version": 1,
        "kind": "metadata-link-binding",
        "working_directory": str(cwd),
        "library": str(library),
        "library_sha256": before[library],
        "linker_map": str(linker_map),
        "map_sha256": before[linker_map],
        "object": str(object_path),
        "object_sha256": before[object_path],
        "receipt_sha256": digest(receipt.read_bytes()),
        "link_driver_sha256": digest(pathlib.Path(__file__).read_bytes()),
        "exports": addresses,
    }


def verify_binding(
    binding_path: pathlib.Path,
    library: pathlib.Path,
    source: pathlib.Path,
    marker: pathlib.Path,
    *,
    compiler: pathlib.Path | None = None,
    build_root: pathlib.Path | None = None,
) -> dict:
    """Rebuild/prove independently, then compare bytes and actual symbol ownership."""
    binding = read_json(binding_path)
    library, source = library.resolve(), source.resolve()
    cwd = pathlib.Path(binding["working_directory"])
    object_path = pathlib.Path(binding["object"])
    linker_map = pathlib.Path(binding["linker_map"])
    if (
        not cwd.is_absolute()
        or not object_path.is_absolute()
        or not linker_map.is_absolute()
    ):
        raise Unproven("metadata build paths must be absolute")
    if binding.get("library") != str(library):
        raise Unproven("metadata binding belongs to another library")
    if build_root is not None:
        build_root = build_root.resolve()
        expected_directory = build_root / "CMakeFiles/pgaccel_metadata.dir"
        if (
            cwd.resolve() != build_root
            or not object_path.resolve().is_relative_to(expected_directory)
            or object_path.name != "join_rows_layout.cpp.o"
            or linker_map.resolve() != library.with_name(library.name + ".map")
        ):
            raise Unproven("metadata evidence is outside the expected release build")
    for artifact in (library, linker_map, binding_path):
        if (
            not artifact.is_file()
            or artifact.stat().st_mtime_ns <= marker.stat().st_mtime_ns
        ):
            raise Unproven("metadata link evidence predates the current rebuild marker")
    receipt_path = object_path.with_name(object_path.name + ".metadata.json")
    compiled = read_json(receipt_path)
    trusted_compiler = compiler or pathlib.Path(
        shutil.which("clang++") or "missing-clang++"
    )
    # A receipt must never select an executable. Only the compiler independently
    # selected for the ABI audit (or the caller's normal Clang) may be replayed.
    if not trusted_compiler.samefile(compiled["compiler"]):
        raise Unproven("metadata receipt compiler is not the audit compiler")
    if compiled.get("source") != str(source) or compiled.get(
        "working_directory"
    ) != str(cwd):
        raise Unproven("metadata receipt does not match the audited source/build")
    flags = compiled.get("flags")
    if not isinstance(flags, list) or not all(isinstance(flag, str) for flag in flags):
        raise Unproven("metadata flags are malformed")
    driver = pathlib.Path(__file__).with_name("metadata_build.py").resolve()
    common = [
        sys.executable,
        str(driver),
        "--compiler",
        compiled["compiler"],
        "--source",
        str(source),
        *(f"--flag={flag}" for flag in flags),
    ]
    with tempfile.TemporaryDirectory(prefix="metadata-replay-") as temporary:
        replay_object = pathlib.Path(temporary) / object_path.name
        for command in (
            [*common, "--output", str(object_path), "--verify", str(receipt_path)],
            [*common, "--output", str(replay_object)],
        ):
            result = subprocess.run(
                command,
                cwd=cwd,
                text=True,
                capture_output=True,
                check=False,
                timeout=120,
            )
            if result.returncode:
                raise Unproven(
                    f"independent metadata replay failed: {result.stderr[-2000:]}"
                )
        replayed = read_json(
            replay_object.with_name(replay_object.name + ".metadata.json")
        )
        if replayed["object_sha256"] != digest(object_path.read_bytes()):
            raise Unproven(
                "independently proved metadata object differs from linked input"
            )
    # Capture uses the same build cwd without changing this process's global cwd.
    command = [
        sys.executable,
        str(pathlib.Path(__file__).resolve()),
        "--library",
        str(library),
        "--map",
        str(linker_map),
        "--object",
        str(object_path),
    ]
    result = subprocess.run(
        command, cwd=cwd, text=True, capture_output=True, check=False, timeout=60
    )
    if result.returncode or json.loads(result.stdout) != binding:
        raise Unproven("metadata library/map/object binding is stale or inconsistent")
    return {**binding, "status": "independently_verified", "proofs": replayed["proofs"]}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", required=True, type=pathlib.Path)
    parser.add_argument("--map", required=True, type=pathlib.Path)
    parser.add_argument("--object", required=True, type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    try:
        document = (
            json.dumps(capture(args.library, args.map, args.object), indent=2) + "\n"
        )
        if args.output:
            args.output.write_text(document, encoding="utf-8")
        else:
            print(document, end="")
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"metadata link binding failed: {error}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
