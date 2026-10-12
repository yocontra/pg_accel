"""Typed workspace-helper call facts, not an approval of the caller's effects.

Caller/callee IDs must come from one compiler view. The workspace parameter
must remain a stable pointer value throughout the caller. Metadata parameters
are an explicit contract, not inferred from a record's name. Facts preserve
returned pointer aliases for a future control-flow consumer; no release waiver
uses them.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import pathlib
import re

from workspace_effects import (
    Node,
    Unproven,
    Verifier,
    _CompilerView,
    _integer,
    _pointer,
    _type,
    _walk,
    compile_view,
    check_value_members,
)


@dataclasses.dataclass(frozen=True)
class AddressCall:
    start: int
    end: int
    callee_id: str
    base_parameter: int
    base_declaration: str
    nullable: bool
    destination_id: str | None
    destination_name: str | None
    destination_path: tuple[str, ...]
    conditional: bool
    local_reads: tuple[str, ...]


@dataclasses.dataclass(frozen=True)
class CallsResult:
    ok: bool
    detail: str
    calls: tuple[AddressCall, ...] = ()
    source_sha256: str = ""
    offset_inputs: tuple[str, ...] = ()


@dataclasses.dataclass(frozen=True)
class ControlFact:
    start: int
    end: int
    kind: str


@dataclasses.dataclass(frozen=True)
class ControlCallsResult:
    """Conditional explicit-control facts, not memory or device approval.

    Metadata reads require the declared input contract. The checked syntactic
    uses cannot establish that unknown ordinary host calls do not mutate the
    same storage through external aliases. Those effects, alias contracts,
    bounds, lifetime and build binding remain independent caller obligations.
    """

    ok: bool
    detail: str
    calls: tuple[AddressCall, ...] = ()
    source_sha256: str = ""
    # Includes both offset and control inputs after transitive closure.
    offset_inputs: tuple[str, ...] = ()
    controls: tuple[ControlFact, ...] = ()


def _children(node: Node) -> list[Node]:
    return node.get("inner", [])


def _strip(node: Node, casts: frozenset[str] = frozenset()) -> Node:
    while len(_children(node)) == 1 and (
        node.get("kind") == "ParenExpr"
        or (node.get("kind") == "ImplicitCastExpr" and node.get("castKind") in casts)
    ):
        node = _children(node)[0]
    return node


class CallVerifier:
    def __init__(self, view: _CompilerView):
        self.effects = Verifier(view)
        self.view = view
        self.definitions = {
            node["id"]: node
            for root in view.roots
            for node in _walk(root)
            if node.get("kind") == "FunctionDecl"
            and any(child.get("kind") == "CompoundStmt" for child in _children(node))
        }

    def prove(
        self,
        caller: str,
        helper: str,
        *,
        base_parameter: int,
        metadata_parameters: tuple[int, ...] = (),
    ) -> CallsResult:
        result = self._prove(
            caller,
            helper,
            base_parameter=base_parameter,
            metadata_parameters=metadata_parameters,
            controls=False,
        )
        assert isinstance(result, CallsResult)
        return result

    def prove_controls(
        self,
        caller: str,
        helper: str,
        *,
        base_parameter: int,
        metadata_parameters: tuple[int, ...] = (),
    ) -> ControlCallsResult:
        result = self._prove(
            caller,
            helper,
            base_parameter=base_parameter,
            metadata_parameters=metadata_parameters,
            controls=True,
        )
        assert isinstance(result, ControlCallsResult)
        return result

    def _prove(
        self,
        caller: str,
        helper: str,
        *,
        base_parameter: int,
        metadata_parameters: tuple[int, ...],
        controls: bool,
    ) -> CallsResult | ControlCallsResult:
        result_type = ControlCallsResult if controls else CallsResult
        try:
            candidates = [
                n for n in self.definitions.values() if n.get("name") == caller
            ]
            if len(candidates) != 1:
                raise Unproven("caller does not have one concrete definition")
            function = candidates[0]
            parameters = [
                n for n in _children(function) if n.get("kind") == "ParmVarDecl"
            ]
            indices = (base_parameter, *metadata_parameters)
            if any(index < 0 or index >= len(parameters) for index in indices):
                raise Unproven("caller parameter contract is out of bounds")
            if len(set(indices)) != len(indices):
                raise Unproven("caller parameter contracts overlap")
            base = parameters[base_parameter]
            if _type(base) not in {"void *", "void *const"}:
                raise Unproven("caller base must be a plain void pointer parameter")
            metadata = {parameters[index]["id"] for index in metadata_parameters}
            for index in metadata_parameters:
                spelling = _type(parameters[index]).replace(
                    "(anonymous namespace)::", ""
                )
                if not spelling.endswith(" &") or any(c in spelling for c in "*[]()"):
                    raise Unproven("metadata root must be an explicit record reference")
            locals_by_id = {
                n["id"]: n
                for n in _walk(function)
                if n.get("kind") in {"VarDecl", "ParmVarDecl"}
            }
            parents = {
                child["id"]: node
                for node in _walk(function)
                for child in _children(node)
                if "id" in child
            }
            self._stable_base(function, base, parents)
            local_reads: set[str] = set()
            inputs: set[str] = set()
            metadata_inputs: set[str] = set()
            initialized: set[str] = set()
            initializing: set[str] = set()
            copy_types: set[str] = set()
            value_members: set[tuple[str, str]] = set()
            control_facts: list[ControlFact] = []
            for node in _walk(function):
                if (
                    node.get("kind") != "DeclRefExpr"
                    or node.get("referencedDecl", {}).get("name") != helper
                ):
                    continue
                use = node
                parent = parents.get(use["id"], {})
                while parent.get("kind") == "ParenExpr" or (
                    parent.get("kind") == "ImplicitCastExpr"
                    and parent.get("castKind") == "FunctionToPointerDecay"
                ):
                    use, parent = parent, parents.get(parent["id"], {})
                if (
                    parent.get("kind") != "CallExpr"
                    or not _children(parent)
                    or _children(parent)[0].get("id") != use["id"]
                ):
                    raise Unproven("helper reference escapes a direct call")

            def scalar(node: Node) -> bool:
                if (
                    controls
                    and node.get("kind") == "DeclRefExpr"
                    and node.get("referencedDecl", {}).get("kind") == "EnumConstantDecl"
                ):
                    return True
                if not _integer(node):
                    return False
                kind = node.get("kind")
                children = _children(node)
                if kind in {"IntegerLiteral", "CXXBoolLiteralExpr"}:
                    return True
                if kind == "DeclRefExpr":
                    ref = node.get("referencedDecl", {})
                    declaration = locals_by_id.get(ref.get("id"))
                    # Only ordinary automatic scalar storage or parameters.
                    # TLS/static initialization can execute at the use site.
                    ordinary = bool(
                        declaration is not None
                        and _integer(declaration)
                        and not declaration.get("tls")
                        and declaration.get("storageClass") not in {"static", "extern"}
                    )
                    if ordinary:
                        return local_input(declaration)
                    return bool(
                        node.get("nonOdrUseReason") == "constant"
                        and _type(node).startswith("const ")
                    )
                if kind in {"MemberExpr", "ArraySubscriptExpr"}:
                    return place(node, frozenset())
                if len(children) == 1 and (
                    kind == "ParenExpr"
                    or (
                        kind in {"ImplicitCastExpr", "CXXStaticCastExpr"}
                        and node.get("castKind")
                        in {
                            "LValueToRValue",
                            "NoOp",
                            "IntegralCast",
                            "IntegralToBoolean",
                        }
                    )
                    or (
                        kind == "UnaryOperator"
                        and node.get("opcode") in {"-", "+", "~", "!"}
                    )
                ):
                    return scalar(children[0])
                if (
                    kind == "BinaryOperator"
                    and node.get("opcode")
                    in (
                        {
                            "+",
                            "-",
                            "*",
                            "&",
                            "|",
                            "^",
                            "<<",
                            ">>",
                            "==",
                            "!=",
                            "<",
                            "<=",
                            ">",
                            ">=",
                        }
                        | ({"&&", "||"} if controls else set())
                    )
                    and len(children) == 2
                ):
                    return all(scalar(child) for child in children)
                if controls and kind == "ConditionalOperator" and len(children) == 3:
                    return all(scalar(child) for child in children)
                return False

            def place(node: Node, visiting: frozenset[str]) -> bool:
                node = _strip(node)
                kind = node.get("kind")
                children = _children(node)
                if "volatile" in _type(node).split():
                    return False
                if kind == "DeclRefExpr":
                    identity = node.get("referencedDecl", {}).get("id")
                    if identity in metadata:
                        if controls:
                            metadata_inputs.add(identity)
                        return True
                    declaration = locals_by_id.get(identity)
                    if (
                        declaration is not None
                        and declaration.get("kind") == "VarDecl"
                        and not any(c in _type(declaration) for c in "*&[]()")
                        and not declaration.get("storageClass")
                        and not declaration.get("tls")
                    ):
                        return local_input(declaration)
                    if (
                        declaration is None
                        or identity in visiting
                        or not _type(declaration).endswith(" &")
                        or declaration.get("storageClass")
                        or declaration.get("tls")
                        or len(_children(declaration)) != 1
                    ):
                        return False
                    return place(_children(declaration)[0], visiting | {identity})
                if (
                    kind == "MemberExpr"
                    and len(children) == 1
                    and not node.get("isArrow")
                ):
                    value_members.add((_type(children[0]), node["name"]))
                    return place(children[0], visiting)
                if kind == "ArraySubscriptExpr" and len(children) == 2:
                    array = children[0]
                    if (
                        array.get("kind") != "ImplicitCastExpr"
                        or array.get("castKind") != "ArrayToPointerDecay"
                        or len(_children(array)) != 1
                    ):
                        return False
                    original = _children(array)[0]
                    # Declared fixed arrays, never pointer fields or row buffers.
                    if re.search(r"\[[1-9][0-9]*\]$", _type(original)) is None:
                        return False
                    return place(original, visiting) and scalar(children[1])
                return False

            def metadata_copy(node: Node) -> bool:
                children = _children(node)
                name = _type(node).removeprefix("const ")
                if (
                    node.get("kind") != "CXXConstructExpr"
                    or node.get("constructionKind") != "complete"
                    or len(children) != 1
                    or any(c in name for c in "*&[]()")
                    or _type(children[0]) != "const " + name
                    or children[0].get("valueCategory") != "lvalue"
                ):
                    return False
                argument = _strip(children[0], frozenset({"NoOp"}))
                if not place(argument, frozenset()):
                    return False
                copy_types.add(name)
                return True

            def local_input(declaration: Node) -> bool:
                identity = declaration["id"]
                inputs.add(identity)
                if declaration.get("kind") == "VarDecl":
                    local_reads.add(identity)
                if identity in initialized:
                    return True
                if identity in initializing:
                    return False
                if declaration.get("kind") == "ParmVarDecl":
                    # Scalar by-value inputs start initialized. Their later
                    # assignments/escapes still have to pass the use check.
                    initialized.add(identity)
                    return _integer(declaration)
                children = _children(declaration)
                if len(children) != 1 or not declaration.get("init"):
                    return False
                initializing.add(identity)
                valid = (
                    scalar(children[0])
                    if _integer(declaration)
                    else metadata_copy(children[0])
                )
                initializing.remove(identity)
                if valid:
                    initialized.add(identity)
                return valid

            def check_input_uses(identity: str) -> None:
                integer = _integer(locals_by_id[identity])
                for node in _walk(function):
                    if (
                        node.get("kind") != "DeclRefExpr"
                        or node.get("referencedDecl", {}).get("id") != identity
                    ):
                        continue
                    use = node
                    parent = parents.get(use["id"], {})
                    conditional_read = False
                    # Local metadata copies may only expose value/member reads
                    # or another compiler-proved trivial same-type copy. In
                    # particular, even a const-reference escape is not trusted.
                    while True:
                        kind = parent.get("kind")
                        if kind == "ParenExpr" or (
                            not integer
                            and kind == "ImplicitCastExpr"
                            and parent.get("castKind")
                            in {"NoOp", "ArrayToPointerDecay"}
                        ):
                            pass
                        elif (
                            not integer
                            and kind == "MemberExpr"
                            and not parent.get("isArrow")
                        ):
                            pass
                        elif (
                            not integer
                            and kind == "ArraySubscriptExpr"
                            and _children(parent)[0].get("id") == use["id"]
                            and scalar(_children(parent)[1])
                        ):
                            pass
                        elif (
                            controls
                            and integer
                            and parent.get("kind") == "ConditionalOperator"
                            and any(
                                child.get("id") == use["id"]
                                for child in _children(parent)[1:]
                            )
                            and scalar(parent)
                        ):
                            conditional_read = True
                        else:
                            break
                        use, parent = parent, parents.get(parent["id"], {})
                    if (
                        parent.get("kind") == "ImplicitCastExpr"
                        and parent.get("castKind") == "LValueToRValue"
                    ):
                        continue
                    if parent.get("kind") == "UnaryExprOrTypeTraitExpr" and parent.get(
                        "name"
                    ) in {"sizeof", "alignof"}:
                        continue
                    if not integer and metadata_copy(parent):
                        continue
                    if conditional_read:
                        raise Unproven("conditional input place escapes a value read")
                    children = _children(parent)
                    if (
                        integer
                        and parent.get("kind")
                        in {"BinaryOperator", "CompoundAssignOperator"}
                        and parent.get("opcode")
                        in {"=", "+=", "-=", "*=", "&=", "|=", "^=", "<<=", ">>="}
                        and len(children) == 2
                        and children[0].get("id") == use["id"]
                        and scalar(children[1])
                    ):
                        continue
                    if (
                        integer
                        and parent.get("kind") == "UnaryOperator"
                        and parent.get("opcode") in {"++", "--"}
                    ):
                        continue
                    raise Unproven(
                        f"workspace offset input {locals_by_id[identity].get('name', identity)} "
                        f"has an unproved {parent.get('kind', 'missing')} use"
                    )

            def destination_place(node: Node) -> tuple[Node | None, tuple[str, ...]]:
                node = _strip(node, frozenset({"LValueToRValue", "NoOp"}))
                if node.get("kind") == "DeclRefExpr":
                    ref = node.get("referencedDecl", {})
                    return locals_by_id.get(ref.get("id")), ()
                if node.get("kind") == "MemberExpr" and len(_children(node)) == 1:
                    root, path = destination_place(_children(node)[0])
                    return root, (*path, node["name"])
                if (
                    node.get("kind") == "ArraySubscriptExpr"
                    and len(_children(node)) == 2
                ):
                    array = _strip(
                        _children(node)[0], frozenset({"ArrayToPointerDecay"})
                    )
                    root, path = destination_place(array)
                    return root, (*path, "[*]")
                return None, ()

            facts: list[AddressCall] = []
            seen_calls: set[str] = set()
            for call in _walk(function):
                if call.get("kind") != "CallExpr" or not _children(call):
                    continue
                if call["id"] in seen_calls:
                    continue
                seen_calls.add(call["id"])
                target = _strip(
                    _children(call)[0], frozenset({"FunctionToPointerDecay"})
                )
                reference = target.get("referencedDecl", {})
                if (
                    target.get("kind") != "DeclRefExpr"
                    or reference.get("name") != helper
                ):
                    continue
                if reference.get("kind") != "FunctionDecl":
                    raise Unproven("workspace target is not a direct function")
                definition = self.definitions.get(reference.get("id"))
                if definition is None:
                    raise Unproven(
                        "resolved helper definition is absent from this compiler view"
                    )
                nullable = self.effects._function(definition)
                if len(_children(call)) != 3:
                    raise Unproven("workspace call does not have exactly two arguments")
                pointer = _strip(
                    _children(call)[1], frozenset({"LValueToRValue", "NoOp"})
                )
                if (
                    pointer.get("kind") != "DeclRefExpr"
                    or pointer.get("referencedDecl", {}).get("id") != base["id"]
                    or _type(pointer) not in {"void *", "void *const"}
                ):
                    raise Unproven(
                        "workspace base has a conversion or unresolved alias"
                    )
                if not scalar(_children(call)[2]):
                    raise Unproven(
                        "workspace offset has a row read, conversion "
                        "or unmodeled effect"
                    )
                use = call
                parent = parents.get(use["id"], {})
                conditional = False
                while (
                    parent.get("kind") == "ParenExpr"
                    or (
                        parent.get("kind") == "ImplicitCastExpr"
                        and parent.get("castKind") in {"NoOp", "BitCast"}
                        and _pointer(parent)
                    )
                    or (
                        parent.get("kind") == "ConditionalOperator"
                        and any(
                            child.get("id") == use["id"]
                            for child in _children(parent)[1:]
                        )
                    )
                ):
                    conditional |= parent.get("kind") == "ConditionalOperator"
                    use, parent = parent, parents.get(parent["id"], {})
                destination = None
                destination_path: tuple[str, ...] = ()
                if parent.get("kind") == "VarDecl" and _pointer(
                    {"type": {"qualType": _type(parent).removesuffix("const")}}
                ):
                    destination = parent
                elif (
                    parent.get("kind") == "BinaryOperator"
                    and parent.get("opcode") == "="
                    and len(_children(parent)) == 2
                    and _children(parent)[1].get("id") == use["id"]
                    and _pointer(_children(parent)[0])
                ):
                    destination, destination_path = destination_place(
                        _children(parent)[0]
                    )
                span = call.get("range", {})
                start, end = span.get("begin", {}), span.get("end", {})
                if "offset" not in start or "offset" not in end or "tokLen" not in end:
                    raise Unproven("call has no exact expanded-source range")
                facts.append(
                    AddressCall(
                        start["offset"],
                        end["offset"] + end["tokLen"],
                        definition["id"],
                        base_parameter,
                        base["id"],
                        nullable,
                        destination.get("id") if destination else None,
                        destination.get("name") if destination else None,
                        destination_path,
                        conditional,
                        tuple(sorted(local_reads)),
                    )
                )
                local_reads.clear()
            if not facts:
                raise Unproven("caller has no resolved calls to the requested helper")

            def record_control(node: Node) -> None:
                span = node.get("range", {})
                begin, end = span.get("begin", {}), span.get("end", {})
                if "offset" not in begin or "offset" not in end or "tokLen" not in end:
                    raise Unproven("control has no exact expanded-source range")
                control_facts.append(
                    ControlFact(
                        begin["offset"], end["offset"] + end["tokLen"], node["kind"]
                    )
                )

            def pure_control(node: Node) -> None:
                if not scalar(node):
                    raise Unproven(
                        f"unproved control expression {node.get('kind', 'missing')} "
                        f"at expanded offset {node.get('range', {}).get('begin', {}).get('offset', 'unknown')}"
                    )
                record_control(node)

            def induction(node: Node) -> None:
                node = _strip(node)
                children = _children(node)
                if node.get("kind") == "DeclStmt":
                    if not children or any(
                        child.get("kind") != "VarDecl"
                        or not _integer(child)
                        or not local_input(child)
                        for child in children
                    ):
                        raise Unproven("unproved loop declaration")
                elif (
                    node.get("kind") == "UnaryOperator"
                    and node.get("opcode") in {"++", "--"}
                    and len(children) == 1
                ):
                    target = _strip(children[0])
                    declaration = locals_by_id.get(
                        target.get("referencedDecl", {}).get("id"), {}
                    )
                    if (
                        target.get("kind") != "DeclRefExpr"
                        or not _integer(declaration)
                        or not local_input(declaration)
                    ):
                        raise Unproven("unproved loop induction target")
                elif (
                    node.get("kind") in {"BinaryOperator", "CompoundAssignOperator"}
                    and node.get("opcode")
                    in {"=", "+=", "-=", "*=", "&=", "|=", "^=", "<<=", ">>="}
                    and len(children) == 2
                ):
                    target = _strip(children[0])
                    declaration = locals_by_id.get(
                        target.get("referencedDecl", {}).get("id"), {}
                    )
                    if (
                        target.get("kind") != "DeclRefExpr"
                        or not _integer(declaration)
                        or not local_input(declaration)
                        or not scalar(children[1])
                    ):
                        raise Unproven("unproved loop update")
                else:
                    raise Unproven(
                        f"unproved loop clause {node.get('kind', 'missing')}"
                    )
                record_control(node)

            def host_controls(node: Node, loops: int = 0) -> None:
                kind, children = node.get("kind"), _children(node)
                if kind == "LambdaExpr":
                    # Capture initializers run here; the body belongs to a
                    # different function and must never donate qualified calls.
                    for child in children:
                        if child.get("kind") not in {"CXXRecordDecl", "CompoundStmt"}:
                            if not scalar(child) and not metadata_copy(child):
                                raise Unproven("unproved lambda capture initialization")
                    return
                if kind == "IfStmt":
                    if (
                        node.get("hasInit")
                        or node.get("hasVar")
                        or len(children) not in {2, 3}
                    ):
                        raise Unproven(
                            "if initializer/condition declaration is unsupported"
                        )
                    pure_control(children[0])
                    for child in children[1:]:
                        host_controls(child, loops)
                    return
                if kind == "ForStmt":
                    # Clang preserves empty slots: init, condition-variable,
                    # condition, increment, body. Never infer roles by filtering.
                    if len(children) != 5 or children[1]:
                        raise Unproven("unmodeled for statement roles")
                    if children[0]:
                        induction(children[0])
                    if children[2]:
                        pure_control(children[2])
                    if children[3]:
                        induction(children[3])
                    host_controls(children[4], loops + 1)
                    return
                if kind in {"WhileStmt", "DoStmt"}:
                    if len(children) != 2 or node.get("hasVar"):
                        raise Unproven("loop condition declaration is unsupported")
                    condition, body = (
                        children if kind == "WhileStmt" else children[::-1]
                    )
                    pure_control(condition)
                    host_controls(body, loops + 1)
                    return
                if kind in {"BreakStmt", "ContinueStmt"}:
                    if not loops:
                        raise Unproven("control transfer outside a modeled loop")
                    record_control(node)
                    return
                if kind == "ReturnStmt":
                    if len(children) > 1:
                        raise Unproven("unmodeled return")
                    if children:
                        pure_control(children[0])
                    record_control(node)
                    return
                if kind == "ConditionalOperator":
                    if len(children) != 3:
                        raise Unproven("unmodeled conditional expression")
                    pure_control(children[0])
                elif kind == "BinaryOperator" and node.get("opcode") in {"&&", "||"}:
                    pure_control(node)
                    return
                if kind and (
                    (
                        kind.endswith("Stmt")
                        and kind not in {"CompoundStmt", "DeclStmt", "NullStmt"}
                    )
                    or kind
                    in {
                        "CXXThrowExpr",
                        "StmtExpr",
                        "CoawaitExpr",
                        "CoyieldExpr",
                        "CoroutineBodyStmt",
                        "BinaryConditionalOperator",
                    }
                ):
                    raise Unproven(f"unsupported explicit control {kind}")
                for child in children:
                    host_controls(child, loops)

            def immutable_metadata() -> None:
                for node in _walk(function):
                    if (
                        node.get("kind") != "DeclRefExpr"
                        or node.get("referencedDecl", {}).get("id") not in metadata
                    ):
                        continue
                    use, parent = node, parents.get(node["id"], {})
                    while True:
                        kind = parent.get("kind")
                        children = _children(parent)
                        if kind == "ParenExpr" or (
                            kind == "ImplicitCastExpr"
                            and parent.get("castKind")
                            in {"NoOp", "ArrayToPointerDecay"}
                        ):
                            pass
                        elif kind == "MemberExpr" and not parent.get("isArrow"):
                            value_members.add((_type(use), parent["name"]))
                        elif (
                            kind == "ArraySubscriptExpr"
                            and len(children) == 2
                            and children[0].get("id") == use["id"]
                            and scalar(children[1])
                        ):
                            pass
                        elif (
                            kind == "ConditionalOperator"
                            and len(children) == 3
                            and any(
                                child.get("id") == use["id"] for child in children[1:]
                            )
                            and scalar(parent)
                        ):
                            pass
                        else:
                            break
                        use, parent = parent, parents.get(parent["id"], {})
                    if (
                        parent.get("kind") == "ImplicitCastExpr"
                        and parent.get("castKind") == "LValueToRValue"
                        and _integer(parent)
                    ):
                        continue
                    if parent.get("kind") == "UnaryExprOrTypeTraitExpr" and parent.get(
                        "name"
                    ) in {"sizeof", "alignof"}:
                        continue
                    if metadata_copy(parent):
                        continue
                    raise Unproven(
                        f"metadata root has an unproved mutation/escape {parent.get('kind', 'missing')} "
                        f"at expanded offset {parent.get('range', {}).get('begin', {}).get('offset', 'unknown')}"
                    )

            if controls:
                body = next(
                    n for n in _children(function) if n.get("kind") == "CompoundStmt"
                )
                host_controls(body)
                immutable_metadata()
            checked_inputs: set[str] = set()
            while pending := inputs - checked_inputs:
                for identity in sorted(pending):
                    check_input_uses(identity)
                    checked_inputs.add(identity)
            if controls:
                # Check after dependency closure so captures cannot hide a
                # transitive input. Even a nested helper-only call declines
                # the complete qualification, rather than yielding a subset.
                for node in _walk(function):
                    if node.get("kind") != "LambdaExpr":
                        continue
                    for nested in _walk(node):
                        if nested.get("kind") == "DeclRefExpr":
                            reference = nested.get("referencedDecl", {})
                            if reference.get("name") == helper or reference.get(
                                "id"
                            ) in inputs | metadata | {base["id"]}:
                                raise Unproven(
                                    "workspace/control dependency crosses a closure boundary"
                                )
            check_value_members(self.view, value_members, copy_types)
            if controls:
                return ControlCallsResult(
                    True,
                    "explicit host controls and syntactic metadata-use discipline qualified under declared input contracts; ordinary host effects, external aliases, bounds and device execution unproved; caller not approved",
                    tuple(facts),
                    self.view.source_sha256,
                    tuple(sorted(inputs | metadata_inputs)),
                    tuple(control_facts),
                )
            return CallsResult(
                True,
                "stable workspace parameter, checked offset initializer/update expressions, "
                "typed call arguments and "
                "returned-alias facts; caller not approved",
                tuple(facts),
                self.view.source_sha256,
                tuple(sorted(inputs)),
            )
        except Unproven as error:
            # A partial fact set must never be consumed as a complete result.
            return result_type(False, str(error), source_sha256=self.view.source_sha256)

    @staticmethod
    def _stable_base(function: Node, base: Node, parents: dict[str, Node]) -> None:
        """Prove value identity, not pointed-to memory effects or lifetime.

        Every evaluated use must copy the incoming pointer value. In particular,
        a declaration ID is not a provenance proof after assignment, and a
        reference/pointer-to-parameter escape could hide a later assignment.
        Inspect the complete body (including lambdas), not only uses before a
        call in source order: a later write can reach that call on a backedge.
        """
        for node in _walk(function):
            if (
                node.get("kind") != "DeclRefExpr"
                or node.get("referencedDecl", {}).get("id") != base["id"]
            ):
                continue
            use = node
            parent = parents.get(use["id"], {})
            while parent.get("kind") == "ParenExpr":
                use, parent = parent, parents.get(parent["id"], {})
            if (
                parent.get("kind") == "ImplicitCastExpr"
                and parent.get("castKind") == "LValueToRValue"
                and _type(parent) == "void *"
            ):
                continue
            # sizeof/alignof on the plain pointer itself cannot evaluate it.
            # Do not accept arbitrary unevaluated subtrees: the immediate use
            # restriction keeps this independent of VLA/extension semantics.
            if (
                parent.get("kind") == "UnaryExprOrTypeTraitExpr"
                and parent.get("name") in {"sizeof", "alignof"}
                and _integer(parent)
            ):
                continue
            raise Unproven(
                "workspace parameter is reassigned or escapes a pointer-value read"
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=pathlib.Path)
    parser.add_argument("--caller", required=True)
    parser.add_argument("--helper", required=True)
    parser.add_argument("--filter", required=True)
    parser.add_argument("--base-parameter", required=True, type=int)
    parser.add_argument("--metadata-parameter", action="append", type=int, default=[])
    parser.add_argument("--include", action="append", type=pathlib.Path, default=[])
    args = parser.parse_args()
    try:
        view = compile_view(
            args.source.read_text(encoding="utf-8"),
            args.source.resolve().parent,
            tuple(args.include),
            name_filter=args.filter,
            retain_functions=frozenset({args.caller, args.helper}),
        )
        result = CallVerifier(view).prove(
            args.caller,
            args.helper,
            base_parameter=args.base_parameter,
            metadata_parameters=tuple(args.metadata_parameter),
        )
    except (OSError, UnicodeError, Unproven) as error:
        result = CallsResult(False, str(error))
    print(json.dumps(dataclasses.asdict(result), indent=2, sort_keys=True))
    return 0 if result.ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
