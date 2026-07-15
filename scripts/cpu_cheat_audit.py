#!/usr/bin/env python3
"""Fail-closed structural audit for GPU kernel C entrypoints.

This is intentionally a small C++ lexer and structural parser rather than an
awk/grep check.  It does not attempt C++ type checking.  It does, however,
remove comments, strings, characters, and preprocessor directives before it:

* finds every ``extern "C" pgaccel_status pgaccel_*`` definition, including
  definitions inside an extern-C linkage block;
* recognizes balanced multiline function bodies and function-try-blocks;
* follows locally defined helper and template calls; and
* accepts only a typed SYCL launch, a narrowly enumerated lifecycle contract,
  or a function whose every return is an explicit failure status.

The audit is conservative.  An unresolved, recursive, or overload-ambiguous
call chain is not proof that device work occurs and therefore fails.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import pathlib
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


@dataclasses.dataclass(frozen=True)
class FileAudit:
    path: pathlib.Path
    definitions: int
    entrypoints: int
    lifecycle_contracts: int
    entrypoint_audits: tuple[EntrypointAudit, ...]
    findings: tuple[Finding, ...]


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
        is_entrypoint = (
            name.startswith("pgaccel_")
            and any(token.value == "pgaccel_status" for token in prefix)
            and _inside_extern_c(
                body_open,
                tokens,
                parents,
                extern_braces,
                signature_start,
                name_index,
            )
        )
        candidates.append(
            Function(
                name=name,
                line=tokens[name_index].line,
                signature_start=signature_start,
                name_index=name_index,
                lparen=lparen,
                rparen=rparen,
                body_open=body_open,
                body_close=body_close,
                parameter_count=_parameter_count(tokens, lparen, rparen),
                is_template=any(token.value == "template" for token in prefix),
                is_entrypoint=is_entrypoint,
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


def _direct_dispatch(tokens: Sequence[Token], function: Function) -> str | None:
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


def _call_argument_count(
    tokens: Sequence[Token], lparen: int, rparen: int
) -> int | None:
    return _parameter_count(tokens, lparen, rparen)


def _calls(
    tokens: Sequence[Token], function: Function, forward: dict[int, int]
) -> list[Call]:
    calls: list[Call] = []
    index = function.body_open + 1
    while index < function.body_close:
        token = tokens[index]
        if token.kind != "identifier" or token.value in CALL_KEYWORDS | NON_GRAPH_CALLS:
            index += 1
            continue
        if index > function.body_open + 1 and tokens[index - 1].value in {
            ".",
            "->",
            "::",
        }:
            index += 1
            continue

        cursor = index + 1
        explicit_template = False
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
            if depth:
                index += 1
                continue
            explicit_template = True
        if cursor >= function.body_close or tokens[cursor].value != "(":
            index += 1
            continue
        rparen = forward.get(cursor)
        if rparen is None or rparen > function.body_close:
            index += 1
            continue
        calls.append(
            Call(
                name=token.value,
                line=token.line,
                argument_count=_call_argument_count(tokens, cursor, rparen),
                explicit_template=explicit_template,
            )
        )
        index += 1
    return calls


def _fail_only(tokens: Sequence[Token], function: Function) -> bool:
    body = tokens[function.body_open + 1 : function.body_close]
    returns = [index for index, token in enumerate(body) if token.value == "return"]
    if not returns:
        return False
    for index in returns:
        cursor = index + 1
        if cursor >= len(body) or body[cursor].value == ";":
            return False
        value = body[cursor].value
        if value in FAILURE_STATUSES:
            continue
        if (
            value == "pgaccel_kernel_failure"
            and cursor + 1 < len(body)
            and body[cursor + 1].value == "("
        ):
            continue
        return False
    return True


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
    if _direct_dispatch(tokens, function) is not None:
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
    if _direct_dispatch(tokens, function) is not None:
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


class _GraphAuditor:
    def __init__(
        self, path: pathlib.Path, tokens: Sequence[Token], functions: Sequence[Function]
    ):
        self.path = path
        self.tokens = tokens
        self.functions = functions
        self.forward, _ = _delimiter_pairs(tokens)
        self.by_name: dict[str, list[Function]] = defaultdict(list)
        for function in functions:
            self.by_name[function.name].append(function)
        self.call_cache: dict[Function, list[Call]] = {}
        self.proof_cache: dict[Function, _Proof] = {}

    def calls(self, function: Function) -> list[Call]:
        if function not in self.call_cache:
            self.call_cache[function] = _calls(self.tokens, function, self.forward)
        return self.call_cache[function]

    def resolve(self, call: Call) -> tuple[list[Function], str | None]:
        candidates = list(self.by_name.get(call.name, ()))
        if not candidates:
            return [], "unresolved"
        if call.explicit_template:
            templates = [candidate for candidate in candidates if candidate.is_template]
            if templates:
                candidates = templates
        if call.argument_count is not None:
            same_arity = [
                candidate
                for candidate in candidates
                if candidate.parameter_count is None
                or candidate.parameter_count == call.argument_count
            ]
            if same_arity:
                candidates = same_arity
        if len(candidates) > 1:
            locations = ", ".join(str(candidate.line) for candidate in candidates[:4])
            return candidates, f"ambiguous local definitions at lines {locations}"
        return candidates, None

    def prove(self, function: Function, stack: tuple[Function, ...] = ()) -> _Proof:
        if function in self.proof_cache:
            return self.proof_cache[function]
        if function in stack:
            start = stack.index(function)
            cycle = " -> ".join(
                f"{item.name} (line {item.line})"
                for item in stack[start:] + (function,)
            )
            return _Proof(
                False,
                f"recursive helper cycle: {cycle}",
                ("missing_device_terminal", "recursive_helper"),
            )

        lifecycle = _lifecycle_proof(self.tokens, function)
        if lifecycle is not None:
            self.proof_cache[function] = lifecycle
            return lifecycle

        fail_only_contract = _fail_only_contract_proof(self.tokens, function)
        if fail_only_contract is not None:
            self.proof_cache[function] = fail_only_contract
            return fail_only_contract

        dispatch = _direct_dispatch(self.tokens, function)
        if dispatch is not None:
            proof = _Proof(True, dispatch, ("device_dispatch",))
            self.proof_cache[function] = proof
            return proof

        if _fail_only(self.tokens, function):
            proof = _Proof(
                True,
                "all returns are explicit failure statuses",
                ("failure_only",),
            )
            self.proof_cache[function] = proof
            return proof

        unresolved: list[str] = []
        ambiguous: list[str] = []
        failed_paths: list[str] = []
        for call in self.calls(function):
            candidates, resolution_error = self.resolve(call)
            if resolution_error == "unresolved":
                unresolved.append(f"{call.name} (line {call.line})")
                continue
            if resolution_error is not None:
                ambiguous.append(f"{call.name} (line {call.line}: {resolution_error})")
                continue
            candidate = candidates[0]
            proof = self.prove(candidate, stack + (function,))
            if proof.ok:
                result = _Proof(
                    True,
                    f"{function.name} -> {candidate.name} "
                    f"(defined line {candidate.line}, called line {call.line}) -> {proof.detail}",
                    proof.classifications,
                )
                self.proof_cache[function] = result
                return result
            failed_paths.append(
                f"{candidate.name} (defined line {candidate.line}, called line {call.line}): "
                f"{proof.detail}"
            )

        body_tokens = self.tokens[function.body_open + 1 : function.body_close]
        body_values = {token.value for token in body_tokens}
        body_sequence = [token.value for token in body_tokens]
        host_loop_lines = sorted(
            {
                token.line
                for token in body_tokens
                if token.value in {"for", "while", "do"}
            }
        )
        fake_counter_lines = _sequence_lines(
            body_tokens, ("pgaccel_record_gpu_exec", "(")
        )
        prefix = (
            "host loop has no device-dispatch terminal"
            if body_values & {"for", "while", "do"}
            else "no device-dispatch terminal"
        )
        classifications = {"missing_device_terminal"}
        if body_values & {"for", "while", "do"}:
            classifications.add("host_computation")
        if _contains_sequence(body_sequence, ("pgaccel_record_gpu_exec", "(")):
            classifications.add("fake_gpu_counter")
        if ambiguous:
            classifications.add("ambiguous_helper")
        if unresolved:
            classifications.add("unresolved_helper")
        for call in self.calls(function):
            candidates, resolution_error = self.resolve(call)
            if resolution_error is None and len(candidates) == 1:
                child = self.prove(candidates[0], stack + (function,))
                if not child.ok:
                    classifications.update(child.classifications)
        details: list[str] = []
        if host_loop_lines:
            details.append(
                "host loop(s) at line(s) "
                + ", ".join(str(line) for line in host_loop_lines)
            )
        if fake_counter_lines:
            details.append(
                "GPU execution counter without a device terminal at line(s) "
                + ", ".join(str(line) for line in fake_counter_lines)
            )
        if ambiguous:
            details.append("ambiguous calls: " + "; ".join(ambiguous[:3]))
        if failed_paths:
            details.append("failed helper paths: " + "; ".join(failed_paths[:3]))
        if unresolved:
            details.append("unresolved calls include " + ", ".join(unresolved[:5]))
        if not details:
            details.append("no local helper call can establish device execution")
        proof = _Proof(
            False,
            prefix + "; " + "; ".join(details),
            tuple(sorted(classifications)),
        )
        self.proof_cache[function] = proof
        return proof


def audit_source(
    path: pathlib.Path, source: str, *, require_entrypoint: bool = True
) -> FileAudit:
    try:
        tokens = lex_cpp(source)
        functions = parse_functions(tokens)
    except ParseError as error:
        finding = Finding(path, 1, "<parser>", str(error), ("parser_error",))
        return FileAudit(path, 0, 0, 0, (), (finding,))

    entrypoints = [function for function in functions if function.is_entrypoint]
    findings: list[Finding] = []
    if require_entrypoint and not entrypoints:
        findings.append(
            Finding(
                path,
                1,
                "<inventory>",
                'no extern "C" pgaccel_status entrypoint definitions found',
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

    auditor = _GraphAuditor(path, tokens, functions)
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
            'no extern "C" pgaccel_status entrypoint definitions found in the audited source set',
            ("empty_inventory",),
        )
        audits[0] = dataclasses.replace(first, findings=first.findings + (finding,))
    return audits


def _parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--json-report",
        type=pathlib.Path,
        help="write the complete entrypoint inventory and findings as JSON",
    )
    parser.add_argument(
        "sources", nargs="+", type=pathlib.Path, help="C++ source files to audit"
    )
    return parser.parse_args(argv)


def _write_json_report(path: pathlib.Path, audits: Sequence[FileAudit]) -> None:
    findings = [finding for audit in audits for finding in audit.findings]
    entrypoint_audits = [entry for audit in audits for entry in audit.entrypoint_audits]
    classification_counts = Counter(
        classification
        for entry in entrypoint_audits
        for classification in entry.classifications
    )
    payload = {
        "schema_version": 1,
        "status": "fail" if findings else "pass",
        "summary": {
            "files": len(audits),
            "definitions": sum(audit.definitions for audit in audits),
            "entrypoints": sum(audit.entrypoints for audit in audits),
            "entrypoints_passed": sum(entry.ok for entry in entrypoint_audits),
            "entrypoints_failed": sum(not entry.ok for entry in entrypoint_audits),
            "lifecycle_contracts": sum(audit.lifecycle_contracts for audit in audits),
            "findings": len(findings),
            "classification_counts": dict(sorted(classification_counts.items())),
        },
        "entrypoints": [
            {
                "path": str(entry.path),
                "line": entry.line,
                "name": entry.entrypoint,
                "status": "pass" if entry.ok else "fail",
                "classifications": list(entry.classifications),
                "detail": entry.detail,
            }
            for entry in entrypoint_audits
        ],
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
    if args.json_report is not None:
        _write_json_report(args.json_report, audits)
    for finding in findings:
        classifications = ",".join(finding.classifications) or "unclassified"
        print(
            f"{finding.path}:{finding.line}: {finding.entrypoint}: "
            f"[{classifications}] {finding.message}",
            file=sys.stderr,
        )

    entrypoints = sum(audit.entrypoints for audit in audits)
    definitions = sum(audit.definitions for audit in audits)
    contracts = sum(audit.lifecycle_contracts for audit in audits)
    if findings:
        print(
            "audit-cpu-cheats: FAIL - "
            f"{len(findings)} finding(s) across {entrypoints} entrypoints; "
            "every compute entrypoint must reach a typed SYCL launch.",
            file=sys.stderr,
        )
        return 1

    print(
        "audit-cpu-cheats: PASS - "
        f"{entrypoints} extern-C status entrypoints, {definitions} local definitions, "
        f"{contracts} explicit lifecycle contracts."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
