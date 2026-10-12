#!/usr/bin/env python3
"""Typed, fail-closed metadata-effect proof over Clang's actual translation unit.

This verifier is deliberately separate from the GPU-dispatch proof. It proves
bounded metadata effects, not arithmetic correctness or device execution. Row
pointer fields are opaque even when their pointee type looks like metadata.
Unknown AST forms, indirect calls, pointer reconstruction, user-defined object
lifecycle, unbounded loops and writes outside local/output metadata fail closed.
No result from this module is currently an exemption in cpu_cheat_audit.py.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
from collections.abc import Iterator
from typing import Any

Node = dict[str, Any]
COMPILER_PROFILE = ("-Werror=return-type", "-Werror=vla", "-Wsystem-headers")


class Unproven(ValueError):
    pass


@dataclasses.dataclass(frozen=True)
class _CompilerAst:
    """Canonical compiler evidence, not a build/dependency attestation."""

    tree: Node
    command: tuple[str, ...]
    source_sha256: str


@dataclasses.dataclass(frozen=True)
class Place:
    root: str
    type: str
    writable: bool
    extent: int = 1


@dataclasses.dataclass(frozen=True)
class Value:
    kind: str = "scalar"
    target: Place | None = None
    constant: int | None = None
    upper: int | None = None
    declaration: str | None = None


@dataclasses.dataclass(frozen=True)
class Result:
    ok: bool
    detail: str
    functions: tuple[str, ...] = ()
    loop_bounds: tuple[int, ...] = ()


def walk(node: Node) -> Iterator[Node]:
    yield node
    for child in node.get("inner", ()):
        if isinstance(child, dict):
            yield from walk(child)


def spelling(node: Node) -> str:
    type_info = node.get("type", {})
    return type_info.get("desugaredQualType", type_info.get("qualType", ""))


def strip(node: Node) -> Node:
    while node.get("kind") in {"ParenExpr", "ImplicitCastExpr"}:
        children = node.get("inner", ())
        if len(children) != 1:
            break
        node = children[0]
    return node


def declaration(node: Node) -> str | None:
    node = strip(node)
    if node.get("kind") == "DeclRefExpr":
        return node.get("referencedDecl", {}).get("id")
    return None


class Verifier:
    MAX_LOOP = 64
    MAX_WORK = 100_000
    MAX_RECORD_CELLS = 256

    def __init__(self, ast: _CompilerAst):
        # Raw JSON can omit evaluated VLA parameter bounds. Accept only the
        # driver profile which rejects that syntax before dumping the AST.
        if not isinstance(ast, _CompilerAst):
            raise Unproven("use the canonical metadata compiler profile")
        self.nodes: dict[str, Node] = {}
        self.records: dict[str, Node] = {}
        self.aliases: dict[str, str] = {}
        self.field_owners: dict[str, str] = {}
        self.definitions: dict[str, Node] = {}
        self.c_linkage: set[str] = set()
        self.record_checked: set[str] = set()
        self.stack: list[str] = []
        self.visited: set[str] = set()
        self.bounds: list[int] = []
        self.frozen: dict[str, tuple[int, int]] = {}
        self.work = 0
        self.multiplier = 1
        self.serial = 0
        self.root_function = ""
        self.published = False
        self.input_read = False
        self.started = False
        short_records: dict[str, set[str]] = {}

        def collect(
            node: Node, namespace: tuple[str, ...] = (), c_linkage: bool = False
        ) -> None:
            kind = node.get("kind")
            identity = node.get("id")
            if identity:
                self.nodes[identity] = node
            name = node.get("name", "")
            if kind == "NamespaceDecl":
                namespace = (*namespace, name)
            if kind == "LinkageSpecDecl":
                c_linkage = node.get("language") == "C"
            if kind == "FunctionDecl" and c_linkage:
                self.c_linkage.add(identity)
            if kind == "CXXRecordDecl" and node.get("completeDefinition"):
                self.records[identity] = node
                if name:
                    self.aliases["::".join((*namespace, name))] = identity
                    short_records.setdefault(name, set()).add(identity)
                for child in node.get("inner", ()):
                    if child.get("kind") == "FieldDecl":
                        self.field_owners[child["id"]] = identity
            if kind == "TypedefDecl":
                targets = [
                    item["decl"]["id"]
                    for item in walk(node)
                    if item.get("kind") == "RecordType" and "decl" in item
                ]
                if targets:
                    self.aliases[name] = targets[0]
                elif any(item.get("kind") == "EnumType" for item in walk(node)):
                    self.aliases[name] = "int"
                else:
                    builtin = [
                        item.get("type", {}).get("qualType", "")
                        for item in walk(node)
                        if item.get("kind") == "BuiltinType"
                    ]
                    if builtin:
                        self.aliases[name] = builtin[0]
            if kind == "EnumDecl" and name:
                self.aliases["::".join((*namespace, name))] = "int"
            if kind == "FunctionDecl" and any(
                item.get("kind") == "CompoundStmt" for item in node.get("inner", ())
            ):
                self.definitions[identity] = node
            for child in node.get("inner", ()):
                if isinstance(child, dict):
                    collect(child, namespace, c_linkage)

        collect(ast.tree)
        for name, identities in short_records.items():
            if len(identities) == 1 and name not in self.aliases:
                self.aliases[name] = next(iter(identities))
            elif len(identities) > 1:
                self.aliases.pop(name, None)
        # Calls can refer to a prior declaration, not the definition's own ID.
        for function in tuple(self.definitions.values()):
            previous = function.get("previousDecl")
            while previous:
                self.definitions[previous] = function
                if previous in self.c_linkage:
                    self.c_linkage.add(function["id"])
                previous = self.nodes.get(previous, {}).get("previousDecl")

    def fail(self, node: Node, detail: str) -> None:
        line = node.get("range", {}).get("begin", {}).get("line", "?")
        raise Unproven(f"{detail} ({node.get('kind', '?')}, line {line})")

    def callable(self, node: Node) -> None:
        seen: set[str] = set()
        while node.get("id") not in seen:
            seen.add(node.get("id", ""))
            for child in node.get("inner", ()):
                kind = child.get("kind", "")
                if kind.endswith("Attr") and kind not in {
                    "BuiltinAttr",
                    "NoThrowAttr",
                    "PureAttr",
                    "ConstAttr",
                }:
                    self.fail(child, "unproven callable attribute/linkage")
            previous = node.get("previousDecl")
            if not previous or previous not in self.nodes:
                break
            node = self.nodes[previous]

    def type(self, value: str) -> str:
        if "volatile" in value:
            raise Unproven("volatile metadata is not supported")
        value = re.sub(r"\b(const|struct|enum)\b", "", value).strip()
        value = re.sub(r"\s+", " ", value)
        seen: set[str] = set()
        while value in self.aliases and value not in seen:
            seen.add(value)
            value = self.aliases[value]
        return value

    def scalar(self, value: str) -> bool:
        value = self.type(value)
        return value in {
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
            "__int128",
            "unsigned __int128",
        }

    def array(self, value: str) -> tuple[str, int] | None:
        match = re.fullmatch(r"(.+)\[(\d+)\]", self.type(value))
        return (self.type(match[1].strip()), int(match[2])) if match else None

    def record(self, type_name: str) -> Node:
        identity = self.type(type_name)
        node = self.records.get(identity)
        if node is None:
            raise Unproven(f"unknown metadata record {type_name}")
        if identity not in self.record_checked:
            definition = node.get("definitionData", {})
            if (
                not definition.get("isAggregate")
                or not definition.get("isTriviallyCopyable")
                or node.get("tagUsed") == "union"
                or node.get("bases")
            ):
                self.fail(node, "non-plain metadata record")
            self.record_checked.add(identity)
            for child in node.get("inner", ()):
                kind = child.get("kind")
                if kind and kind.endswith("Attr"):
                    self.fail(child, "unproven metadata record layout attribute")
                if kind in {
                    "CXXConstructorDecl",
                    "CXXDestructorDecl",
                    "CXXMethodDecl",
                    "CXXConversionDecl",
                } and not child.get("isImplicit"):
                    self.fail(child, "user-defined metadata object lifecycle/operator")
                if kind != "FieldDecl":
                    continue
                field_type = self.type(spelling(child))
                if "*" not in field_type:
                    self.data_type(field_type)
                for initializer in child.get("inner", ()):
                    allowed = {
                        "IntegerLiteral",
                        "ImplicitCastExpr",
                        "CXXNullPtrLiteralExpr",
                        "InitListExpr",
                        "ImplicitValueInitExpr",
                        "CXXBoolLiteralExpr",
                        "ConstantExpr",
                    }
                    if any(
                        item.get("kind") not in allowed for item in walk(initializer)
                    ):
                        self.fail(initializer, "unproven default member initializer")
            if self.cells(identity) > self.MAX_RECORD_CELLS:
                self.fail(node, "metadata record exceeds the fixed cell budget")
        return node

    def cells(self, type_name: str, depth: int = 0) -> int:
        if depth > 8:
            raise Unproven("metadata record nesting exceeds the fixed bound")
        type_name = self.type(type_name)
        if self.scalar(type_name) or "*" in type_name:
            return 1
        array = self.array(type_name)
        if array:
            return array[1] * self.cells(array[0], depth + 1)
        record = self.records.get(type_name)
        if record is None:
            raise Unproven("unknown metadata storage extent")
        return sum(
            self.cells(spelling(field), depth + 1)
            for field in record.get("inner", ())
            if field.get("kind") == "FieldDecl"
        )

    def data_type(self, type_name: str) -> None:
        if self.scalar(type_name):
            return
        array = self.array(type_name)
        if array:
            if not 0 < array[1] <= self.MAX_LOOP:
                raise Unproven("metadata array exceeds the fixed bound")
            self.data_type(array[0])
            return
        self.record(type_name)

    def constant(self, node: Node, stack: tuple[str, ...] = ()) -> int | None:
        node = strip(node)
        if (
            node.get("kind") in {"IntegerLiteral", "ConstantExpr", "CXXBoolLiteralExpr"}
            and "value" in node
        ):
            return int(node["value"])
        identity = declaration(node)
        target = self.nodes.get(identity or "", {})
        if (
            identity
            and identity not in stack
            and (target.get("constexpr") or target.get("kind") == "EnumConstantDecl")
        ):
            children = target.get("inner", ())
            if len(children) == 1:
                return self.constant(children[0], (*stack, identity))
        return None

    def read(self, value: Value | Place) -> Value | Place:
        if not isinstance(value, Place):
            return value
        if value.root == "output":
            raise Unproven("metadata output is write-only")
        if value.root.startswith("input"):
            self.input_read = True
        if "*" in value.type:
            # Crucially, a pointer loaded from a record never acquires metadata
            # authority from its spelling. It could point directly at row data.
            return Value("pointer")
        if self.scalar(value.type):
            bounds = self.frozen.get(value.root)
            return Value(constant=bounds[0], upper=bounds[1]) if bounds else Value()
        self.data_type(value.type)
        return value

    def write(self, place: Value | Place, value: Value | Place, node: Node) -> None:
        if not isinstance(place, Place) or not place.writable or "*" in place.type:
            self.fail(node, "write outside mutable scalar/record metadata")
        if place.root in self.frozen:
            self.fail(node, "loop induction variable modified or escaped")
        if place.root == "output":
            if self.stack[-1] != self.root_function:
                self.fail(node, "helper publication lacks an export-path proof")
            self.published = True
        self.data_type(place.type)
        source = self.read(value)
        if self.scalar(place.type) != (
            isinstance(source, Value) and source.kind == "scalar"
        ):
            self.fail(node, "incompatible metadata assignment")
        if isinstance(source, Place) and self.type(place.type) != self.type(
            source.type
        ):
            self.fail(node, "cross-type aggregate assignment")

    def temporary(self, node: Node, type_name: str) -> Place:
        self.data_type(type_name)
        self.serial += 1
        return Place(f"temporary:{self.serial}", self.type(type_name), True)

    def expr(self, node: Node, env: dict[str, Value | Place]) -> Value | Place:
        self.work += self.multiplier
        if self.work > self.MAX_WORK:
            self.fail(node, "metadata work budget exceeded")
        kind = node.get("kind")
        children = node.get("inner", ())
        if (
            kind in {"ParenExpr", "ExprWithCleanups", "MaterializeTemporaryExpr"}
            and len(children) == 1
        ):
            return self.expr(children[0], env)
        if kind in {"IntegerLiteral", "CXXBoolLiteralExpr", "ConstantExpr"}:
            if kind == "ConstantExpr" and children:
                self.expr(children[0], env)
            return Value(constant=self.constant(node), upper=self.constant(node))
        if kind == "CXXNullPtrLiteralExpr":
            return Value("pointer")
        if kind == "DeclRefExpr":
            identity = node.get("referencedDecl", {}).get("id", "")
            if identity in env:
                return env[identity]
            target = self.nodes.get(identity, node.get("referencedDecl", {}))
            if target.get("kind") in {"FunctionDecl", "CXXMethodDecl"}:
                return Value("function", declaration=identity)
            constant = self.constant(node)
            if constant is not None:
                return Value(constant=constant, upper=constant)
            self.fail(node, "unproven global/nonlocal read")
        if kind == "ImplicitCastExpr" and len(children) == 1:
            source = self.expr(children[0], env)
            cast = node.get("castKind")
            if cast == "LValueToRValue":
                return self.read(source)
            if cast in {"NoOp", "FunctionToPointerDecay", "BuiltinFnToFnPtr"}:
                return source
            if cast == "ArrayToPointerDecay" and isinstance(source, Place):
                array = self.array(source.type)
                if array:
                    return Value(
                        "pointer",
                        Place(source.root, array[0], source.writable, array[1]),
                    )
            if cast == "NullToPointer":
                return Value("pointer")
            if cast == "BitCast" and self.type(spelling(node)) == "void *":
                pointer = self.read(source)
                if isinstance(pointer, Value) and pointer.kind == "pointer":
                    return pointer
            source = self.read(source)
            if cast in {
                "IntegralCast",
                "IntegralToBoolean",
                "PointerToBoolean",
            } and isinstance(source, Value):
                return Value()  # Casts cannot preserve a range across narrowing.
            self.fail(node, f"unproven implicit cast {cast}")
        if (
            kind in {"CXXStaticCastExpr", "CXXReinterpretCastExpr"}
            and len(children) == 1
        ):
            source = self.read(self.expr(children[0], env))
            if self.scalar(spelling(node)) and isinstance(source, Value):
                if source.kind == "scalar" or (
                    source.kind == "pointer"
                    and node.get("castKind") == "PointerToIntegral"
                ):
                    return Value()
            self.fail(node, "pointer reconstruction or non-scalar cast")
        if kind == "MemberExpr" and len(children) == 1:
            base = self.expr(children[0], env)
            if node.get("isArrow"):
                base = self.read(base)
                base = (
                    base.target
                    if isinstance(base, Value) and base.kind == "pointer"
                    else None
                )
            if not isinstance(base, Place):
                self.fail(node, "member access through opaque row pointer")
            self.record(base.type)
            field = node.get("referencedMemberDecl", "")
            if self.field_owners.get(field) != self.type(base.type):
                self.fail(node, "field does not belong to the proven metadata object")
            return Place(base.root, self.type(spelling(node)), base.writable)
        if kind == "ArraySubscriptExpr" and len(children) == 2:
            base = self.read(self.expr(children[0], env))
            index = self.read(self.expr(children[1], env))
            if (
                not isinstance(base, Value)
                or base.kind != "pointer"
                or base.target is None
            ):
                self.fail(node, "indexing opaque row storage")
            if (
                not isinstance(index, Value)
                or index.constant is None
                or index.upper is None
                or not 0 <= index.constant <= index.upper < base.target.extent
            ):
                self.fail(node, "metadata array index lacks a fixed in-bounds range")
            return dataclasses.replace(base.target, extent=1)
        if kind == "UnaryOperator" and len(children) == 1:
            op = node.get("opcode")
            value = self.expr(children[0], env)
            if op == "&" and isinstance(value, Place):
                return Value("pointer", value)
            if op == "*":
                pointer = self.read(value)
                if (
                    isinstance(pointer, Value)
                    and pointer.kind == "pointer"
                    and pointer.target is not None
                ):
                    return pointer.target
                self.fail(node, "dereferencing opaque row storage")
            source = self.read(value)
            if op in {"++", "--"}:
                self.write(value, Value(), node)
                return Value()
            if (
                op in {"!", "~", "+", "-"}
                and isinstance(source, Value)
                and (source.kind == "scalar" or op == "!")
            ):
                return Value()
            self.fail(node, "unsupported unary metadata operation")
        if kind in {"BinaryOperator", "CompoundAssignOperator"} and len(children) == 2:
            left = self.expr(children[0], env)
            right = self.expr(children[1], env)
            op = node.get("opcode", "")
            if op == "=":
                self.write(left, right, node)
                return left
            a, b = self.read(left), self.read(right)
            if not isinstance(a, Value) or not isinstance(b, Value):
                self.fail(node, "non-scalar builtin operation")
            if op in {"==", "!=", "<", "<=", ">", ">=", "&&", "||"}:
                return Value()
            if a.kind != "scalar" or b.kind != "scalar":
                self.fail(node, "pointer arithmetic/escape")
            if kind == "CompoundAssignOperator":
                self.write(left, Value(), node)
            elif op not in {"+", "-", "*", "/", "%", "&", "|", "^", "<<", ">>"}:
                self.fail(node, "unsupported scalar operation")
            return Value()
        if kind == "ConditionalOperator" and len(children) == 3:
            values = [self.read(self.expr(child, env)) for child in children]
            if all(
                isinstance(value, Value) and value.kind == "scalar" for value in values
            ):
                return Value()
            self.fail(node, "non-scalar conditional alias")
        if kind == "UnaryExprOrTypeTraitExpr" and node.get("name") in {
            "sizeof",
            "alignof",
        }:
            measured = node.get("argType", {}).get("qualType") or (
                spelling(children[0]) if len(children) == 1 else ""
            )
            # Do not skip a type merely because its spelling contains '*': a
            # variable-length array bound can contain multiplication/calls,
            # and sizeof evaluates those bounds. Only fixed data types pass.
            self.data_type(measured)
            return Value()
        if kind in {"CXXConstructExpr", "InitListExpr"}:
            result = self.temporary(node, spelling(node))
            for child in children:
                self.read(self.expr(child, env))
            return result
        if kind in {"ImplicitValueInitExpr", "CXXDefaultInitExpr"}:
            type_name = self.type(spelling(node))
            if "*" in type_name:
                return Value("pointer")
            if self.scalar(type_name):
                return Value(constant=0, upper=0)
            return self.temporary(node, type_name)
        if kind in {"CallExpr", "CXXOperatorCallExpr"} and children:
            callee = self.expr(children[0], env)
            if not isinstance(callee, Value) or callee.kind != "function":
                self.fail(node, "indirect/callback call")
            target = self.nodes.get(callee.declaration or "", {})
            self.callable(target)
            arguments = [self.expr(child, env) for child in children[1:]]
            if kind == "CXXOperatorCallExpr":
                if (
                    target.get("name") != "operator="
                    or not target.get("isImplicit")
                    or len(arguments) != 2
                ):
                    self.fail(node, "unproven overloaded operation")
                self.write(arguments[0], arguments[1], node)
                return arguments[0]
            definition = self.definitions.get(callee.declaration or "")
            if definition is not None:
                return self.call(definition, arguments)
            if target.get("name") == "memcmp" and any(
                item.get("kind") == "BuiltinAttr" for item in target.get("inner", ())
            ):
                self.memcmp(node, arguments, children[3] if len(children) == 4 else {})
                return Value()
            self.fail(node, f"unknown call {target.get('name', '?')}")
        self.fail(node, "unsupported metadata expression")

    def memcmp(
        self, node: Node, arguments: list[Value | Place], size_node: Node
    ) -> None:
        if len(arguments) != 3:
            self.fail(node, "unexpected memcmp arity")
        size_node = strip(size_node)
        if (
            size_node.get("kind") != "UnaryExprOrTypeTraitExpr"
            or size_node.get("name") != "sizeof"
        ):
            self.fail(node, "memcmp size is not one exact metadata object")
        children = size_node.get("inner", ())
        measured = size_node.get("argType", {}).get("qualType") or (
            spelling(children[0]) if len(children) == 1 else ""
        )
        self.data_type(measured)
        for argument in arguments[:2]:
            value = self.read(argument)
            if (
                not isinstance(value, Value)
                or value.kind != "pointer"
                or value.target is None
                or self.type(value.target.type) != self.type(measured)
            ):
                self.fail(node, "memcmp reads outside a fixed metadata object")
            self.read(value.target)

    def stmt(
        self,
        node: Node,
        env: dict[str, Value | Place],
        failure_guards: frozenset[str] = frozenset(),
    ) -> None:
        kind = node.get("kind")
        children = node.get("inner", ())
        if kind == "CompoundStmt":
            for child in children:
                self.stmt(child, env, failure_guards)
        elif kind == "DeclStmt":
            for child in children:
                if (
                    child.get("kind") != "VarDecl"
                    or child.get("storageClass")
                    or child.get("tls")
                ):
                    self.fail(child, "nonlocal or unsupported declaration")
                type_name = self.type(spelling(child))
                initializer = child.get("inner", ())
                if "*" in type_name:
                    self.fail(child, "mutable pointer aliases are not modeled")
                if type_name.endswith("&"):
                    if len(initializer) != 1:
                        self.fail(child, "reference without an exact initializer")
                    alias = self.expr(initializer[0], env)
                    if not isinstance(alias, Place):
                        self.fail(child, "reference outside known metadata")
                    env[child["id"]] = alias
                else:
                    self.data_type(type_name)
                    for item in initializer:
                        self.read(self.expr(item, env))
                    env[child["id"]] = Place(child["id"], type_name, True)
        elif kind == "IfStmt" and len(children) in {2, 3} and not node.get("hasInit"):
            self.expr(children[0], env)
            before = self.published
            condition = strip(children[0])
            extra: frozenset[str] = frozenset()
            guarded_return = children[1]
            if (
                guarded_return.get("kind") == "CompoundStmt"
                and len(guarded_return.get("inner", ())) == 1
            ):
                guarded_return = guarded_return["inner"][0]
            if (
                guarded_return.get("kind") == "ReturnStmt"
                and condition.get("kind") == "BinaryOperator"
                and condition.get("opcode") == "!="
            ):
                a, b = condition["inner"]
                if self.constant(b) == 0 and declaration(a):
                    extra = frozenset({declaration(a)})
                elif self.constant(a) == 0 and declaration(b):
                    extra = frozenset({declaration(b)})
            self.stmt(children[1], dict(env), failure_guards | extra)
            then_published = self.published
            self.published = before
            if len(children) == 3:
                self.stmt(children[2], dict(env), failure_guards)
            self.published = self.published and then_published
        elif kind == "ForStmt" and len(children) == 5:
            init, condition_variable, condition, increment, body = children
            if (
                condition_variable
                or init.get("kind") != "DeclStmt"
                or len(init.get("inner", ())) != 1
            ):
                self.fail(node, "unsupported loop initialization")
            variable = init["inner"][0]
            initializers = variable.get("inner", ())
            identity = variable.get("id", "")
            bound_test = condition
            if bound_test.get("opcode") == "&&":
                bound_test = bound_test["inner"][0]
            parts = bound_test.get("inner", ())
            limit = self.constant(parts[1]) if len(parts) == 2 else None
            if (
                len(initializers) != 1
                or self.constant(initializers[0]) != 0
                or bound_test.get("opcode") != "<"
                or declaration(parts[0]) != identity
                or limit is None
                or not 0 < limit <= self.MAX_LOOP
                or increment.get("kind") != "UnaryOperator"
                or increment.get("opcode") != "++"
                or len(increment.get("inner", ())) != 1
                or declaration(increment["inner"][0]) != identity
            ):
                self.fail(node, "loop has no fixed 0..N induction proof")
            self.stmt(init, env, failure_guards)
            self.bounds.append(limit)
            self.frozen[identity] = (0, limit - 1)
            self.multiplier *= limit
            before = self.published
            try:
                self.expr(condition, env)
                self.stmt(body, dict(env), failure_guards)
            finally:
                self.multiplier //= limit
                self.frozen.pop(identity)
                self.published = before
        elif kind == "ReturnStmt" and len(children) == 1:
            value = self.read(self.expr(children[0], env))
            if not isinstance(value, Value) or value.kind != "scalar":
                self.fail(node, "non-scalar helper return/escape")
            if self.stack[-1] == self.root_function and not self.published:
                constant = self.constant(children[0])
                if (constant is None or constant == 0) and declaration(
                    children[0]
                ) not in failure_guards:
                    self.fail(
                        node, "possible successful return without metadata publication"
                    )
        elif kind in {"BreakStmt", "NullStmt"}:
            if kind == "BreakStmt" and not self.frozen:
                self.fail(node, "break outside proven bounded loop")
        else:
            self.expr(node, env)

    def call(self, function: Node, arguments: list[Value | Place]) -> Value:
        identity = function["id"]
        self.callable(function)
        if identity in self.stack or len(self.stack) >= 16:
            self.fail(function, "recursive or excessively deep metadata call graph")
        return_type = spelling(function).split("(", 1)[0].strip()
        if not self.scalar(return_type):
            self.fail(function, "helper return type is not a builtin integer/status")
        parameters = [
            item
            for item in function.get("inner", ())
            if item.get("kind") == "ParmVarDecl"
        ]
        bodies = [
            item
            for item in function.get("inner", ())
            if item.get("kind") == "CompoundStmt"
        ]
        if len(parameters) != len(arguments) or len(bodies) != 1:
            self.fail(function, "unresolved parameter/body mapping")
        env: dict[str, Value | Place] = {}
        for parameter, argument in zip(parameters, arguments):
            type_name = self.type(spelling(parameter))
            if "*" in type_name:
                pointer = self.read(argument)
                if not isinstance(pointer, Value) or pointer.kind != "pointer":
                    self.fail(parameter, "pointer argument has no provenance")
                env[parameter["id"]] = pointer
            elif type_name.endswith("&"):
                if not isinstance(argument, Place):
                    self.fail(parameter, "reference argument is not metadata")
                env[parameter["id"]] = argument
            else:
                self.data_type(type_name)
                self.read(argument)
                env[parameter["id"]] = Place(parameter["id"], type_name, True)
        self.stack.append(identity)
        self.visited.add(function.get("name", "?"))
        try:
            self.stmt(bodies[0], env)
        finally:
            self.stack.pop()
        return Value()

    def prove(self, name: str, inputs: tuple[str, ...]) -> Result:
        if self.started:
            return Result(False, "a verifier instance proves exactly one export")
        self.started = True
        try:
            candidates = {
                item["id"]: item
                for item in self.definitions.values()
                if item.get("name") == name
            }
            if len(candidates) != 1:
                raise Unproven("metadata export does not have one definition")
            function = next(iter(candidates.values()))
            if function["id"] not in self.c_linkage or function.get("variadic"):
                raise Unproven("metadata definition is not the exact C ABI symbol")
            if not spelling(function).startswith("pgaccel_status ("):
                raise Unproven("metadata export must return pgaccel_status")
            parameters = [
                item
                for item in function.get("inner", ())
                if item.get("kind") == "ParmVarDecl"
            ]
            if len(parameters) != len(inputs) + 1:
                raise Unproven("metadata export parameter count differs from contract")
            arguments: list[Value | Place] = []
            for index, (parameter, input_type) in enumerate(zip(parameters, inputs)):
                if spelling(parameter) != f"const {input_type} *":
                    raise Unproven("metadata input type differs from contract")
                self.record(input_type)
                arguments.append(
                    Value(
                        "pointer", Place(f"input:{index}", self.type(input_type), False)
                    )
                )
            if parameters[-1].get("type", {}).get("qualType") != "uint64_t *":
                raise Unproven("metadata output must be one uint64_t")
            arguments.append(
                Value("pointer", Place("output", self.type("uint64_t"), True))
            )
            self.root_function = function["id"]
            self.call(function, arguments)
            if not self.published or not self.input_read:
                raise Unproven("metadata input observation/publication was not proven")
            return Result(
                True,
                "bounded typed metadata effects",
                tuple(sorted(self.visited)),
                tuple(self.bounds),
            )
        except Unproven as error:
            return Result(
                False, str(error), tuple(sorted(self.visited)), tuple(self.bounds)
            )


def compile_ast(
    source: str, directory: pathlib.Path, include: pathlib.Path
) -> _CompilerAst:
    compiler = shutil.which("clang++")
    if compiler is None:
        raise Unproven("clang++ is required for typed metadata proof")
    profile = (
        compiler,
        "-std=c++17",
        *COMPILER_PROFILE,
        "-I",
        str(directory),
        "-I",
        str(include),
    )
    # Diagnostic pragmas in a transitive header (or produced by _Pragma) can
    # override -Werror. Inspect the compiler-expanded directives, not raw source.
    try:
        process = subprocess.run(
            (*profile, "-E", "-x", "c++", "-"),
            input=source,
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
            timeout=30,
        )
    except (OSError, UnicodeError, subprocess.TimeoutExpired) as error:
        raise Unproven(f"metadata preprocessing failed: {error}") from error
    if process.returncode or len(process.stdout) > 32_000_000:
        raise Unproven("metadata preprocessing exceeded its compiler/size contract")
    check_preprocessing(process.stdout)
    command = (
        *profile,
        "-fsyntax-only",
        "-x",
        "c++-cpp-output",
        "-Xclang",
        "-ast-dump=json",
        "-",
    )
    return _compile_ast_command(process.stdout, command)


def check_preprocessing(source: str) -> None:
    """Keep the mandatory diagnostics effective throughout the translation unit."""
    for match in re.finditer(
        r"^\s*#\s*pragma\s+((?:clang|GCC)\s+diagnostic\b[^\n]*)", source, re.M
    ):
        directive = match[1].split()
        # Compiler output is canonicalized; accept only stack operations and
        # the SDK's macro-redefinition suppression. Unknown suppression groups
        # could include VLA or return-type, so they cannot be inherited safely.
        if len(directive) == 3 and directive[2] in {"push", "pop"}:
            continue
        if len(directive) == 4 and (
            directive[2] in {"error", "fatal"}
            or (directive[2] == "ignored" and directive[3] == '"-Wmacro-redefined"')
        ):
            continue
        raise Unproven("metadata source changes the mandatory diagnostic profile")


def _compile_ast_command(source: str, command: tuple[str, ...]) -> _CompilerAst:
    """Internal driver hook; callers must enforce the canonical warning profile."""
    try:
        process = subprocess.run(
            command,
            input=source,
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
            timeout=30,
        )
    except (OSError, UnicodeError, subprocess.TimeoutExpired) as error:
        raise Unproven(f"metadata compiler unavailable: {error}") from error
    if process.returncode or len(process.stdout) > 32_000_000:
        raise Unproven("metadata translation unit did not compile within the AST limit")
    try:
        tree = json.loads(process.stdout)
    except (ValueError, RecursionError) as error:
        raise Unproven("invalid metadata compiler AST") from error
    if not isinstance(tree, dict) or tree.get("kind") != "TranslationUnitDecl":
        raise Unproven("metadata compiler did not emit a complete translation unit")
    return _CompilerAst(
        tree, command, hashlib.sha256(source.encode("utf-8")).hexdigest()
    )
