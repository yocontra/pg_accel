#!/usr/bin/env python3
"""Fail-closed ABI and all-success-path audit for GPU kernel exports.

The analyzer uses a C++ lexer and balanced structural parser rather than text
search.  It inventories every extern-C ``pgaccel_*`` definition regardless of
return type, compares source and normalized-preprocessor views with public
header declarations, and records stable inventory hashes.  For compute paths,
only a nonempty, correctly scoped SYCL launch that contributes to a mutable ABI
output can dominate success.  Host finalization, dead or deferred launches,
ambiguous staging, unresolved templates/overloads, and recursive helpers fail
closed.  Narrow lifecycle, failure-only, and zero-work paths require explicit
source evidence and never borrow GPU execution counters as proof.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import pathlib
import re
import sys
from collections import Counter, defaultdict
from collections.abc import Iterable, Sequence


DISPATCH_METHODS = frozenset({"parallel_for", "single_task", "submit"})
CONTROL_KEYWORDS = frozenset(
    {
        "alignas",
        "alignof",
        "catch",
        "decltype",
        "for",
        "if",
        "noexcept",
        "requires",
        "sizeof",
        "static_assert",
        "switch",
        "while",
    }
)
CALL_KEYWORDS = CONTROL_KEYWORDS | frozenset(
    {
        "co_await",
        "co_return",
        "const_cast",
        "delete",
        "dynamic_cast",
        "new",
        "reinterpret_cast",
        "return",
        "static_cast",
        "throw",
        "typeid",
    }
)
NON_GRAPH_CALLS = frozenset(
    {
        # Runtime accounting is audited separately and never proves dispatch.
        "pgaccel_record_gpu_exec",
        # Standard math is host work, not a locally resolvable launch helper.
        "cos",
        "sin",
    }
)
FAILURE_STATUSES = frozenset(
    {
        "PGACCEL_ERROR",
        "PGACCEL_UNSUPPORTED",
        "PGACCEL_OOM",
        "PGACCEL_TIMEOUT",
        "PGACCEL_ERROR_INIT",
        "PGACCEL_ERROR_NO_DEVICE",
        "PGACCEL_INVALID_ARGUMENT",
        "PGACCEL_ERROR_OOM",
        "PGACCEL_ERROR_TIMEOUT",
        "PGACCEL_ERROR_UNSUPPORTED",
    }
)


@dataclasses.dataclass(frozen=True)
class LifecycleContract:
    """An exact non-compute ABI contract and the source evidence it requires."""

    purpose: str
    required_sequences: tuple[tuple[str, ...], ...]
    allow_host_loops: bool = False


@dataclasses.dataclass(frozen=True)
class FailOnlyContract:
    """An ABI shim that accepts no nonempty compute work."""

    purpose: str
    required_sequences: tuple[tuple[str, ...], ...]


# These are ABI support operations, not compute kernels.  Exact names keep the
# exception boundary reviewable; required token sequences stop a vacuous name
# match from bypassing the audit.  Compute wrappers must never be added here.
LIFECYCLE_CONTRACTS: dict[str, LifecycleContract] = {
    "pgaccel_init": LifecycleContract(
        "SYCL runtime/device initialization",
        (("sycl", "::", "device", "::", "get_devices", "("),),
        allow_host_loops=True,
    ),
    "pgaccel_shutdown": LifecycleContract(
        "SYCL queue teardown",
        (("wait_and_throw", "("), ("delete", "g_queue")),
    ),
    "pgaccel_archive_stats_snapshot": LifecycleContract(
        "AdaptiveCpp JIT-cache diagnostics",
        (("std", "::", "filesystem", "::", "directory_iterator", "("),),
        allow_host_loops=True,
    ),
    "pgaccel_archive_jit_cache_dir": LifecycleContract(
        "AdaptiveCpp JIT-cache diagnostics",
        (("resolve_jit_cache_dir", "("),),
    ),
    "pgaccel_expr_shared_alloc": LifecycleContract(
        "shared-USM allocation",
        (("sycl", "::", "malloc_shared", "("),),
    ),
    "pgaccel_expr_device_alloc": LifecycleContract(
        "device-USM allocation",
        (("sycl", "::", "malloc_device", "("),),
    ),
    "pgaccel_expr_device_alloc_copy": LifecycleContract(
        "resident allocation and transfer",
        (
            ("sycl", "::", "malloc_shared", "("),
            ("memcpy", "("),
        ),
    ),
    "pgaccel_expr_device_copy_from_host": LifecycleContract(
        "host-to-device transfer",
        (("memcpy", "("),),
    ),
    "pgaccel_expr_device_copy_to_host": LifecycleContract(
        "device-to-host transfer",
        (("memcpy", "("),),
    ),
    "pgaccel_grouped_agg_workspace_requirements": LifecycleContract(
        "workspace metadata calculation",
        (("validate_desc", "("),),
    ),
    "pgaccel_grouped_agg_workspace_alloc": LifecycleContract(
        "workspace USM allocation",
        (
            ("sycl", "::", "aligned_alloc_shared", "("),
            ("sycl", "::", "aligned_alloc_device", "("),
        ),
    ),
    "pgaccel_spatial_workspace_finish": LifecycleContract(
        "post-dispatch failure-flag readback",
        (
            ("pgaccel_d2h", "("),
            ("workspace", "->", "failure_flags"),
        ),
    ),
}


FAIL_ONLY_CONTRACTS: dict[str, FailOnlyContract] = {
    "pgaccel_spatial_eval_resident_ex": FailOnlyContract(
        "zero-row contract validator; nonempty work is unsupported",
        (
            ("request", "->", "count", "!=", "0"),
            ("return", "PGACCEL_UNSUPPORTED", ";"),
            ("resident_validate_request_contract", "("),
        ),
    ),
    "pgaccel_spatial_intersects": FailOnlyContract(
        "zero-row compatibility shim; nonempty work is unsupported",
        (
            ("count_a", "==", "0"),
            ("count_b", "==", "0"),
            ("return", "PGACCEL_OK", ";"),
            ("return", "PGACCEL_UNSUPPORTED", ";"),
        ),
    ),
}


class ParseError(ValueError):
    """Source is not structurally safe to audit."""


@dataclasses.dataclass(frozen=True)
class PreprocessorDirective:
    line: int
    text: str


def normalize_preprocessor(
    source: str,
) -> tuple[str, tuple[PreprocessorDirective, ...]]:
    """Remove definitely inactive branches while preserving source locations.

    Unknown feature conditions are intentionally retained and later treated as
    ambiguous when they affect an export.  The normalizer is not a C++ macro
    expander; export-bearing macros are separately reported and fail closed.
    """

    lines = source.splitlines(keepends=True)
    output: list[str] = []
    directives: list[PreprocessorDirective] = []
    # Each frame is (parent active, known condition, selected branch active).
    stack: list[tuple[bool, bool | None, bool]] = []
    active = True
    index = 0
    while index < len(lines):
        line = lines[index]
        stripped = line.lstrip()
        if not stripped.startswith("#"):
            output.append(line if active else re.sub(r"[^\n]", " ", line))
            index += 1
            continue

        start = index
        logical = stripped.rstrip("\r\n")
        while logical.rstrip().endswith("\\") and index + 1 < len(lines):
            index += 1
            logical += "\n" + lines[index].rstrip("\r\n")
        directives.append(PreprocessorDirective(start + 1, logical))
        for directive_line in lines[start : index + 1]:
            output.append(re.sub(r"[^\n]", " ", directive_line))

        match = re.match(r"\s*#\s*([A-Za-z_][A-Za-z0-9_]*)\b(.*)", logical, re.S)
        command = match.group(1) if match else ""
        argument = match.group(2).strip() if match else ""
        if command in {"if", "ifdef", "ifndef"}:
            known: bool | None = None
            if command == "if" and re.fullmatch(r"\(?\s*0+[uUlL]*\s*\)?", argument):
                known = False
            elif command == "if" and re.fullmatch(r"\(?\s*1+[uUlL]*\s*\)?", argument):
                known = True
            selected = active and known is not False
            stack.append((active, known, selected))
            active = selected
        elif command == "else" and stack:
            parent, known, _ = stack[-1]
            selected = parent and known is not True
            stack[-1] = (parent, known, selected)
            active = selected
        elif command == "elif" and stack:
            parent, previous, _ = stack[-1]
            known = None
            if re.fullmatch(r"\(?\s*0+[uUlL]*\s*\)?", argument):
                known = False
            elif re.fullmatch(r"\(?\s*1+[uUlL]*\s*\)?", argument):
                known = True
            selected = parent and previous is not True and known is not False
            stack[-1] = (parent, known if previous is False else None, selected)
            active = selected
        elif command == "endif" and stack:
            parent, _, _ = stack.pop()
            active = parent
        index += 1

    if stack:
        raise ParseError("unterminated preprocessor conditional")
    return "".join(output), tuple(directives)


@dataclasses.dataclass(frozen=True)
class Token:
    kind: str
    value: str
    line: int
    column: int


@dataclasses.dataclass(frozen=True)
class Function:
    name: str
    line: int
    signature_start: int
    name_index: int
    lparen: int
    rparen: int
    body_open: int
    body_close: int
    parameter_count: int | None
    is_template: bool
    is_entrypoint: bool
    is_export: bool
    is_status: bool
    return_spelling: str


@dataclasses.dataclass(frozen=True)
class Call:
    name: str
    line: int
    argument_count: int | None
    explicit_template: bool


@dataclasses.dataclass(frozen=True)
class Finding:
    path: pathlib.Path
    line: int
    entrypoint: str
    message: str
    classifications: tuple[str, ...] = ()


@dataclasses.dataclass(frozen=True)
class EntrypointAudit:
    path: pathlib.Path
    line: int
    entrypoint: str
    ok: bool
    classifications: tuple[str, ...]
    detail: str
    is_status: bool = True
    return_type: str = "pgaccel_status"


@dataclasses.dataclass(frozen=True)
class FileAudit:
    path: pathlib.Path
    definitions: int
    entrypoints: int
    lifecycle_contracts: int
    entrypoint_audits: tuple[EntrypointAudit, ...]
    findings: tuple[Finding, ...]
    status_entrypoints: int = 0
    non_status_entrypoints: int = 0


@dataclasses.dataclass(frozen=True)
class AbiSymbol:
    path: pathlib.Path
    line: int
    name: str
    return_spelling: str
    parameter_count: int | None
    origin: str


@dataclasses.dataclass(frozen=True)
class AbiInventory:
    definitions: tuple[AbiSymbol, ...]
    declarations: tuple[AbiSymbol, ...]
    findings: tuple[Finding, ...]
    per_file: tuple[dict[str, object], ...]
    definition_hash: str
    source_definition_hash: str
    declaration_hash: str


@dataclasses.dataclass(frozen=True)
class _Proof:
    ok: bool
    detail: str
    classifications: tuple[str, ...]


def _advance_position(fragment: str, line: int, column: int) -> tuple[int, int]:
    newlines = fragment.count("\n")
    if newlines == 0:
        return line, column + len(fragment)
    return line + newlines, len(fragment.rsplit("\n", 1)[1]) + 1


def _raw_string_end(source: str, start: int) -> int | None:
    for prefix in ('u8R"', 'uR"', 'UR"', 'LR"', 'R"'):
        if not source.startswith(prefix, start):
            continue
        delimiter_start = start + len(prefix)
        open_paren = source.find("(", delimiter_start, delimiter_start + 17)
        if open_paren < 0:
            raise ParseError("invalid C++ raw-string delimiter")
        delimiter = source[delimiter_start:open_paren]
        if any(char.isspace() or char in "\\()" for char in delimiter):
            raise ParseError("invalid C++ raw-string delimiter")
        close_marker = ")" + delimiter + '"'
        close = source.find(close_marker, open_paren + 1)
        if close < 0:
            raise ParseError("unterminated C++ raw string")
        return close + len(close_marker)
    return None


def lex_cpp(source: str) -> list[Token]:
    """Tokenize structural C++ while discarding non-code text."""

    tokens: list[Token] = []
    index = 0
    line = 1
    column = 1
    length = len(source)

    while index < length:
        char = source[index]

        if char.isspace():
            next_index = index + 1
            while next_index < length and source[next_index].isspace():
                next_index += 1
            line, column = _advance_position(source[index:next_index], line, column)
            index = next_index
            continue

        line_start = source.rfind("\n", 0, index) + 1
        if char == "#" and not source[line_start:index].strip():
            end = index
            while True:
                newline = source.find("\n", end)
                if newline < 0:
                    end = length
                    break
                logical_line = source[end:newline].rstrip()
                end = newline + 1
                if not logical_line.endswith("\\"):
                    break
            line, column = _advance_position(source[index:end], line, column)
            index = end
            continue

        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            if end < 0:
                end = length
            line, column = _advance_position(source[index:end], line, column)
            index = end
            continue

        if source.startswith("/*", index):
            end = source.find("*/", index + 2)
            if end < 0:
                raise ParseError(f"unterminated block comment at line {line}")
            end += 2
            line, column = _advance_position(source[index:end], line, column)
            index = end
            continue

        raw_end = _raw_string_end(source, index)
        if raw_end is not None:
            tokens.append(Token("string", "<string>", line, column))
            line, column = _advance_position(source[index:raw_end], line, column)
            index = raw_end
            continue

        if char in {'"', "'"}:
            quote = char
            start_line = line
            end = index + 1
            escaped = False
            while end < length:
                current = source[end]
                if not escaped and current == quote:
                    end += 1
                    break
                if not escaped and current == "\n":
                    raise ParseError(f"unterminated literal at line {start_line}")
                if not escaped and current == "\\":
                    escaped = True
                else:
                    escaped = False
                end += 1
            else:
                raise ParseError(f"unterminated literal at line {start_line}")
            kind = (
                "string_c" if quote == '"' and source[index:end] == '"C"' else "literal"
            )
            tokens.append(
                Token(kind, "C" if kind == "string_c" else "<literal>", line, column)
            )
            line, column = _advance_position(source[index:end], line, column)
            index = end
            continue

        if char.isalpha() or char == "_":
            end = index + 1
            while end < length and (source[end].isalnum() or source[end] == "_"):
                end += 1
            value = source[index:end]
            tokens.append(Token("identifier", value, line, column))
            column += end - index
            index = end
            continue

        if char.isdigit():
            end = index + 1
            while end < length and (source[end].isalnum() or source[end] in "_.'"):
                end += 1
            tokens.append(Token("number", source[index:end], line, column))
            column += end - index
            index = end
            continue

        matched = None
        for operator in (
            "<=>",
            "->*",
            "<<=",
            ">>=",
            "...",
            "::",
            "->",
            ".*",
            "&&",
            "||",
            "==",
            "!=",
            "<=",
            ">=",
            "++",
            "--",
            "<<",
            ">>",
            "+=",
            "-=",
            "*=",
            "/=",
            "%=",
            "&=",
            "|=",
            "^=",
        ):
            if source.startswith(operator, index):
                matched = operator
                break
        value = matched or char
        tokens.append(Token("punctuation", value, line, column))
        index += len(value)
        column += len(value)

    return tokens


def _delimiter_pairs(tokens: Sequence[Token]) -> tuple[dict[int, int], dict[int, int]]:
    opening = {"(": ")", "[": "]", "{": "}"}
    closing = {value: key for key, value in opening.items()}
    stack: list[tuple[str, int]] = []
    forward: dict[int, int] = {}
    reverse: dict[int, int] = {}
    for index, token in enumerate(tokens):
        if token.value in opening:
            stack.append((token.value, index))
        elif token.value in closing:
            if not stack or stack[-1][0] != closing[token.value]:
                raise ParseError(
                    f"unbalanced {token.value!r} at line {token.line}, column {token.column}"
                )
            _, start = stack.pop()
            forward[start] = index
            reverse[index] = start
    if stack:
        value, index = stack[-1]
        token = tokens[index]
        raise ParseError(
            f"unclosed {value!r} at line {token.line}, column {token.column}"
        )
    return forward, reverse


def _brace_parents(tokens: Sequence[Token]) -> dict[int, int | None]:
    stack: list[int] = []
    parents: dict[int, int | None] = {}
    for index, token in enumerate(tokens):
        if token.value == "{":
            parents[index] = stack[-1] if stack else None
            stack.append(index)
        elif token.value == "}":
            if stack:
                stack.pop()
    return parents


def _extern_c_braces(tokens: Sequence[Token]) -> set[int]:
    braces: set[int] = set()
    for index, token in enumerate(tokens):
        if token.value != "{" or index < 2:
            continue
        if tokens[index - 2].value == "extern" and tokens[index - 1].kind == "string_c":
            braces.add(index)
    return braces


def _signature_start(tokens: Sequence[Token], name_index: int) -> int:
    index = name_index - 1
    while index >= 0 and tokens[index].value not in {";", "{", "}"}:
        index -= 1
    return index + 1


def _parameter_count(tokens: Sequence[Token], lparen: int, rparen: int) -> int | None:
    if rparen == lparen + 1:
        return 0
    values = [token.value for token in tokens[lparen + 1 : rparen]]
    if values == ["void"]:
        return 0

    paren = bracket = brace = angle = 0
    commas = 0
    for value in values:
        if value == "(":
            paren += 1
        elif value == ")":
            paren -= 1
        elif value == "[":
            bracket += 1
        elif value == "]":
            bracket -= 1
        elif value == "{":
            brace += 1
        elif value == "}":
            brace -= 1
        elif value == "<":
            angle += 1
        elif value == ">" and angle:
            angle -= 1
        elif value == ">>" and angle:
            angle = max(0, angle - 2)
        elif value == "," and not (paren or bracket or brace or angle):
            commas += 1
    if paren or bracket or brace or angle:
        return None
    return commas + 1


def _name_before_lparen(tokens: Sequence[Token], lparen: int) -> int | None:
    index = lparen - 1
    if index < 0:
        return None

    # Explicit specialization/instantiation: helper<T>(...).  C++ relational
    # expressions can look similar, so only cross a balanced angle suffix when
    # it ends directly before the parameter list.
    if tokens[index].value in {">", ">>"}:
        depth = 2 if tokens[index].value == ">>" else 1
        index -= 1
        while index >= 0 and depth:
            value = tokens[index].value
            if value == ">":
                depth += 1
            elif value == ">>":
                depth += 2
            elif value == "<":
                depth -= 1
            elif value == "<<":
                depth -= 2
            index -= 1
        if depth != 0:
            return None
    if index < 0 or tokens[index].kind != "identifier":
        return None
    return index


def _inside_extern_c(
    body_open: int,
    tokens: Sequence[Token],
    parents: dict[int, int | None],
    extern_braces: set[int],
    signature_start: int,
    name_index: int,
) -> bool:
    signature = tokens[signature_start:name_index]
    direct = any(token.value == "extern" for token in signature) and any(
        token.kind == "string_c" for token in signature
    )
    if direct:
        return True
    parent = parents.get(body_open)
    while parent is not None:
        if parent in extern_braces:
            return True
        parent = parents.get(parent)
    return False


def parse_functions(tokens: Sequence[Token]) -> list[Function]:
    forward, reverse = _delimiter_pairs(tokens)
    parents = _brace_parents(tokens)
    extern_braces = _extern_c_braces(tokens)
    candidates: list[Function] = []

    for body_open, body_close in forward.items():
        if tokens[body_open].value != "{":
            continue

        search = body_open - 1
        rparen = None
        name_index = None
        while search >= 0 and tokens[search].value not in {";", "{", "}"}:
            if tokens[search].value == ")" and search in reverse:
                possible_lparen = reverse[search]
                possible_name = _name_before_lparen(tokens, possible_lparen)
                if (
                    possible_name is not None
                    and tokens[possible_name].value not in CONTROL_KEYWORDS
                ):
                    start = _signature_start(tokens, possible_name)
                    prefix_values = {
                        token.value for token in tokens[start:possible_name]
                    }
                    if not (prefix_values & {"if", "for", "while", "switch", "catch"}):
                        rparen = search
                        name_index = possible_name
                        break
                search = possible_lparen - 1
                continue
            search -= 1
        if rparen is None or name_index is None:
            continue

        lparen = reverse[rparen]
        signature_start = _signature_start(tokens, name_index)
        prefix = tokens[signature_start:name_index]
        if not prefix:
            continue
        name = tokens[name_index].value
        is_export = name.startswith("pgaccel_") and _inside_extern_c(
            body_open,
            tokens,
            parents,
            extern_braces,
            signature_start,
            name_index,
        )
        suffix = tokens[rparen + 1 : body_open]
        is_status = any(token.value == "pgaccel_status" for token in (*prefix, *suffix))
        if any(token.value == "->" for token in suffix):
            arrow = next(
                index for index, token in enumerate(suffix) if token.value == "->"
            )
            return_tokens = suffix[arrow + 1 :]
        else:
            return_tokens = [
                token
                for token in prefix
                if token.value not in {"extern", "C", "static", "inline", "constexpr"}
                and token.kind != "string_c"
            ]
        return_spelling = " ".join(token.value for token in return_tokens).strip()
        is_entrypoint = is_export and is_status
        effective_body_close = body_close
        catch_cursor = body_close + 1
        while catch_cursor < len(tokens) and tokens[catch_cursor].value == "catch":
            catch_cursor += 1
            if catch_cursor >= len(tokens) or tokens[catch_cursor].value != "(":
                break
            catch_rparen = forward.get(catch_cursor)
            if catch_rparen is None or catch_rparen + 1 >= len(tokens):
                break
            catch_open = catch_rparen + 1
            if tokens[catch_open].value != "{" or catch_open not in forward:
                break
            effective_body_close = forward[catch_open]
            catch_cursor = effective_body_close + 1
        candidates.append(
            Function(
                name=name,
                line=tokens[name_index].line,
                signature_start=signature_start,
                name_index=name_index,
                lparen=lparen,
                rparen=rparen,
                body_open=body_open,
                body_close=effective_body_close,
                parameter_count=_parameter_count(tokens, lparen, rparen),
                is_template=any(token.value == "template" for token in prefix),
                is_entrypoint=is_entrypoint,
                is_export=is_export,
                is_status=is_status,
                return_spelling=return_spelling or "<unknown>",
            )
        )

    # Normal C++ functions cannot be nested.  Candidate definitions inside a
    # function are lambdas, requires-expressions, or control syntax that merely
    # resembles a definition and must not become call-graph nodes.
    candidates.sort(key=lambda function: (function.body_open, -function.body_close))
    functions: list[Function] = []
    active: list[Function] = []
    for candidate in candidates:
        active = [
            function for function in active if function.body_close > candidate.body_open
        ]
        if active:
            continue
        functions.append(candidate)
        active.append(candidate)
    return functions


def _contains_sequence(values: Sequence[str], sequence: Sequence[str]) -> bool:
    size = len(sequence)
    return any(
        tuple(values[index : index + size]) == tuple(sequence)
        for index in range(len(values) - size + 1)
    )


def _sequence_lines(tokens: Sequence[Token], sequence: Sequence[str]) -> list[int]:
    size = len(sequence)
    return [
        tokens[index].line
        for index in range(len(tokens) - size + 1)
        if tuple(token.value for token in tokens[index : index + size])
        == tuple(sequence)
    ]


def _sycl_receivers(tokens: Sequence[Token], function: Function) -> set[str]:
    receivers: set[str] = set()
    relevant = tokens[function.signature_start : function.body_close + 1]
    for index in range(len(relevant) - 3):
        if (
            relevant[index].value != "sycl"
            or relevant[index + 1].value != "::"
            or relevant[index + 2].value not in {"queue", "handler"}
        ):
            continue
        cursor = index + 3
        while cursor < len(relevant) and relevant[cursor].value not in {
            "=",
            ",",
            ")",
            ";",
            "{",
        }:
            token = relevant[cursor]
            if token.kind == "identifier" and token.value not in {"const", "volatile"}:
                receivers.add(token.value)
                break
            cursor += 1
    return receivers


def _contract_contains_dispatch(
    tokens: Sequence[Token], function: Function
) -> str | None:
    receivers = _sycl_receivers(tokens, function)
    body = tokens[function.body_open + 1 : function.body_close]
    for index in range(len(body) - 3):
        if body[index].kind != "identifier" or body[index].value not in receivers:
            continue
        if body[index + 1].value not in {".", "->"}:
            continue
        method = body[index + 2].value
        if method not in DISPATCH_METHODS:
            continue
        cursor = index + 3
        if cursor < len(body) and body[cursor].value == "<":
            depth = 1
            cursor += 1
            while cursor < len(body) and depth:
                if body[cursor].value == "<":
                    depth += 1
                elif body[cursor].value == ">":
                    depth -= 1
                elif body[cursor].value == ">>":
                    depth = max(0, depth - 2)
                cursor += 1
        if cursor < len(body) and body[cursor].value == "(":
            return f"typed SYCL {method} at line {body[index + 2].line}"
    return None


def _lifecycle_proof(tokens: Sequence[Token], function: Function) -> _Proof | None:
    contract = LIFECYCLE_CONTRACTS.get(function.name)
    if contract is None:
        return None
    values = [
        token.value for token in tokens[function.body_open + 1 : function.body_close]
    ]
    missing = [
        sequence
        for sequence in contract.required_sequences
        if not _contains_sequence(values, sequence)
    ]
    forbidden: list[str] = []
    if not contract.allow_host_loops and set(values) & {"for", "while", "do"}:
        forbidden.append("host loop")
    if _contains_sequence(values, ("pgaccel_record_gpu_exec", "(")):
        forbidden.append("GPU execution counter")
    if _contract_contains_dispatch(tokens, function) is not None:
        forbidden.append("device dispatch")
    if missing or forbidden:
        expected = " and ".join(" ".join(sequence) for sequence in missing)
        details: list[str] = []
        if expected:
            details.append(f"lacks required source evidence: {expected}")
        if forbidden:
            details.append("contains " + ", ".join(forbidden))
        return _Proof(
            False,
            f"lifecycle contract {contract.purpose!r} is invalid: {'; '.join(details)}",
            ("invalid_lifecycle_contract",),
        )
    return _Proof(
        True,
        f"explicit lifecycle contract: {contract.purpose}",
        ("lifecycle",),
    )


def _fail_only_contract_proof(
    tokens: Sequence[Token], function: Function
) -> _Proof | None:
    contract = FAIL_ONLY_CONTRACTS.get(function.name)
    if contract is None:
        return None
    values = [
        token.value for token in tokens[function.body_open + 1 : function.body_close]
    ]
    missing = [
        sequence
        for sequence in contract.required_sequences
        if not _contains_sequence(values, sequence)
    ]
    forbidden: list[str] = []
    if set(values) & {"for", "while", "do"}:
        forbidden.append("host loop")
    if _contains_sequence(values, ("pgaccel_record_gpu_exec", "(")):
        forbidden.append("GPU execution counter")
    if _contract_contains_dispatch(tokens, function) is not None:
        forbidden.append("device dispatch")
    if missing or forbidden:
        details: list[str] = []
        if missing:
            details.append(
                "missing " + " and ".join(" ".join(sequence) for sequence in missing)
            )
        if forbidden:
            details.append("contains " + ", ".join(forbidden))
        return _Proof(
            False,
            f"fail-only contract {contract.purpose!r} is invalid: {'; '.join(details)}",
            ("invalid_failure_only_contract",),
        )
    return _Proof(
        True,
        f"explicit fail-only contract: {contract.purpose}",
        ("failure_only",),
    )


@dataclasses.dataclass(frozen=True)
class _Region:
    key: str
    branch: str
    start: int
    end: int
    zero_work: bool = False
    definitely_inactive: bool = False


@dataclasses.dataclass(frozen=True)
class _DispatchEvidence:
    index: int
    line: int
    method: str
    detail: str
    context: tuple[tuple[str, str], ...]


@dataclasses.dataclass(frozen=True)
class _IndexedCall:
    call: Call
    index: int
    lparen: int
    rparen: int
    argument_values: tuple[str, ...]


def _is_inside(index: int, ranges: Sequence[tuple[int, int]]) -> bool:
    return any(start < index < end for start, end in ranges)


def _lambda_ranges(
    tokens: Sequence[Token],
    function: Function,
    forward: dict[int, int],
    reverse: dict[int, int],
) -> list[tuple[int, int]]:
    ranges: list[tuple[int, int]] = []
    for brace in range(function.body_open + 1, function.body_close):
        if tokens[brace].value != "{" or brace not in forward:
            continue
        cursor = brace - 1
        if cursor >= 0 and tokens[cursor].value == ")" and cursor in reverse:
            cursor = reverse[cursor] - 1
        if cursor >= 0 and tokens[cursor].value == "]":
            ranges.append((brace, forward[brace]))
    return ranges


def _statement_range(
    tokens: Sequence[Token], start: int, limit: int, forward: dict[int, int]
) -> tuple[int, int, int]:
    if start >= limit:
        return start, start, start
    if tokens[start].value == "{" and start in forward:
        close = forward[start]
        return start, close, close + 1
    depth = 0
    cursor = start
    while cursor < limit:
        value = tokens[cursor].value
        if value in {"(", "[", "{"}:
            depth += 1
        elif value in {
            ")",
            "]",
            "}",
        }:
            depth = max(0, depth - 1)
        elif value == ";" and depth == 0:
            return start - 1, cursor + 1, cursor + 1
        cursor += 1
    return start - 1, limit, limit


def _condition_is_zero(values: Sequence[str]) -> bool:
    compact = tuple(value for value in values if value not in {"(", ")"})
    if len(compact) == 2 and compact[0] == "!" and compact[1].isidentifier():
        return True
    if len(compact) != 3:
        return False
    left, operator, right = compact
    return operator == "==" and (
        (left.isidentifier() and right in {"0", "0u", "0U"})
        or (right.isidentifier() and left in {"0", "0u", "0U"})
    )


def _regions(
    tokens: Sequence[Token], function: Function, forward: dict[int, int]
) -> list[_Region]:
    regions: list[_Region] = []
    serial = 0
    cursor = function.body_open + 1
    while cursor < function.body_close:
        if tokens[cursor].value == "if" and cursor + 1 < function.body_close:
            constexpr = cursor + 1
            if tokens[constexpr].value == "constexpr":
                constexpr += 1
            if tokens[constexpr].value != "(" or constexpr not in forward:
                cursor += 1
                continue
            condition_close = forward[constexpr]
            condition = [
                token.value for token in tokens[constexpr + 1 : condition_close]
            ]
            start, end, after = _statement_range(
                tokens, condition_close + 1, function.body_close, forward
            )
            serial += 1
            key = f"if:{tokens[cursor].line}:{serial}"
            false_condition = condition in (["false"], ["0"])
            true_condition = condition in (["true"], ["1"])
            regions.append(
                _Region(
                    key,
                    "true",
                    start,
                    end,
                    _condition_is_zero(condition),
                    false_condition,
                )
            )
            if after < function.body_close and tokens[after].value == "else":
                false_start, false_end, _ = _statement_range(
                    tokens, after + 1, function.body_close, forward
                )
                regions.append(
                    _Region(
                        key,
                        "false",
                        false_start,
                        false_end,
                        False,
                        true_condition,
                    )
                )
        elif tokens[cursor].value == "catch":
            search = cursor + 1
            if (
                search < function.body_close
                and tokens[search].value == "("
                and search in forward
            ):
                search = forward[search] + 1
            start, end, _ = _statement_range(
                tokens, search, function.body_close, forward
            )
            serial += 1
            regions.append(
                _Region(f"catch:{tokens[cursor].line}:{serial}", "catch", start, end)
            )
        cursor += 1
    return regions


def _context(index: int, regions: Sequence[_Region]) -> tuple[tuple[str, str], ...]:
    return tuple(
        sorted(
            (region.key, region.branch)
            for region in regions
            if region.start <= index <= region.end
        )
    )


def _context_dominates(
    evidence: tuple[tuple[str, str], ...], success: tuple[tuple[str, str], ...]
) -> bool:
    success_map = dict(success)
    return all(success_map.get(key) == branch for key, branch in evidence)


def _parameter_names(
    tokens: Sequence[Token], function: Function
) -> tuple[set[str], set[str]]:
    forward, _ = _delimiter_pairs(tokens)
    parameters: set[str] = set()
    mutable: set[str] = set()
    start = function.lparen + 1
    segments: list[tuple[int, int]] = []
    cursor = start
    segment_start = start
    while cursor < function.rparen:
        if tokens[cursor].value in {"(", "[", "{"} and cursor in forward:
            cursor = forward[cursor] + 1
            continue
        if tokens[cursor].value == ",":
            segments.append((segment_start, cursor))
            segment_start = cursor + 1
        cursor += 1
    segments.append((segment_start, function.rparen))
    ignored = {
        "const",
        "volatile",
        "struct",
        "class",
        "typename",
        "void",
        "bool",
        "char",
        "short",
        "int",
        "long",
        "float",
        "double",
        "size_t",
        "uint8_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "int8_t",
        "int16_t",
        "int32_t",
        "int64_t",
    }
    for left, right in segments:
        values = [token.value for token in tokens[left:right]]
        identifiers = [
            token.value
            for token in tokens[left:right]
            if token.kind == "identifier" and token.value not in ignored
        ]
        if not identifiers:
            continue
        name = identifiers[-1]
        parameters.add(name)
        name_offset = max(
            index
            for index, token in enumerate(tokens[left:right])
            if token.value == name
        )
        before_name = values[:name_offset]
        if ("*" in before_name or "&" in before_name) and "const" not in before_name:
            mutable.add(name)
    return parameters, mutable


def _scope_for(index: int, brace_ranges: Sequence[tuple[int, int]]) -> tuple[int, int]:
    containing = [item for item in brace_ranges if item[0] < index < item[1]]
    return max(containing, key=lambda item: item[0]) if containing else (-1, 1 << 60)


def _declaration_kind(
    tokens: Sequence[Token], name_index: int, function: Function
) -> str | None:
    if name_index + 1 >= function.body_close or tokens[name_index + 1].value not in {
        "=",
        ";",
        ",",
        ")",
        "{",
    }:
        return None
    if tokens[name_index + 1].value in {",", ")"}:
        parameter_position = function.lparen < name_index < function.rparen
        if not parameter_position:
            depth = 0
            cursor = name_index - 1
            while cursor >= function.body_open:
                if tokens[cursor].value == ")":
                    depth += 1
                elif tokens[cursor].value == "(":
                    if depth == 0:
                        parameter_position = (
                            cursor > 0 and tokens[cursor - 1].value == "]"
                        )
                        break
                    depth -= 1
                cursor -= 1
        if not parameter_position:
            return None
    left = name_index - 1
    while left > function.signature_start and tokens[left].value not in {";", "{", "}"}:
        if tokens[left].value == "(":
            break
        left -= 1
    prefix = [token.value for token in tokens[left + 1 : name_index]]
    if not any(token.kind == "identifier" for token in tokens[left + 1 : name_index]):
        return None
    if _contains_sequence(prefix, ("sycl", "::", "queue")):
        return "queue"
    if _contains_sequence(prefix, ("sycl", "::", "handler")):
        return "handler"
    return "other"


def _receiver_kind(
    tokens: Sequence[Token],
    function: Function,
    receiver: str,
    call_index: int,
    forward: dict[int, int],
) -> str | None:
    brace_ranges = [
        (open_index, close_index)
        for open_index, close_index in forward.items()
        if tokens[open_index].value == "{"
    ]
    declarations: list[tuple[int, int, str]] = []
    for index in range(function.signature_start, call_index):
        if tokens[index].value != receiver:
            continue
        kind = _declaration_kind(tokens, index, function)
        if kind is None:
            continue
        scope = _scope_for(index, brace_ranges)
        if scope[0] < call_index < scope[1]:
            declarations.append((scope[0], index, kind))
    if not declarations:
        return None
    return max(declarations, key=lambda item: (item[0], item[1]))[2]


def _method_lparen(
    tokens: Sequence[Token], method_index: int, limit: int, forward: dict[int, int]
) -> tuple[int, int] | None:
    cursor = method_index + 1
    if cursor < limit and tokens[cursor].value == "<":
        depth = 1
        cursor += 1
        while cursor < limit and depth:
            if tokens[cursor].value == "<":
                depth += 1
            elif tokens[cursor].value == ">":
                depth -= 1
            elif tokens[cursor].value == ">>":
                depth = max(0, depth - 2)
            cursor += 1
    if cursor >= limit or tokens[cursor].value != "(" or cursor not in forward:
        return None
    return cursor, forward[cursor]


def _kernel_lambda(
    tokens: Sequence[Token],
    left: int,
    right: int,
    forward: dict[int, int],
    reverse: dict[int, int],
) -> tuple[int, int] | None:
    candidates: list[tuple[int, int]] = []
    for index in range(left + 1, right):
        if tokens[index].value != "{" or index not in forward:
            continue
        cursor = index - 1
        if tokens[cursor].value == ")" and cursor in reverse:
            cursor = reverse[cursor] - 1
        if cursor >= left and tokens[cursor].value == "]":
            candidates.append((index, forward[index]))
    return candidates[-1] if candidates else None


def _handler_lambda(
    tokens: Sequence[Token],
    left: int,
    right: int,
    forward: dict[int, int],
    reverse: dict[int, int],
) -> tuple[int, int] | None:
    candidates: list[tuple[int, int]] = []
    for index in range(left + 1, right):
        if tokens[index].value != "{" or index not in forward:
            continue
        cursor = index - 1
        if tokens[cursor].value == ")" and cursor in reverse:
            cursor = reverse[cursor] - 1
        if cursor >= left and tokens[cursor].value == "]":
            candidates.append((index, forward[index]))
    return candidates[0] if candidates else None


def _lambda_contributes(
    tokens: Sequence[Token], bounds: tuple[int, int], mutable: set[str]
) -> bool:
    left, right = bounds
    values = [token.value for token in tokens[left + 1 : right]]
    if not values or not mutable.intersection(values):
        return False
    assignment = {"=", "+=", "-=", "*=", "/=", "++", "--"}
    return bool(assignment.intersection(values))


def _lambda_nonempty(tokens: Sequence[Token], bounds: tuple[int, int]) -> bool:
    left, right = bounds
    return any(token.value != ";" for token in tokens[left + 1 : right])


def _direct_dispatches(
    tokens: Sequence[Token],
    function: Function,
    mutable: set[str],
    regions: Sequence[_Region],
) -> tuple[list[_DispatchEvidence], list[_DispatchEvidence], list[str]]:
    forward, reverse = _delimiter_pairs(tokens)
    lambdas = _lambda_ranges(tokens, function, forward, reverse)
    evidence: list[_DispatchEvidence] = []
    raw_launches: list[_DispatchEvidence] = []
    rejected: list[str] = []
    index = function.body_open + 1
    while index + 2 < function.body_close:
        if (
            tokens[index].kind != "identifier"
            or tokens[index + 1].value not in {".", "->"}
            or tokens[index + 2].value not in DISPATCH_METHODS
        ):
            index += 1
            continue
        receiver = tokens[index].value
        method = tokens[index + 2].value
        kind = _receiver_kind(tokens, function, receiver, index, forward)
        call = _method_lparen(tokens, index + 2, function.body_close, forward)
        if call is None:
            index += 1
            continue
        lparen, rparen = call
        enclosing_lambdas = [
            bounds for bounds in lambdas if bounds[0] < index < bounds[1]
        ]
        inactive = any(
            region.definitely_inactive and region.start <= index <= region.end
            for region in regions
        )
        if inactive:
            rejected.append(f"dead {method} at line {tokens[index + 2].line}")
            index = rparen + 1
            continue
        if method == "submit":
            if kind != "queue" or enclosing_lambdas:
                rejected.append(
                    f"untyped or deferred submit at line {tokens[index + 2].line}"
                )
                index = rparen + 1
                continue
            handler_lambda = _handler_lambda(tokens, lparen, rparen, forward, reverse)
            if handler_lambda is None:
                rejected.append(f"empty submit at line {tokens[index + 2].line}")
                index = rparen + 1
                continue
            inner_found = False
            raw_inner_found = False
            for inner in range(handler_lambda[0] + 1, handler_lambda[1] - 2):
                if (
                    tokens[inner].kind == "identifier"
                    and tokens[inner + 1].value in {".", "->"}
                    and tokens[inner + 2].value in {"parallel_for", "single_task"}
                    and _receiver_kind(
                        tokens, function, tokens[inner].value, inner, forward
                    )
                    == "handler"
                ):
                    inner_call = _method_lparen(
                        tokens, inner + 2, handler_lambda[1], forward
                    )
                    if inner_call is None:
                        continue
                    kernel = _kernel_lambda(
                        tokens, inner_call[0], inner_call[1], forward, reverse
                    )
                    if kernel is not None and _lambda_nonempty(tokens, kernel):
                        raw_inner_found = True
                        if _lambda_contributes(tokens, kernel, mutable):
                            inner_found = True
                            break
            if raw_inner_found:
                raw_launches.append(
                    _DispatchEvidence(
                        index,
                        tokens[index + 2].line,
                        method,
                        f"typed nonempty SYCL submit chain at line {tokens[index + 2].line}",
                        _context(index, regions),
                    )
                )
            if not inner_found:
                rejected.append(
                    f"submit without output-producing kernel at line {tokens[index + 2].line}"
                )
                index = rparen + 1
                continue
        else:
            if kind != "queue" or enclosing_lambdas:
                rejected.append(
                    f"untyped or deferred {method} at line {tokens[index + 2].line}"
                )
                index = rparen + 1
                continue
            kernel = _kernel_lambda(tokens, lparen, rparen, forward, reverse)
            if kernel is not None and _lambda_nonempty(tokens, kernel):
                raw_launches.append(
                    _DispatchEvidence(
                        index,
                        tokens[index + 2].line,
                        method,
                        f"typed nonempty SYCL {method} chain at line {tokens[index + 2].line}",
                        _context(index, regions),
                    )
                )
            if kernel is None or not _lambda_contributes(tokens, kernel, mutable):
                rejected.append(
                    f"{method} does not produce an ABI output at line {tokens[index + 2].line}"
                )
                index = rparen + 1
                continue
        evidence.append(
            _DispatchEvidence(
                index,
                tokens[index + 2].line,
                method,
                f"typed output-producing SYCL {method} at line {tokens[index + 2].line}",
                _context(index, regions),
            )
        )
        index = rparen + 1
    return evidence, raw_launches, rejected


def _indexed_calls(
    tokens: Sequence[Token],
    function: Function,
    lambda_ranges: Sequence[tuple[int, int]],
) -> list[_IndexedCall]:
    forward, _ = _delimiter_pairs(tokens)
    result: list[_IndexedCall] = []
    for index in range(function.body_open + 1, function.body_close):
        if _is_inside(index, lambda_ranges):
            continue
        token = tokens[index]
        if token.kind != "identifier" or token.value in CALL_KEYWORDS | NON_GRAPH_CALLS:
            continue
        if index > 0 and tokens[index - 1].value in {".", "->", "::"}:
            continue
        cursor = index + 1
        explicit = False
        if cursor < function.body_close and tokens[cursor].value == "<":
            depth = 1
            cursor += 1
            while cursor < function.body_close and depth:
                if tokens[cursor].value == "<":
                    depth += 1
                elif tokens[cursor].value == ">":
                    depth -= 1
                elif tokens[cursor].value == ">>":
                    depth = max(0, depth - 2)
                cursor += 1
            explicit = True
        if (
            cursor >= function.body_close
            or tokens[cursor].value != "("
            or cursor not in forward
        ):
            continue
        close = forward[cursor]
        result.append(
            _IndexedCall(
                Call(
                    token.value,
                    token.line,
                    _parameter_count(tokens, cursor, close),
                    explicit,
                ),
                index,
                cursor,
                close,
                tuple(item.value for item in tokens[cursor + 1 : close]),
            )
        )
    return result


def _returns(
    tokens: Sequence[Token],
    function: Function,
    lambda_ranges: Sequence[tuple[int, int]],
) -> list[tuple[int, list[Token]]]:
    result: list[tuple[int, list[Token]]] = []
    index = function.body_open + 1
    while index < function.body_close:
        if tokens[index].value != "return" or _is_inside(index, lambda_ranges):
            index += 1
            continue
        cursor = index + 1
        expression: list[Token] = []
        depth = 0
        while cursor < function.body_close:
            value = tokens[cursor].value
            if value in {"(", "[", "{"}:
                depth += 1
            elif value in {
                ")",
                "]",
                "}",
            }:
                depth = max(0, depth - 1)
            if value == ";" and depth == 0:
                break
            expression.append(tokens[cursor])
            cursor += 1
        result.append((index, expression))
        index = cursor + 1
    return result


def _host_output_accesses(
    tokens: Sequence[Token],
    function: Function,
    mutable: set[str],
    lambda_ranges: Sequence[tuple[int, int]],
) -> tuple[list[int], list[int]]:
    forward, _ = _delimiter_pairs(tokens)
    lines: set[int] = set()
    transfers: set[int] = set()
    assignments = {"=", "+=", "-=", "*=", "/=", "++", "--"}
    for index in range(function.body_open + 1, function.body_close):
        if tokens[index].value not in mutable or _is_inside(index, lambda_ranges):
            continue
        cursor = index + 1
        if (
            cursor < function.body_close
            and tokens[cursor].value == "["
            and cursor in forward
        ):
            cursor = forward[cursor] + 1
        elif cursor + 1 < function.body_close and tokens[cursor].value == "->":
            cursor += 2
        if cursor < function.body_close and tokens[cursor].value in assignments:
            lines.add(tokens[index].line)
        if (
            index > 0
            and tokens[index - 1].value == "*"
            and cursor < function.body_close
            and tokens[cursor].value in assignments
        ):
            lines.add(tokens[index].line)
        if (
            index > 1
            and tokens[index - 1].value == "("
            and tokens[index - 2].value in {"memcpy", "copy"}
        ):
            if index > 2 and tokens[index - 3].value in {".", "->"}:
                transfers.add(tokens[index].line)
            else:
                lines.add(tokens[index].line)
    return sorted(lines), sorted(transfers)


def _bounded_detail(parts: Iterable[str], limit: int = 6000) -> str:
    detail = "; ".join(dict.fromkeys(part for part in parts if part))
    if len(detail) <= limit:
        return detail
    return (
        detail[: limit - 64].rstrip() + "; [additional deterministic evidence omitted]"
    )


class _PathAuditor:
    """Conservative all-success-path verifier for ABI exports and helpers."""

    def __init__(
        self, path: pathlib.Path, tokens: Sequence[Token], functions: Sequence[Function]
    ):
        self.path = path
        self.tokens = tokens
        self.functions = functions
        self.forward, self.reverse = _delimiter_pairs(tokens)
        self.by_name: dict[str, list[Function]] = defaultdict(list)
        for function in functions:
            self.by_name[function.name].append(function)
        self.cache: dict[Function, _Proof] = {}

    def resolve(self, call: Call) -> tuple[Function | None, str | None]:
        candidates = list(self.by_name.get(call.name, ()))
        if call.explicit_template:
            templates = [candidate for candidate in candidates if candidate.is_template]
            if templates:
                candidates = templates
        if call.argument_count is not None:
            candidates = [
                candidate
                for candidate in candidates
                if candidate.parameter_count is None
                or candidate.parameter_count == call.argument_count
            ]
        if not candidates:
            return None, "unresolved_helper"
        if len(candidates) != 1:
            return None, "ambiguous_helper"
        return candidates[0], None

    def prove(self, function: Function, stack: tuple[Function, ...] = ()) -> _Proof:
        if function in self.cache:
            return self.cache[function]
        if function in stack:
            return _Proof(
                False,
                f"recursive helper cycle through {function.name} at line {function.line}",
                ("recursive_helper", "review_required"),
            )

        lifecycle = _lifecycle_proof(self.tokens, function)
        if lifecycle is not None:
            self.cache[function] = lifecycle
            return lifecycle
        fail_contract = _fail_only_contract_proof(self.tokens, function)
        if fail_contract is not None:
            self.cache[function] = fail_contract
            return fail_contract

        lambda_ranges = _lambda_ranges(
            self.tokens, function, self.forward, self.reverse
        )
        regions = _regions(self.tokens, function, self.forward)
        _, mutable = _parameter_names(self.tokens, function)
        direct, raw_launches, rejected = _direct_dispatches(
            self.tokens, function, mutable, regions
        )
        calls = _indexed_calls(self.tokens, function, lambda_ranges)
        classifications: set[str] = set()
        details: list[str] = []
        if direct:
            classifications.update({"device_dispatch", "large_input_gpu_chain"})
            details.extend(item.detail for item in direct)
        else:
            classifications.add("missing_device_terminal")
        if raw_launches:
            classifications.add("large_input_gpu_chain")
            details.extend(
                item.detail for item in raw_launches if item.detail not in details
            )
        if rejected:
            classifications.add("rejected_terminal")
            details.extend(rejected[:4])

        body = self.tokens[function.body_open + 1 : function.body_close]
        body_values = [token.value for token in body]
        counter_lines = _sequence_lines(body, ("pgaccel_record_gpu_exec", "("))
        if counter_lines:
            classifications.add("fake_gpu_counter")
            details.append(
                "GPU execution counter is observability only at line(s) "
                + ", ".join(map(str, counter_lines))
            )

        host_loop_lines = sorted(
            {
                token.line
                for index, token in enumerate(self.tokens)
                if function.body_open < index < function.body_close
                and token.value in {"for", "while", "do"}
                and not _is_inside(index, lambda_ranges)
            }
        )
        if host_loop_lines:
            classifications.update({"host_computation", "review_required"})
            details.append(
                "host loop(s) at line(s) " + ", ".join(map(str, host_loop_lines))
            )

        output_write_lines, transfer_lines = _host_output_accesses(
            self.tokens, function, mutable, lambda_ranges
        )
        zero_ranges = [region for region in regions if region.zero_work]
        unsafe_write_lines = [
            line
            for line in output_write_lines
            if not any(
                region.start
                <= next(
                    (
                        index
                        for index in range(region.start, region.end + 1)
                        if self.tokens[index].line == line
                    ),
                    region.end + 1,
                )
                <= region.end
                for region in zero_ranges
            )
        ]
        if unsafe_write_lines:
            classifications.update({"host_output_write", "host_computation"})
            details.append(
                "host writes ABI output at line(s) "
                + ", ".join(map(str, unsafe_write_lines))
            )
        if transfer_lines:
            classifications.update({"host_staging_review", "review_required"})
            details.append(
                "output transfer provenance requires review at line(s) "
                + ", ".join(map(str, transfer_lines))
            )

        if "if" in body_values and "constexpr" in body_values:
            classifications.update(
                {"template_specialization_review", "review_required"}
            )
            details.append("if constexpr paths require a concrete specialization proof")

        # A successful helper call used as a statement can establish evidence
        # only when it receives one of this function's mutable ABI outputs.
        call_evidence: list[_DispatchEvidence] = []
        unsafe_contributing_helpers: list[str] = []
        call_proofs: dict[int, tuple[_IndexedCall, _Proof | None, str | None]] = {}
        for indexed in calls:
            candidate, error = self.resolve(indexed.call)
            proof = (
                self.prove(candidate, stack + (function,))
                if candidate is not None
                else None
            )
            call_proofs[indexed.index] = (indexed, proof, error)
            if (
                proof is not None
                and "large_input_gpu_chain" in proof.classifications
                and mutable.intersection(indexed.argument_values)
            ):
                classifications.add("large_input_gpu_chain")
                details.append(
                    f"large-input chain {function.name} -> {candidate.name} at line "
                    f"{indexed.call.line}: typed SYCL launch observed; result proof remains "
                    "independently fail-closed"
                )
            if (
                proof is not None
                and not proof.ok
                and mutable.intersection(indexed.argument_values)
            ):
                classifications.update(proof.classifications)
                unsafe_contributing_helpers.append(
                    f"output helper {candidate.name} at line {indexed.call.line} is not "
                    "independently device-proven"
                )
            if (
                proof is not None
                and proof.ok
                and mutable.intersection(indexed.argument_values)
            ):
                call_evidence.append(
                    _DispatchEvidence(
                        indexed.index,
                        indexed.call.line,
                        "helper",
                        f"{function.name} -> {candidate.name} at line {indexed.call.line}: {proof.detail}",
                        _context(indexed.index, regions),
                    )
                )

        all_evidence = sorted(direct + call_evidence, key=lambda item: item.index)
        success_paths = 0
        unsafe_success: list[str] = []
        returns = _returns(self.tokens, function, lambda_ranges)
        for return_index, expression in returns:
            values = [token.value for token in expression]
            if values and values[0] in FAILURE_STATUSES:
                continue
            if values and values[0] == "pgaccel_kernel_failure":
                continue
            if not function.is_status and values in (["nullptr"], ["NULL"], ["0"]):
                continue
            success_paths += 1
            return_context = _context(return_index, regions)
            if any(key.startswith("catch:") for key, _ in return_context):
                unsafe_success.append(
                    f"CPU-success catch at line {self.tokens[return_index].line}"
                )
                continue
            zero = any(
                region.zero_work and region.start <= return_index <= region.end
                for region in zero_ranges
            )
            if zero and not host_loop_lines and not counter_lines:
                classifications.add("zero_work")
                continue

            returned_call = next(
                (
                    record
                    for index, record in call_proofs.items()
                    if return_index < index
                    and index < return_index + len(expression) + 2
                ),
                None,
            )
            if returned_call is not None:
                indexed, proof, error = returned_call
                if proof is not None and proof.ok:
                    classifications.update(proof.classifications)
                    details.append(
                        f"returned helper chain {function.name} -> {indexed.call.name}: {proof.detail}"
                    )
                    continue
                if proof is not None:
                    classifications.update(proof.classifications)
                    unsafe_success.append(
                        f"returned helper {indexed.call.name} at line {indexed.call.line} is unsafe: {proof.detail}"
                    )
                else:
                    classifications.add(error or "unresolved_helper")
                    unsafe_success.append(
                        f"{error or 'unresolved helper'} {indexed.call.name} at line {indexed.call.line}"
                    )
                continue

            dominating = [
                item
                for item in all_evidence
                if item.index < return_index
                and _context_dominates(item.context, return_context)
            ]
            if not dominating:
                unsafe_success.append(
                    f"success at line {self.tokens[return_index].line} is not dominated by output-producing SYCL work"
                )

        if function.is_status and returns and success_paths == 0:
            proof = _Proof(
                True,
                "all reachable returns are explicit failure statuses",
                ("failure_only",),
            )
            self.cache[function] = proof
            return proof

        if (
            not function.is_status
            and function.return_spelling == "void"
            and not returns
        ):
            classifications.update({"unclassified_nonstatus_export", "review_required"})
            unsafe_success.append("void export lacks an explicit lifecycle contract")
        elif not returns:
            classifications.update({"missing_return_analysis", "review_required"})
            unsafe_success.append("no auditable terminal return")

        hard_failure = bool(
            unsafe_success
            or host_loop_lines
            or unsafe_write_lines
            or transfer_lines
            or unsafe_contributing_helpers
            or "template_specialization_review" in classifications
        )
        if hard_failure:
            classifications.update({"undominated_success", "review_required"})
            details.extend(unsafe_contributing_helpers)
            details.extend(unsafe_success)
            proof = _Proof(
                False,
                _bounded_detail(details) or "success path is not device-proven",
                tuple(sorted(classifications)),
            )
        elif success_paths and all_evidence:
            details.extend(
                item.detail for item in all_evidence if item.detail not in details
            )
            classifications.discard("missing_device_terminal")
            proof = _Proof(
                True,
                _bounded_detail(details),
                tuple(sorted(classifications or {"device_dispatch"})),
            )
        else:
            classifications.update({"missing_device_terminal", "review_required"})
            details.extend(unsafe_success)
            proof = _Proof(
                False,
                _bounded_detail(details) or "no output-producing SYCL terminal",
                tuple(sorted(classifications)),
            )
        self.cache[function] = proof
        return proof


def audit_source(
    path: pathlib.Path, source: str, *, require_entrypoint: bool = True
) -> FileAudit:
    try:
        normalized_source, directives = normalize_preprocessor(source)
        tokens = lex_cpp(normalized_source)
        functions = parse_functions(tokens)
    except ParseError as error:
        finding = Finding(path, 1, "<parser>", str(error), ("parser_error",))
        return FileAudit(path, 0, 0, 0, (), (finding,))

    entrypoints = [function for function in functions if function.is_export]
    findings: list[Finding] = []
    for directive in directives:
        if re.match(r"\s*#\s*define\b", directive.text) and (
            "pgaccel_" in directive.text
            or re.search(r"extern\s*(?:\\\"C\\\"|\"C\")", directive.text)
        ):
            findings.append(
                Finding(
                    path,
                    directive.line,
                    "<preprocessor>",
                    "export-bearing macro cannot be proven against the source ABI inventory",
                    ("macro_hidden_export", "review_required"),
                )
            )
    if require_entrypoint and not entrypoints:
        findings.append(
            Finding(
                path,
                1,
                "<inventory>",
                'no extern "C" pgaccel_* export definitions found',
                ("empty_inventory",),
            )
        )

    duplicate_entries = {
        name: definitions
        for name, definitions in _group_by_name(entrypoints).items()
        if len(definitions) > 1
    }
    for name, definitions in sorted(duplicate_entries.items()):
        locations = ", ".join(str(function.line) for function in definitions)
        findings.append(
            Finding(
                path,
                definitions[0].line,
                name,
                f"ambiguous duplicate entrypoint definitions at lines {locations}",
                ("ambiguous_entrypoint",),
            )
        )

    auditor = _PathAuditor(path, tokens, functions)
    entrypoint_audits: list[EntrypointAudit] = []
    for entrypoint in sorted(entrypoints, key=lambda function: function.line):
        if entrypoint.name in duplicate_entries:
            entrypoint_audits.append(
                EntrypointAudit(
                    path,
                    entrypoint.line,
                    entrypoint.name,
                    False,
                    ("ambiguous_entrypoint",),
                    "duplicate entrypoint definition",
                    entrypoint.is_status,
                    entrypoint.return_spelling,
                )
            )
            continue
        proof = auditor.prove(entrypoint)
        entrypoint_audits.append(
            EntrypointAudit(
                path,
                entrypoint.line,
                entrypoint.name,
                proof.ok,
                proof.classifications,
                proof.detail,
                entrypoint.is_status,
                entrypoint.return_spelling,
            )
        )
        if not proof.ok:
            findings.append(
                Finding(
                    path,
                    entrypoint.line,
                    entrypoint.name,
                    proof.detail,
                    proof.classifications,
                )
            )

    lifecycle_count = sum(
        1
        for entrypoint_audit in entrypoint_audits
        if entrypoint_audit.ok and "lifecycle" in entrypoint_audit.classifications
    )
    return FileAudit(
        path=path,
        definitions=len(functions),
        entrypoints=len(entrypoints),
        lifecycle_contracts=lifecycle_count,
        entrypoint_audits=tuple(entrypoint_audits),
        findings=tuple(findings),
        status_entrypoints=sum(entrypoint.is_status for entrypoint in entrypoints),
        non_status_entrypoints=sum(
            not entrypoint.is_status for entrypoint in entrypoints
        ),
    )


def _group_by_name(functions: Iterable[Function]) -> dict[str, list[Function]]:
    result: dict[str, list[Function]] = defaultdict(list)
    for function in functions:
        result[function.name].append(function)
    return result


def audit_paths(paths: Sequence[pathlib.Path]) -> list[FileAudit]:
    audits: list[FileAudit] = []
    for path in paths:
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            finding = Finding(path, 1, "<read>", str(error), ("read_error",))
            audits.append(FileAudit(path, 0, 0, 0, (), (finding,)))
            continue
        audits.append(audit_source(path, source, require_entrypoint=False))
    if audits and not any(audit.entrypoints for audit in audits):
        first = audits[0]
        finding = Finding(
            first.path,
            1,
            "<inventory>",
            'no extern "C" pgaccel_* export definitions found in the audited source set',
            ("empty_inventory",),
        )
        audits[0] = dataclasses.replace(first, findings=first.findings + (finding,))
    return audits


def _enclosing_brace(tokens: Sequence[Token], target: int) -> int | None:
    stack: list[int] = []
    for index, token in enumerate(tokens[:target]):
        if token.value == "{":
            stack.append(index)
        elif token.value == "}" and stack:
            stack.pop()
    return stack[-1] if stack else None


def _declaration_return_spelling(
    tokens: Sequence[Token],
    signature_start: int,
    name_index: int,
    rparen: int,
    semicolon: int,
) -> str:
    suffix = tokens[rparen + 1 : semicolon]
    if any(token.value == "->" for token in suffix):
        arrow = next(index for index, token in enumerate(suffix) if token.value == "->")
        result = suffix[arrow + 1 :]
    else:
        result = [
            token
            for token in tokens[signature_start:name_index]
            if token.value
            not in {"extern", "C", "static", "inline", "constexpr", "PGACCEL_EXPORT"}
            and token.kind != "string_c"
        ]
    return " ".join(token.value for token in result).strip() or "<unknown>"


def parse_declarations(
    path: pathlib.Path, source: str
) -> tuple[list[AbiSymbol], list[Finding]]:
    normalized, directives = normalize_preprocessor(source)
    tokens = lex_cpp(normalized)
    forward, _ = _delimiter_pairs(tokens)
    extern_braces = _extern_c_braces(tokens)
    parents = _brace_parents(tokens)
    symbols: list[AbiSymbol] = []
    findings: list[Finding] = []
    for directive in directives:
        if re.match(r"\s*#\s*define\b", directive.text) and (
            "pgaccel_" in directive.text
            or re.search(r"extern\s*(?:\\\"C\\\"|\"C\")", directive.text)
        ):
            findings.append(
                Finding(
                    path,
                    directive.line,
                    "<preprocessor>",
                    "export-bearing header macro is absent from the parsed ABI declaration inventory",
                    ("macro_hidden_export", "abi_inventory_mismatch"),
                )
            )
    for name_index, token in enumerate(tokens):
        if token.kind != "identifier" or not token.value.startswith("pgaccel_"):
            continue
        lparen = name_index + 1
        if (
            lparen >= len(tokens)
            or tokens[lparen].value != "("
            or lparen not in forward
        ):
            continue
        rparen = forward[lparen]
        cursor = rparen + 1
        while cursor < len(tokens) and tokens[cursor].value not in {";", "{"}:
            cursor += 1
        if cursor >= len(tokens) or tokens[cursor].value != ";":
            continue
        signature_start = _signature_start(tokens, name_index)
        direct = any(
            item.value == "extern" for item in tokens[signature_start:name_index]
        ) and any(
            item.kind == "string_c" for item in tokens[signature_start:name_index]
        )
        parent = _enclosing_brace(tokens, name_index)
        linked = direct
        while parent is not None and not linked:
            linked = parent in extern_braces
            parent = parents.get(parent)
        if not linked:
            continue
        symbols.append(
            AbiSymbol(
                path,
                token.line,
                token.value,
                _declaration_return_spelling(
                    tokens, signature_start, name_index, rparen, cursor
                ),
                _parameter_count(tokens, lparen, rparen),
                "header_declaration",
            )
        )
    return symbols, findings


def _definition_symbols(
    path: pathlib.Path, source: str, *, normalized: bool = True
) -> list[AbiSymbol]:
    parsed_source = normalize_preprocessor(source)[0] if normalized else source
    tokens = lex_cpp(parsed_source)
    return [
        AbiSymbol(
            path,
            function.line,
            function.name,
            function.return_spelling,
            function.parameter_count,
            "source_definition",
        )
        for function in parse_functions(tokens)
        if function.is_export
    ]


def _canonical_symbol(symbol: AbiSymbol) -> tuple[str, str, int | None]:
    return symbol.name, " ".join(symbol.return_spelling.split()), symbol.parameter_count


def _inventory_hash(symbols: Iterable[AbiSymbol]) -> str:
    rows = sorted(_canonical_symbol(symbol) for symbol in symbols)
    encoded = json.dumps(rows, separators=(",", ":"), ensure_ascii=True).encode("ascii")
    return hashlib.sha256(encoded).hexdigest()


def audit_abi(
    source_paths: Sequence[pathlib.Path], header_paths: Sequence[pathlib.Path]
) -> AbiInventory:
    definitions: list[AbiSymbol] = []
    source_definitions: list[AbiSymbol] = []
    declarations: list[AbiSymbol] = []
    findings: list[Finding] = []
    per_file: list[dict[str, object]] = []
    for path in sorted(dict.fromkeys(source_paths), key=lambda item: item.as_posix()):
        try:
            source = path.read_text(encoding="utf-8")
            symbols = _definition_symbols(path, source)
            raw_symbols = _definition_symbols(path, source, normalized=False)
            _, directives = normalize_preprocessor(source)
        except (OSError, UnicodeError, ParseError) as error:
            findings.append(
                Finding(path, 1, "<inventory>", str(error), ("abi_inventory_error",))
            )
            continue
        definitions.extend(symbols)
        source_definitions.extend(raw_symbols)
        normalized_rows = {_canonical_symbol(symbol) for symbol in symbols}
        raw_rows = {_canonical_symbol(symbol) for symbol in raw_symbols}
        if normalized_rows != raw_rows:
            locations = {
                _canonical_symbol(symbol): symbol for symbol in (*symbols, *raw_symbols)
            }
            for row in sorted(normalized_rows ^ raw_rows):
                symbol = locations[row]
                findings.append(
                    Finding(
                        symbol.path,
                        symbol.line,
                        symbol.name,
                        "source and normalized-preprocessor definition inventories disagree",
                        ("preprocessor_inventory_mismatch", "abi_inventory_mismatch"),
                    )
                )
        for directive in directives:
            if re.match(r"\s*#\s*define\b", directive.text) and (
                "pgaccel_" in directive.text
                or re.search(r"extern\s*(?:\\\"C\\\"|\"C\")", directive.text)
            ):
                findings.append(
                    Finding(
                        path,
                        directive.line,
                        "<preprocessor>",
                        "macro-hidden source export cannot be reconciled with definitions",
                        ("macro_hidden_export", "abi_inventory_mismatch"),
                    )
                )
        per_file.append(
            {
                "path": path.as_posix(),
                "kind": "definitions",
                "count": len(symbols),
                "inventory_hash": _inventory_hash(symbols),
                "source_inventory_hash": _inventory_hash(raw_symbols),
                "symbols": [
                    symbol.name
                    for symbol in sorted(
                        symbols, key=lambda item: (item.name, item.line)
                    )
                ],
            }
        )
    for path in sorted(dict.fromkeys(header_paths), key=lambda item: item.as_posix()):
        try:
            symbols, header_findings = parse_declarations(
                path, path.read_text(encoding="utf-8")
            )
        except (OSError, UnicodeError, ParseError) as error:
            findings.append(
                Finding(path, 1, "<inventory>", str(error), ("abi_inventory_error",))
            )
            continue
        declarations.extend(symbols)
        findings.extend(header_findings)
        per_file.append(
            {
                "path": path.as_posix(),
                "kind": "declarations",
                "count": len(symbols),
                "inventory_hash": _inventory_hash(symbols),
                "symbols": [
                    symbol.name
                    for symbol in sorted(
                        symbols, key=lambda item: (item.name, item.line)
                    )
                ],
            }
        )

    definition_names = {symbol.name for symbol in definitions}
    declaration_names = {symbol.name for symbol in declarations}
    definitions_by_name: dict[str, list[AbiSymbol]] = defaultdict(list)
    declarations_by_name: dict[str, list[AbiSymbol]] = defaultdict(list)
    for symbol in definitions:
        definitions_by_name[symbol.name].append(symbol)
    for symbol in declarations:
        declarations_by_name[symbol.name].append(symbol)
    for name in sorted(definition_names - declaration_names):
        symbol = definitions_by_name[name][0]
        findings.append(
            Finding(
                symbol.path,
                symbol.line,
                name,
                "extern-C definition has no matching public header declaration",
                ("extra_abi_definition", "abi_inventory_mismatch"),
            )
        )
    for name in sorted(declaration_names - definition_names):
        symbol = declarations_by_name[name][0]
        findings.append(
            Finding(
                symbol.path,
                symbol.line,
                name,
                "public extern-C declaration has no source definition",
                ("missing_abi_definition", "abi_inventory_mismatch"),
            )
        )
    for name in sorted(definition_names & declaration_names):
        definition_signatures = {
            (" ".join(symbol.return_spelling.split()), symbol.parameter_count)
            for symbol in definitions_by_name[name]
        }
        declaration_signatures = {
            (" ".join(symbol.return_spelling.split()), symbol.parameter_count)
            for symbol in declarations_by_name[name]
        }
        if definition_signatures.isdisjoint(declaration_signatures):
            symbol = definitions_by_name[name][0]
            findings.append(
                Finding(
                    symbol.path,
                    symbol.line,
                    name,
                    f"definition signature {sorted(definition_signatures)!r} does not match header {sorted(declaration_signatures)!r}",
                    ("abi_signature_mismatch", "abi_inventory_mismatch"),
                )
            )
    return AbiInventory(
        tuple(
            sorted(
                definitions,
                key=lambda item: (item.path.as_posix(), item.line, item.name),
            )
        ),
        tuple(
            sorted(
                declarations,
                key=lambda item: (item.path.as_posix(), item.line, item.name),
            )
        ),
        tuple(
            sorted(
                findings,
                key=lambda item: (
                    item.path.as_posix(),
                    item.line,
                    item.entrypoint,
                    item.message,
                ),
            )
        ),
        tuple(
            sorted(per_file, key=lambda item: (str(item["path"]), str(item["kind"])))
        ),
        _inventory_hash(definitions),
        _inventory_hash(source_definitions),
        _inventory_hash(declarations),
    )


def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--json-report",
        type=pathlib.Path,
        help="write the complete entrypoint inventory and findings as JSON",
    )
    parser.add_argument(
        "--headers",
        nargs="*",
        default=(),
        type=pathlib.Path,
        help="public C/C++ headers whose extern-C declarations form the ABI baseline",
    )
    parser.add_argument(
        "sources", nargs="+", type=pathlib.Path, help="C++ source files to audit"
    )
    return parser.parse_args(argv)


def _write_json_report(
    path: pathlib.Path, audits: Sequence[FileAudit], abi: AbiInventory | None = None
) -> None:
    findings = [finding for audit in audits for finding in audit.findings]
    if abi is not None:
        findings.extend(abi.findings)
    entrypoint_audits = [entry for audit in audits for entry in audit.entrypoint_audits]
    classification_counts = Counter(
        classification
        for finding in findings
        for classification in finding.classifications
    )
    payload = {
        "schema_version": 2,
        "status": "fail" if findings else "pass",
        "summary": {
            "files": len(audits),
            "definitions": sum(audit.definitions for audit in audits),
            "entrypoints": sum(audit.entrypoints for audit in audits),
            "status_entrypoints": sum(audit.status_entrypoints for audit in audits),
            "non_status_entrypoints": sum(
                audit.non_status_entrypoints for audit in audits
            ),
            "entrypoints_passed": sum(entry.ok for entry in entrypoint_audits),
            "entrypoints_failed": sum(not entry.ok for entry in entrypoint_audits),
            "status_entrypoints_failed": sum(
                entry.is_status and not entry.ok for entry in entrypoint_audits
            ),
            "non_status_entrypoints_failed": sum(
                not entry.is_status and not entry.ok for entry in entrypoint_audits
            ),
            "lifecycle_contracts": sum(audit.lifecycle_contracts for audit in audits),
            "findings": len(findings),
            "classification_counts": dict(sorted(classification_counts.items())),
        },
        "entrypoints": [
            {
                "path": str(entry.path),
                "line": entry.line,
                "name": entry.entrypoint,
                "return_type": entry.return_type,
                "status": "pass" if entry.ok else "fail",
                "classifications": list(entry.classifications),
                "detail": entry.detail,
            }
            for entry in entrypoint_audits
        ],
        "abi_inventory": (
            {
                "definition_count": len(abi.definitions),
                "unique_definition_count": len(
                    {symbol.name for symbol in abi.definitions}
                ),
                "declaration_count": len(abi.declarations),
                "unique_declaration_count": len(
                    {symbol.name for symbol in abi.declarations}
                ),
                "definition_hash": abi.definition_hash,
                "source_definition_hash": abi.source_definition_hash,
                "declaration_hash": abi.declaration_hash,
                "per_file": list(abi.per_file),
                "definitions": [
                    {
                        "path": symbol.path.as_posix(),
                        "line": symbol.line,
                        "name": symbol.name,
                        "return_type": symbol.return_spelling,
                        "parameter_count": symbol.parameter_count,
                    }
                    for symbol in abi.definitions
                ],
                "declarations": [
                    {
                        "path": symbol.path.as_posix(),
                        "line": symbol.line,
                        "name": symbol.name,
                        "return_type": symbol.return_spelling,
                        "parameter_count": symbol.parameter_count,
                    }
                    for symbol in abi.declarations
                ],
            }
            if abi is not None
            else None
        ),
        "findings": [
            {
                "path": str(finding.path),
                "line": finding.line,
                "entrypoint": finding.entrypoint,
                "classifications": list(finding.classifications),
                "message": finding.message,
            }
            for finding in findings
        ],
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def main(argv: Sequence[str] | None = None) -> int:
    args = _parse_args(sys.argv[1:] if argv is None else argv)
    unique_paths = list(dict.fromkeys(args.sources))
    audits = audit_paths(unique_paths)
    findings = [finding for audit in audits for finding in audit.findings]
    abi = audit_abi(unique_paths, args.headers) if args.headers else None
    if abi is not None:
        findings.extend(abi.findings)
    if args.json_report is not None:
        _write_json_report(args.json_report, audits, abi)
    for finding in findings:
        classifications = ",".join(finding.classifications) or "unclassified"
        print(
            f"{finding.path}:{finding.line}: {finding.entrypoint}: "
            f"[{classifications}] {finding.message}",
            file=sys.stderr,
        )

    entrypoints = sum(audit.entrypoints for audit in audits)
    status_entrypoints = sum(audit.status_entrypoints for audit in audits)
    non_status_entrypoints = sum(audit.non_status_entrypoints for audit in audits)
    definitions = sum(audit.definitions for audit in audits)
    contracts = sum(audit.lifecycle_contracts for audit in audits)
    if findings:
        print(
            "audit-cpu-cheats: FAIL - "
            f"{len(findings)} finding(s) across {entrypoints} ABI exports "
            f"({status_entrypoints} status, {non_status_entrypoints} non-status); "
            "every successful compute path must be output-producing device work.",
            file=sys.stderr,
        )
        return 1

    print(
        "audit-cpu-cheats: PASS - "
        f"{entrypoints} extern-C exports, {definitions} local definitions, "
        f"{contracts} explicit lifecycle contracts."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
