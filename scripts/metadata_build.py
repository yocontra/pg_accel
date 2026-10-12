#!/usr/bin/env python3
"""Build and replay a typed metadata proof against the exact object input.

Build driver, not a release-audit exemption. A receipt binds compiler,
flags, source, the transitive preprocessed input, verifier implementation and
object bytes. It does NOT prove that a library linked this object; that remains
the responsibility of the release link/inventory gate.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import pathlib
import re
import subprocess
import tempfile

from metadata_effects import (
    COMPILER_PROFILE,
    Unproven,
    Verifier,
    _compile_ast_command,
    check_preprocessing,
    walk,
)

ROOTS = {
    "pgaccel_join_rows_workspace_bytes": ("pgaccel_join_rows_request",),
    "pgaccel_join_rows_controlled_workspace_bytes": (
        "pgaccel_join_rows_request",
        "pgaccel_join_control",
    ),
}
PROFILE = COMPILER_PROFILE


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def checked_flags(flags: tuple[str, ...]) -> tuple[str, ...]:
    """No plugins, PCH/modules, response files, extra inputs or action overrides.

    Reject unknown flags instead of silently dropping compiler behavior. Include
    and macro switches affect preprocessing; the other allowed switches are
    applied identically to AST checking and code generation.
    """
    result: list[str] = []
    pairs = {"-I", "-isystem", "-isysroot", "--sysroot", "-D", "-U", "-target", "-arch"}
    position = 0
    while position < len(flags):
        flag = flags[position]
        if flag in pairs:
            position += 1
            if (
                position == len(flags)
                or not flags[position]
                or flags[position].startswith("-")
            ):
                raise Unproven(f"missing metadata compiler argument for {flag}")
            result.extend((flag, flags[position]))
        elif flag in {
            "-std=c++17",
            "-std=gnu++17",
            "-fPIC",
            "-fpic",
            "-g",
            "-g0",
            "-ffast-math",
            "-fno-fast-math",
            "-fprofile-instr-generate",
            "-fcoverage-mapping",
            "-fno-omit-frame-pointer",
        } or re.fullmatch(
            r"-O[0-3szg]|-[IDU].+|--sysroot=.+|-mmacosx-version-min=[0-9.]+", flag
        ):
            result.append(flag)
        else:
            raise Unproven(f"unsupported metadata compiler flag: {flag}")
        position += 1
    return tuple(result)


def run(command: tuple[str, ...], source: str | None = None) -> str:
    try:
        process = subprocess.run(
            command,
            input=source,
            text=True,
            encoding="utf-8",
            capture_output=True,
            timeout=60,
            check=False,
        )
    except (OSError, UnicodeError, subprocess.TimeoutExpired) as error:
        raise Unproven(f"metadata build command failed: {error}") from error
    if process.returncode or len(process.stdout) > 32_000_000:
        raise Unproven(f"metadata build command failed: {process.stderr[:4000]}")
    return process.stdout


def inputs(
    compiler: pathlib.Path,
    source: pathlib.Path,
    flags: tuple[str, ...],
    roots: dict[str, tuple[str, ...]],
    dependencies: tuple[str, ...] = (),
) -> tuple[dict, str, tuple[str, ...]]:
    compiler = compiler.resolve(strict=True)
    source = source.resolve(strict=True)
    flags = checked_flags(flags)
    version = run((str(compiler), "--version"))
    if "clang" not in version.lower():
        raise Unproven("metadata build requires Clang")
    command = (str(compiler), "-std=c++17", *flags, *PROFILE)
    # Discover the include closure and its exact preprocessed semantics. Build
    # later freezes these files in a closed virtual filesystem so AST checking
    # and code generation retain real file identities (including coverage).
    preprocessed = run((*command, *dependencies, "-E", "-x", "c++", str(source)))
    check_preprocessing(preprocessed)
    binding = {
        "schema_version": 1,
        "compiler": str(compiler),
        "compiler_sha256": digest(compiler.read_bytes()),
        "compiler_version": version,
        "working_directory": str(pathlib.Path.cwd().resolve()),
        "flags": list(flags),
        "profile": list(PROFILE),
        "source": str(source),
        "source_sha256": digest(source.read_bytes()),
        "preprocessed_sha256": digest(preprocessed.encode("utf-8")),
        "headers": {
            str(path): digest(path.read_bytes())
            for path in included_files(preprocessed, source)
        },
        "roots": {name: list(types) for name, types in roots.items()},
        "driver_sha256": digest(pathlib.Path(__file__).read_bytes()),
        "verifier_sha256": digest(
            pathlib.Path(__file__).with_name("metadata_effects.py").read_bytes()
        ),
    }
    return binding, preprocessed, command


def included_files(preprocessed: str, source: pathlib.Path) -> list[pathlib.Path]:
    files = {source}
    for match in re.finditer(r'^# \d+ ("(?:[^"\\]|\\.)*")', preprocessed, re.M):
        name = json.loads(match[1])
        if not name.startswith("<"):
            files.add(pathlib.Path(name).absolute())
    return sorted(files)


def freeze_files(binding: dict, directory: pathlib.Path) -> pathlib.Path:
    """No fallthrough: absent/conditional includes cannot appear between phases."""
    roots = []
    for index, (name, expected) in enumerate(binding["headers"].items()):
        contents = pathlib.Path(name).read_bytes()
        if digest(contents) != expected:
            raise Unproven("metadata include changed while freezing the build")
        frozen = directory / f"input-{index}"
        frozen.write_bytes(contents)
        roots.append(
            {
                "type": "file",
                "name": name,
                "external-contents": str(frozen),
                "use-external-name": False,
            }
        )
    overlay = directory / "inputs.json"
    overlay.write_text(
        json.dumps(
            {
                "version": 0,
                "fallthrough": False,
                "roots": roots,
            }
        ),
        encoding="utf-8",
    )
    return overlay


def prove_unit(ast, roots: dict[str, tuple[str, ...]]) -> list[dict]:
    """Root effects alone cannot authorize load-time work or interposable helpers."""
    results = []
    for name, types in roots.items():
        verifier = Verifier(ast)
        result = verifier.prove(name, types)
        if not result.ok:
            raise Unproven(f"{name}: {result.detail}")
        for function in verifier.definitions.values():
            if (
                function.get("name") in result.functions
                and function.get("name") != name
            ):
                if function.get("storageClass") != "static":
                    raise Unproven("metadata helper must have internal static linkage")
        results.append({"name": name, **dataclasses.asdict(result)})

    def globals_only(node):
        kind = node.get("kind")
        if kind == "FileScopeAsmDecl":
            raise Unproven("metadata unit has top-level assembly")
        if kind in {
            "FunctionDecl",
            "CXXMethodDecl",
            "CXXConstructorDecl",
            "CXXDestructorDecl",
        }:
            # Function attributes can run code even when no approved root calls it.
            if any(
                child.get("kind") in {"ConstructorAttr", "DestructorAttr"}
                for child in node.get("inner", ())
            ):
                raise Unproven("metadata unit has load/unload functions")
            return
        if kind == "VarDecl" and node.get("init"):
            if not node.get("constexpr") or any(
                item.get("kind")
                not in {
                    "VarDecl",
                    "IntegerLiteral",
                    "ImplicitCastExpr",
                    "ConstantExpr",
                    "UnaryOperator",
                    "BinaryOperator",
                    "ParenExpr",
                    "CXXBoolLiteralExpr",
                }
                for item in walk(node)
            ):
                raise Unproven(
                    "metadata unit has unproven global initialization: "
                    f"{node.get('name')}"
                )
        for child in node.get("inner", ()):
            globals_only(child)

    globals_only(ast.tree)
    return results


def build(
    compiler: pathlib.Path,
    source: pathlib.Path,
    output: pathlib.Path,
    flags: tuple[str, ...] = (),
    roots: dict[str, tuple[str, ...]] = ROOTS,
    depfile: pathlib.Path | None = None,
) -> pathlib.Path:
    dependencies = (
        ("-MD", "-MF", str(depfile.resolve()), "-MQ", str(output.resolve()))
        if depfile
        else ()
    )
    binding, preprocessed, command = inputs(
        compiler, source, flags, roots, dependencies
    )
    output = output.resolve()
    receipt = output.with_name(output.name + ".metadata.json")
    # Temporary object in the same directory makes publication atomic. Failed
    # proofs/compiles preserve the previous build but cannot issue a new receipt.
    with tempfile.TemporaryDirectory(
        prefix="metadata-build-", dir=output.parent
    ) as temporary:
        directory = pathlib.Path(temporary)
        overlay = freeze_files(binding, directory)
        frozen_command = (*command, "-ivfsoverlay", str(overlay), "-x", "c++")
        frozen_input = run((*frozen_command, "-E", binding["source"]))
        if frozen_input != preprocessed:
            raise Unproven("closed metadata inputs changed preprocessing semantics")
        ast = _compile_ast_command(
            "",
            (
                *frozen_command,
                "-fsyntax-only",
                "-Xclang",
                "-ast-dump=json",
                binding["source"],
            ),
        )
        results = prove_unit(ast, roots)
        staged = directory / output.name
        run((*frozen_command, "-c", binding["source"], "-o", str(staged)))
        binding.update(
            {
                "object": str(output),
                "object_sha256": digest(staged.read_bytes()),
                "proofs": results,
            }
        )
        staged_receipt = pathlib.Path(temporary) / "receipt.json"
        staged_receipt.write_text(
            json.dumps(binding, indent=2) + "\n", encoding="utf-8"
        )
        staged.replace(output)
        staged_receipt.replace(receipt)
    return receipt


def verify_receipt(
    receipt: pathlib.Path,
    compiler: pathlib.Path,
    source: pathlib.Path,
    output: pathlib.Path,
    flags: tuple[str, ...] = (),
    roots: dict[str, tuple[str, ...]] = ROOTS,
) -> None:
    """Replay with caller-supplied expected inputs, never receipt-selected paths.

    The receipt is freshness evidence, not a signature or an exemption. A caller
    approving an export must also rerun the proof and establish actual linkage.
    """
    try:
        recorded = json.loads(receipt.read_text(encoding="utf-8"))
        expected, _, _ = inputs(compiler, source, flags, roots)
        expected.update(
            {
                "object": str(output.resolve()),
                "object_sha256": digest(output.read_bytes()),
            }
        )
        if not isinstance(recorded, dict) or any(
            recorded.get(key) != value for key, value in expected.items()
        ):
            raise Unproven("metadata build receipt is stale or mismatched")
    except (OSError, ValueError) as error:
        raise Unproven(f"cannot validate metadata build receipt: {error}") from error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", required=True, type=pathlib.Path)
    parser.add_argument("--source", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--flag", action="append", default=[])
    parser.add_argument("--verify", type=pathlib.Path)
    parser.add_argument("--depfile", type=pathlib.Path)
    args = parser.parse_args()
    try:
        if args.verify:
            verify_receipt(
                args.verify, args.compiler, args.source, args.output, tuple(args.flag)
            )
        else:
            build(
                args.compiler,
                args.source,
                args.output,
                tuple(args.flag),
                depfile=args.depfile,
            )
    except (OSError, Unproven) as error:
        parser.exit(1, f"metadata build failed: {error}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
