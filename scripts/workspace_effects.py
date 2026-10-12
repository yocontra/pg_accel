#!/usr/bin/env python3
"""Compiler-resolved workspace-address effects; never a GPU-work approval.

This checks function bodies, not call-site conversions, bounds, alignment,
linked-object identity or caller alias propagation. The release analyzer must
not consume these results until those independent obligations are satisfied.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import pathlib
import shutil
import subprocess
from typing import Any

Node = dict[str, Any]
INTEGERS = frozenset(
    {
        "bool",
        "char",
        "signed char",
        "unsigned char",
        "short",
        "unsigned short",
        "int",
        "unsigned int",
        "long",
        "unsigned long",
        "long long",
        "unsigned long long",
    }
)


class Unproven(ValueError):
    pass


@dataclasses.dataclass(frozen=True)
class Result:
    ok: bool
    detail: str
    definitions: int = 0
    base_parameter: int | None = None
    nullable: bool = False
    source_sha256: str = ""


@dataclasses.dataclass(frozen=True)
class _CompilerView:
    roots: tuple[Node, ...]
    source_sha256: str
    expanded_source: str
    profile: tuple[str, ...]


@dataclasses.dataclass(frozen=True)
class _Value:
    kind: str
    nullable: bool = False


def _type(node: Node) -> str:
    info = node.get("type", {})
    return info.get("desugaredQualType", info.get("qualType", ""))


def _integer(node: Node) -> bool:
    return _type(node).removeprefix("const ") in INTEGERS


def _pointer(node: Node) -> bool:
    spelling = _type(node).replace("(anonymous namespace)::", "")
    return spelling.endswith(" *") and not any(c in spelling for c in "()[]&")


def _walk(node: Node):
    yield node
    for child in node.get("inner", ()):
        yield from _walk(child)


def _run(command: tuple[str, ...], source: str, *, limit: int = 32_000_000) -> str:
    try:
        result = subprocess.run(
            command,
            input=source,
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
            timeout=60,
        )
    except (OSError, UnicodeError, subprocess.TimeoutExpired) as error:
        raise Unproven(f"workspace compiler failed: {error}") from error
    if result.returncode or len(result.stdout) > limit:
        raise Unproven("workspace translation unit exceeded compiler/size contract")
    return result.stdout


def compile_view(
    source: str,
    directory: pathlib.Path,
    includes: tuple[pathlib.Path, ...],
    *,
    name_filter: str,
    retain_functions: frozenset[str] | None = None,
) -> _CompilerView:
    """Parse the actual expanded TU, retaining compiler-resolved helper nodes.

    The second compilation consumes the first pass's text, not mutable headers.
    This is source evidence only, not an attestation of any native object.
    """
    compiler = shutil.which("clang++")
    if compiler is None or not name_filter or "\x00" in name_filter:
        raise Unproven("a compiler and nonempty AST filter are required")
    profile = (compiler, "-std=c++17", "-Werror=return-type", "-Werror=vla")
    search = tuple(arg for path in (directory, *includes) for arg in ("-I", str(path)))
    expanded = _run((*profile, *search, "-E", "-x", "c++", "-"), source)
    dumped = _run(
        (
            *profile,
            "-fsyntax-only",
            "-x",
            "c++-cpp-output",
            "-Xclang",
            "-ast-dump=json",
            "-Xclang",
            f"-ast-dump-filter={name_filter}",
            "-",
        ),
        expanded,
        # A common namespace filter keeps caller/callee declaration IDs in one
        # compiler pass. Drop unrelated top-level nodes immediately below.
        limit=128_000_000 if retain_functions is not None else 32_000_000,
    )
    # Filtered Clang dumps contain a sequence of JSON objects, not an array.
    decoder = json.JSONDecoder()
    roots: list[Node] = []
    cursor = 0
    try:
        while cursor < len(dumped):
            if dumped[cursor].isspace():
                cursor += 1
                continue
            root, cursor = decoder.raw_decode(dumped, cursor)
            if not isinstance(root, dict):
                raise ValueError("AST root is not an object")
            if retain_functions is None or root.get("name") in retain_functions:
                roots.append(root)
    except (ValueError, RecursionError) as error:
        raise Unproven("invalid workspace compiler AST") from error
    return _CompilerView(
        tuple(roots), hashlib.sha256(expanded.encode()).hexdigest(), expanded, profile
    )


def check_value_members(
    view: _CompilerView,
    members: set[tuple[str, str]],
    copy_types: set[str] | None = None,
) -> None:
    """Reject reference fields hidden by MemberExpr's expression type.

    decltype of an unparenthesized member access preserves its DECLARED type.
    Assertions use the same expanded TU and compiler profile, without reopening
    headers or trying to correlate declaration IDs across compiler invocations.
    Unnameable/private/ambiguous owner types fail conservatively at compilation.
    Optional same-type metadata copies must have a trivial const-reference
    constructor and destructor; an AST constructor node is not enough evidence.
    """
    if not isinstance(view, _CompilerView):
        raise Unproven("compiler-owned evidence is required")
    if not members and not copy_types:
        return
    assertions = "\n".join(
        "static_assert(!__is_reference(decltype((("
        + owner.replace("(anonymous namespace)::", "")
        + "*)0)->"
        + member
        + ')), "workspace reference field");'
        for owner, member in sorted(members)
    )
    # The AST's CXXConstructExpr alone does not prove that a same-type copy is
    # inert: a user-defined copy constructor/destructor could do host row work.
    for spelling in sorted(copy_types or ()):
        name = spelling.replace("(anonymous namespace)::", "")
        assertions += (
            f"\nstatic_assert(__is_trivially_constructible({name}, const {name}&)"
            f" && __is_trivially_destructible({name}),"
            ' "workspace metadata copy has lifecycle effects");'
        )
    _run(
        (*view.profile, "-fsyntax-only", "-x", "c++-cpp-output", "-"),
        view.expanded_source + "\n\n" + assertions + "\n",
    )


class Verifier:
    def __init__(self, view: _CompilerView):
        if not isinstance(view, _CompilerView):
            raise Unproven("compiler-owned evidence is required")
        self.view = view

    def _expression(self, node: Node, base: str, offset: str) -> _Value:
        kind = node.get("kind")
        children = node.get("inner", ())
        if kind == "ParenExpr" and len(children) == 1:
            return self._expression(children[0], base, offset)
        if kind == "DeclRefExpr":
            reference = node.get("referencedDecl", {})
            if reference.get("id") == base and _type(node) == "void *":
                return _Value("pointer")
            if reference.get("id") == offset and _integer(node):
                return _Value("integer")
            if (
                reference.get("kind") == "VarDecl"
                and _integer(node)
                and _type(node).startswith("const ")
                and node.get("nonOdrUseReason") == "constant"
            ):
                # Clang marks this as a constant use, not a storage access.
                # A merely const global/TLS value with dynamic initialization
                # has no such evidence and is rejected.
                return _Value("integer")
        if kind in {"IntegerLiteral", "CXXBoolLiteralExpr"} and _integer(node):
            return _Value("integer")
        if kind == "CXXNullPtrLiteralExpr":
            return _Value("null", True)
        if kind in {"ImplicitCastExpr", "CXXStaticCastExpr", "CXXReinterpretCastExpr"}:
            if len(children) != 1:
                raise Unproven("cast contains unmodeled effects")
            value = self._expression(children[0], base, offset)
            cast = node.get("castKind")
            if (
                value.kind == "pointer"
                and _pointer(node)
                and cast in {"LValueToRValue", "NoOp", "BitCast", "Dependent"}
            ):
                return value
            if (
                value.kind == "integer"
                and _integer(node)
                and cast
                in {"LValueToRValue", "NoOp", "IntegralCast", "IntegralToBoolean"}
            ):
                return value
            if value.kind == "null" and _pointer(node) and cast == "NullToPointer":
                return value
        if kind == "BinaryOperator" and len(children) == 2:
            left, right = [self._expression(child, base, offset) for child in children]
            operator = node.get("opcode")
            if (
                operator == "+"
                and left.kind == "pointer"
                and not left.nullable
                and right.kind == "integer"
                and _pointer(node)
            ):
                return left
            if (
                operator in {"==", "!=", "<", "<=", ">", ">="}
                and left.kind == right.kind == "integer"
                and _integer(node)
            ):
                return _Value("integer")
        if kind == "UnaryOperator" and len(children) == 1:
            value = self._expression(children[0], base, offset)
            if (
                node.get("opcode") in {"-", "+", "~", "!"}
                and value.kind == "integer"
                and _integer(node)
            ):
                return value
        if kind == "ConditionalOperator" and len(children) == 3:
            condition, yes, no = [
                self._expression(child, base, offset) for child in children
            ]
            if (
                condition.kind == "integer"
                and yes.kind in {"pointer", "null"}
                and no.kind in {"pointer", "null"}
                and "pointer" in {yes.kind, no.kind}
            ):
                return _Value("pointer", yes.nullable or no.nullable)
        raise Unproven(f"unmodeled workspace address expression: {kind}")

    def _function(self, node: Node) -> bool:
        children = node.get("inner", ())
        return_type = (
            _type(node).replace("(anonymous namespace)::", "").split("(", 1)[0].strip()
        )
        if not _pointer({"type": {"qualType": return_type}}):
            raise Unproven("declared return type is not an explicit object pointer")
        if node.get("variadic") or any(
            child.get("kind") not in {"ParmVarDecl", "CompoundStmt", "TemplateArgument"}
            for child in children
        ):
            raise Unproven("function has attributes or unmodeled declarations")
        parameters = [child for child in children if child.get("kind") == "ParmVarDecl"]
        bodies = [child for child in children if child.get("kind") == "CompoundStmt"]
        if (
            len(parameters) != 2
            or len(bodies) != 1
            or _type(parameters[0]) != "void *"
            or not _integer(parameters[1])
            or any(parameter.get("inner") for parameter in parameters)
        ):
            raise Unproven("expected only a void pointer and builtin integral offset")
        body = bodies[0].get("inner", ())
        if len(body) != 1 or body[0].get("kind") != "ReturnStmt":
            raise Unproven("workspace helper must contain exactly one return")
        returned = body[0].get("inner", ())
        if len(returned) != 1:
            raise Unproven("workspace helper has no exact return value")
        value = self._expression(returned[0], parameters[0]["id"], parameters[1]["id"])
        if value.kind != "pointer":
            raise Unproven("returned value is not derived from the base pointer")
        return value.nullable

    def prove(self, name: str) -> Result:
        definitions = {
            node["id"]: node
            for root in self.view.roots
            for node in _walk(root)
            if node.get("kind") == "FunctionDecl"
            and node.get("name") == name
            and any(
                child.get("kind") == "CompoundStmt" for child in node.get("inner", ())
            )
        }
        try:
            if not definitions:
                raise Unproven("no matching compiler-resolved helper definitions")
            nullable = False
            for node in definitions.values():
                nullable |= self._function(node)
            return Result(
                True,
                "pure base-derived address return; no device evidence",
                len(definitions),
                0,
                nullable,
                self.view.source_sha256,
            )
        except Unproven as error:
            return Result(False, str(error), source_sha256=self.view.source_sha256)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=pathlib.Path, required=True)
    parser.add_argument("--helper", required=True)
    parser.add_argument("--filter", required=True, help="Clang qualified-name filter")
    parser.add_argument("--include", type=pathlib.Path, action="append", default=[])
    args = parser.parse_args()
    try:
        view = compile_view(
            args.source.read_text(encoding="utf-8"),
            args.source.resolve().parent,
            tuple(args.include),
            name_filter=args.filter,
        )
        result = Verifier(view).prove(args.helper)
    except (OSError, UnicodeError, Unproven) as error:
        result = Result(False, str(error))
    print(json.dumps(dataclasses.asdict(result), indent=2, sort_keys=True))
    return 0 if result.ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
