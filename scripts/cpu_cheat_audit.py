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
import functools
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
from collections import Counter, defaultdict
from collections.abc import Iterable, Mapping, Sequence
from types import MappingProxyType


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

KNOWN_PREPROCESSOR_FEATURES = frozenset(
    {
        "__APPLE__",
        "__clang__",
        "__cplusplus",
    }
)
OUTPUT_ASSIGNMENTS = frozenset(
    {"=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "++", "--"}
)

DEFAULT_ABI_MANIFEST = pathlib.Path(__file__).with_name("cpu_cheat_abi_manifest.txt")
EXPECTED_ABI_MANIFEST_COUNT = 167
EXPECTED_ABI_MANIFEST_SHA256 = (
    "3c8a3db2cd7a070af3ebf796cb7d3189add46959c4cc2eb481554478da4ab2c6"
)
INTERNAL_NON_ABI_HEADERS = frozenset({"alloc_helper.h", "pgaccel_queue.h"})


@dataclasses.dataclass(frozen=True)
class LifecycleContract:
    """An exact non-compute ABI contract and the source evidence it requires."""

    purpose: str
    required_sequences: tuple[tuple[str, ...], ...]
    signature: str
    allow_host_loops: bool = False
    required_any_sequences: tuple[tuple[str, ...], ...] = ()
    noop_guard_markers: tuple[tuple[str, ...], ...] = ()
    noop_guard_polarity: str = "any"
    noop_guard_conditions: tuple[tuple[str, ...], ...] = ()
    conditional_evidence_markers: tuple[tuple[str, ...], ...] = ()
    allow_zero_success: bool = False
    output_policy: str = "none"
    allow_empty_body: bool = False
    allow_ternary: bool = False
    allowed_output_calls: tuple[str, ...] = ()
    exact_body_alternatives: tuple[tuple[str, ...], ...] = ()
    transfer_contract: str = "none"


@dataclasses.dataclass(frozen=True)
class FailOnlyContract:
    """An ABI shim that accepts no nonempty compute work."""

    purpose: str
    required_sequences: tuple[tuple[str, ...], ...]


# These are ABI support operations, not compute kernels.  Exact names keep the
# exception boundary reviewable; required token sequences stop a vacuous name
# match from bypassing the audit.  Compute wrappers must never be added here.
LIFECYCLE_CONTRACTS: Mapping[str, LifecycleContract] = MappingProxyType(
    {
        "pgaccel_init": LifecycleContract(
            "SYCL runtime/device initialization",
            (("sycl", "::", "device", "::", "get_devices", "("),),
            "pgaccel_status ()",
            allow_host_loops=True,
            noop_guard_markers=(("initialized",), ("g_initialized",)),
            noop_guard_polarity="truthy",
        ),
        "pgaccel_shutdown": LifecycleContract(
            "SYCL queue teardown",
            (("wait_and_throw", "("), ("delete", "g_queue")),
            "pgaccel_status ()",
            noop_guard_markers=(("initialized",), ("g_initialized",)),
            noop_guard_polarity="falsy",
            conditional_evidence_markers=(("g_queue",), ("g_ooo_queue",)),
        ),
        "pgaccel_archive_stats_snapshot": LifecycleContract(
            "AdaptiveCpp JIT-cache diagnostics",
            (("std", "::", "filesystem", "::", "directory_iterator", "("),),
            "pgaccel_status (pgaccel_archive_snapshot *)",
            allow_host_loops=True,
            noop_guard_conditions=(
                (
                    "!",
                    "std",
                    "::",
                    "filesystem",
                    "::",
                    "is_directory",
                    "(",
                    "cache_dir",
                    ",",
                    "ec",
                    ")",
                    "||",
                    "ec",
                ),
            ),
            output_policy="metadata",
        ),
        "pgaccel_archive_jit_cache_dir": LifecycleContract(
            "AdaptiveCpp JIT-cache diagnostics",
            (("resolve_jit_cache_dir", "("),),
            "pgaccel_status (char *, size_t)",
            output_policy="metadata",
        ),
        "pgaccel_expr_shared_alloc": LifecycleContract(
            "shared-USM allocation",
            (("sycl", "::", "malloc_shared", "("),),
            "pgaccel_status (size_t, void **)",
            noop_guard_conditions=(("init_status", "!=", "PGACCEL_OK"),),
            allow_zero_success=True,
            output_policy="allocation",
        ),
        "pgaccel_expr_device_alloc": LifecycleContract(
            "device-USM allocation",
            (("sycl", "::", "malloc_device", "("),),
            "pgaccel_status (size_t, void **)",
            noop_guard_conditions=(("init_status", "!=", "PGACCEL_OK"),),
            allow_zero_success=True,
            output_policy="allocation",
        ),
        "pgaccel_expr_device_alloc_copy": LifecycleContract(
            "resident allocation and transfer",
            (("memcpy", "("),),
            "pgaccel_status (const void *, size_t, void **)",
            required_any_sequences=(
                ("sycl", "::", "malloc_shared", "("),
                ("pgaccel_expr_device_alloc", "("),
            ),
            noop_guard_conditions=(
                ("init_status", "!=", "PGACCEL_OK"),
                ("status", "!=", "PGACCEL_OK"),
            ),
            allow_zero_success=True,
            output_policy="allocation",
            allowed_output_calls=("free", "pgaccel_expr_device_alloc"),
            transfer_contract="allocation_copy",
        ),
        "pgaccel_expr_device_copy_from_host": LifecycleContract(
            "host-to-device transfer",
            (("memcpy", "("),),
            "pgaccel_status (void *, const void *, size_t)",
            noop_guard_conditions=(("init_status", "!=", "PGACCEL_OK"),),
            allow_zero_success=True,
            transfer_contract="parameters",
        ),
        "pgaccel_expr_device_copy_to_host": LifecycleContract(
            "device-to-host transfer",
            (("memcpy", "("),),
            "pgaccel_status (void *, const void *, size_t)",
            noop_guard_conditions=(("init_status", "!=", "PGACCEL_OK"),),
            allow_zero_success=True,
            transfer_contract="parameters",
        ),
        "pgaccel_grouped_agg_workspace_requirements": LifecycleContract(
            "workspace metadata calculation",
            (("validate_desc", "("),),
            "pgaccel_status (const pgaccel_grouped_agg_desc *, pgaccel_grouped_agg_workspace_req *)",
            output_policy="metadata",
        ),
        "pgaccel_grouped_agg_workspace_alloc": LifecycleContract(
            "workspace USM allocation",
            (),
            "pgaccel_status (size_t, size_t, int32_t, void **)",
            required_any_sequences=(
                ("sycl", "::", "aligned_alloc_shared", "("),
                ("sycl", "::", "aligned_alloc_device", "("),
            ),
            noop_guard_conditions=(("init_status", "!=", "PGACCEL_OK"),),
            allow_zero_success=True,
            output_policy="allocation",
            allow_ternary=True,
            allowed_output_calls=("free",),
        ),
        "pgaccel_spatial_workspace_finish": LifecycleContract(
            "post-dispatch failure-flag readback",
            (
                ("pgaccel_d2h", "("),
                ("workspace", "->", "failure_flags"),
            ),
            "pgaccel_status (const pgaccel_spatial_workspace *, int32_t *)",
            output_policy="metadata",
        ),
        "pgaccel_gpu_exec_count": LifecycleContract(
            "GPU execution counter accessor",
            (),
            "uint64_t ()",
            required_any_sequences=(
                ("return", "counter", ";"),
                ("return", "tl_gpu_exec_count", ";"),
            ),
            exact_body_alternatives=(
                ("return", "counter", ";"),
                ("return", "tl_gpu_exec_count", ";"),
            ),
        ),
        "pgaccel_reset_gpu_exec_count": LifecycleContract(
            "GPU execution counter reset",
            (),
            "void ()",
            required_any_sequences=(
                ("counter", "=", "0"),
                ("tl_gpu_exec_count", "=", "0"),
            ),
            exact_body_alternatives=(
                ("counter", "=", "0", ";"),
                ("tl_gpu_exec_count", "=", "0", ";"),
            ),
        ),
        "pgaccel_get_device_info": LifecycleContract(
            "device metadata accessor",
            (("return", "g_device_info", ";"),),
            "pgaccel_device_info ()",
            exact_body_alternatives=(("return", "g_device_info", ";"),),
        ),
        "pgaccel_get_caps": LifecycleContract(
            "platform capability accessor",
            (("return", "g_caps", ";"),),
            "pgaccel_platform_caps ()",
            exact_body_alternatives=(("return", "g_caps", ";"),),
        ),
        "pgaccel_prefork_warmup": LifecycleContract(
            "fork-safety environment setup",
            (),
            "void ()",
            required_any_sequences=(("setenv", "("),),
            allow_empty_body=True,
        ),
        "pgaccel_expr_shared_free": LifecycleContract(
            "shared-USM release",
            (("sycl", "::", "free", "("),),
            "void (void *)",
            noop_guard_conditions=(
                ("ptr", "==", "nullptr"),
                ("pgaccel_init", "(", ")", "!=", "PGACCEL_OK"),
                ("q", "==", "nullptr"),
            ),
        ),
        "pgaccel_expr_device_free": LifecycleContract(
            "device-USM release",
            (("sycl", "::", "free", "("),),
            "void (void *)",
            noop_guard_conditions=(
                ("ptr", "==", "nullptr"),
                ("pgaccel_init", "(", ")", "!=", "PGACCEL_OK"),
                ("q", "==", "nullptr"),
            ),
        ),
        "pgaccel_grouped_agg_workspace_free": LifecycleContract(
            "workspace USM release",
            (("sycl", "::", "free", "("),),
            "void (void *)",
            noop_guard_conditions=(
                ("ptr", "==", "nullptr"),
                ("init_status", "!=", "PGACCEL_OK"),
                ("q", "==", "nullptr"),
            ),
        ),
        "pgaccel_pool_reset": LifecycleContract(
            "retired pool no-op reset",
            (),
            "void ()",
            allow_empty_body=True,
            exact_body_alternatives=((),),
        ),
        "pgaccel_hash_join_free": LifecycleContract(
            "hash-join allocation release",
            (("free_table_storage", "("), ("delete", "ht")),
            "void (pgaccel_hash_table *)",
            noop_guard_conditions=(("ht", "==", "nullptr"),),
        ),
        "pgaccel_agg_free": LifecycleContract(
            "hash-aggregate allocation release",
            (("delete", "state"),),
            "void (pgaccel_agg_state *)",
        ),
        "pgaccel_agg_group_count": LifecycleContract(
            "hash-aggregate group-count accessor",
            (("state", "->", "group_count"),),
            "size_t (const pgaccel_agg_state *)",
            noop_guard_conditions=(("state", "==", "nullptr"),),
        ),
        "pgaccel_agg_get_group_keys": LifecycleContract(
            "hash-aggregate group-key accessor",
            (("state", "->", "group_key_buf", ".", "data", "("),),
            "const void * (const pgaccel_agg_state *)",
            noop_guard_conditions=(("state", "==", "nullptr"),),
        ),
        "pgaccel_agg_get_results": LifecycleContract(
            "hash-aggregate result accessor",
            (("state", "->", "results", "["),),
            "const double * (const pgaccel_agg_state *, size_t)",
            noop_guard_conditions=(
                (
                    "state",
                    "==",
                    "nullptr",
                    "||",
                    "agg_idx",
                    ">=",
                    "state",
                    "->",
                    "num_aggs",
                ),
                (
                    "state",
                    "->",
                    "results",
                    ".",
                    "size",
                    "(",
                    ")",
                    "<=",
                    "agg_idx",
                ),
            ),
        ),
        "pgaccel_agg_get_partial_results": LifecycleContract(
            "hash-aggregate partial-result accessor",
            (("state", "->", "partial_results", "["),),
            "const double * (const pgaccel_agg_state *, size_t)",
            noop_guard_conditions=(
                (
                    "state",
                    "==",
                    "nullptr",
                    "||",
                    "agg_idx",
                    ">=",
                    "state",
                    "->",
                    "num_aggs",
                ),
                (
                    "state",
                    "->",
                    "partial_results",
                    ".",
                    "size",
                    "(",
                    ")",
                    "<=",
                    "agg_idx",
                ),
            ),
        ),
        "pgaccel_agg_get_partial_width": LifecycleContract(
            "hash-aggregate partial-width accessor",
            (("state", "->", "partial_widths", "["),),
            "size_t (const pgaccel_agg_state *, size_t)",
            noop_guard_conditions=(
                (
                    "state",
                    "==",
                    "nullptr",
                    "||",
                    "agg_idx",
                    ">=",
                    "state",
                    "->",
                    "num_aggs",
                ),
            ),
        ),
        "pgaccel_agg_get_counts": LifecycleContract(
            "hash-aggregate count accessor",
            (("state", "->", "counts", ".", "data", "("),),
            "const int64_t * (const pgaccel_agg_state *)",
            noop_guard_conditions=(("state", "==", "nullptr"),),
        ),
    }
)


FAIL_ONLY_CONTRACTS: Mapping[str, FailOnlyContract] = MappingProxyType(
    {
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
)


class ParseError(ValueError):
    """Source is not structurally safe to audit."""


@dataclasses.dataclass(frozen=True)
class PreprocessorDirective:
    line: int
    text: str


def _directive_parts(directive: PreprocessorDirective) -> tuple[str, str]:
    match = re.match(r"\s*#\s*([A-Za-z_][A-Za-z0-9_]*)\b(.*)", directive.text, re.S)
    if match is None:
        return "", ""
    return match.group(1), match.group(2).strip()


def _header_guard_names(directives: Sequence[PreprocessorDirective]) -> set[str]:
    guards: set[str] = set()
    for index, directive in enumerate(directives[:-1]):
        command, argument = _directive_parts(directive)
        next_command, next_argument = _directive_parts(directives[index + 1])
        if (
            command == "ifndef"
            and argument.isidentifier()
            and next_command == "define"
            and next_argument.split(maxsplit=1)[0] == argument
            and directives[index + 1].line == directive.line + 1
        ):
            guards.add(argument)
    return guards


def _known_preprocessor_condition(command: str, argument: str) -> bool:
    if command in {"if", "elif"} and re.fullmatch(
        r"\(?\s*(?:0+|1+)[uUlL]*\s*\)?", argument
    ):
        return True
    if command in {"ifdef", "ifndef"}:
        return argument in KNOWN_PREPROCESSOR_FEATURES
    features = re.findall(r"defined\s*(?:\(\s*)?([A-Za-z_][A-Za-z0-9_]*)", argument)
    if features and all(feature in KNOWN_PREPROCESSOR_FEATURES for feature in features):
        remainder = re.sub(
            r"defined\s*(?:\(\s*)?[A-Za-z_][A-Za-z0-9_]*\s*\)?", "", argument
        )
        return not re.search(r"[A-Za-z_]", remainder)
    return False


def _ambiguous_preprocessor_directives(
    directives: Sequence[PreprocessorDirective],
) -> tuple[PreprocessorDirective, ...]:
    guards = _header_guard_names(directives)
    ambiguous: list[PreprocessorDirective] = []
    for directive in directives:
        command, argument = _directive_parts(directive)
        if command not in {"if", "ifdef", "ifndef", "elif"}:
            continue
        if command == "ifndef" and argument in guards:
            continue
        if not _known_preprocessor_condition(command, argument):
            ambiguous.append(directive)
    return tuple(ambiguous)


def _macro_inventory_findings(
    path: pathlib.Path,
    directives: Sequence[PreprocessorDirective],
    *,
    header: bool = False,
    abi_inventory: bool = False,
) -> list[Finding]:
    findings: list[Finding] = []
    for directive in directives:
        command, _ = _directive_parts(directive)
        if command != "define":
            continue
        token_paste = "##" in directive.text
        export_bearing = "pgaccel_" in directive.text or bool(
            re.search(r"extern\s*(?:\\\"C\\\"|\"C\")", directive.text)
        )
        if not token_paste and not export_bearing:
            continue
        if token_paste:
            message = (
                "token-pasting macro can synthesize an export outside the parsed ABI "
                "inventory"
            )
            classifications = (
                "token_paste_export_risk",
                "macro_hidden_export",
                "abi_inventory_mismatch"
                if header or abi_inventory
                else "review_required",
            )
        else:
            message = (
                "export-bearing header macro is absent from the parsed ABI declaration inventory"
                if header
                else "export-bearing macro cannot be proven against the source ABI inventory"
            )
            classifications = (
                "macro_hidden_export",
                "abi_inventory_mismatch"
                if header or abi_inventory
                else "review_required",
            )
        findings.append(
            Finding(
                path,
                directive.line,
                "<preprocessor>",
                message,
                classifications,
            )
        )
    return findings


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


@functools.lru_cache(maxsize=1)
def _compiler_include_directories() -> tuple[pathlib.Path, ...]:
    repo_root = pathlib.Path(__file__).resolve().parents[1]
    roots = [repo_root]
    try:
        completed = subprocess.run(
            ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
            cwd=repo_root,
            text=True,
            capture_output=True,
            check=False,
            timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired):
        completed = None
    if completed is not None and completed.returncode == 0:
        roots.append(pathlib.Path(completed.stdout.strip()).resolve().parent)

    candidates = [repo_root / "pgaccel-kernels/include"]
    acpp_prefix = os.environ.get("ACPP_PREFIX")
    if acpp_prefix:
        candidates.append(pathlib.Path(acpp_prefix) / "include/AdaptiveCpp")
    candidates.extend(
        root / ".pgaccel/acpp/current/include/AdaptiveCpp" for root in roots
    )
    return tuple(dict.fromkeys(path.resolve() for path in candidates if path.is_dir()))


@functools.lru_cache(maxsize=128)
def _compiler_preprocess_cached(
    source: str, source_directory: str, compiler: str
) -> str:
    command = [
        compiler,
        "-std=c++17",
        "-E",
        "-x",
        "c++",
        "-I",
        source_directory,
    ]
    for include_directory in _compiler_include_directories():
        command.extend(("-I", str(include_directory)))
    command.append("-")
    try:
        completed = subprocess.run(
            command,
            input=source,
            text=True,
            capture_output=True,
            check=False,
            timeout=60,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ParseError(f"compiler preprocessing failed: {error}") from error
    if completed.returncode != 0:
        diagnostic = completed.stderr.strip()[-3000:]
        raise ParseError(
            f"compiler preprocessing exited {completed.returncode}: {diagnostic}"
        )
    main_file_lines: list[str] = []
    in_main_file = False
    for line in completed.stdout.splitlines(keepends=True):
        marker = re.match(r'^\s*#\s*\d+\s+"([^"]+)"', line)
        if marker is not None:
            in_main_file = marker.group(1) == "<stdin>"
            continue
        if in_main_file:
            main_file_lines.append(line)
    if not main_file_lines:
        raise ParseError("compiler preprocessing emitted no main-file source")
    return "".join(main_file_lines)


def _compiler_preprocess_source(path: pathlib.Path, source: str) -> str:
    compiler = os.environ.get("PGACCEL_ABI_CLANG") or shutil.which("clang++")
    if compiler is None:
        raise ParseError("clang++ is required to expand source macros")
    source_directory = str(path.resolve().parent)
    return _compiler_preprocess_cached(source, source_directory, compiler)


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
    required_parameter_count: int | None
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
    qualified: bool = False


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
    parameter_types: tuple[str, ...]
    full_signature: str
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
    manifest: dict[str, object] | None = None
    compiler: dict[str, object] | None = None
    objects: tuple[dict[str, object], ...] = ()


@dataclasses.dataclass(frozen=True)
class AbiManifest:
    path: pathlib.Path
    count: int
    sha256: str
    signatures: dict[str, str]


@dataclasses.dataclass(frozen=True)
class CompilerAbiInventory:
    symbols: tuple[AbiSymbol, ...]
    compiler_path: str
    compiler_version: str
    command: tuple[str, ...]
    umbrella_sha256: str
    stderr: str


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


def _required_parameter_count(
    tokens: Sequence[Token], lparen: int, rparen: int
) -> int | None:
    """Return the call arity before trailing C++ default arguments.

    A malformed or non-trailing default sequence is intentionally unresolved.
    Call-graph resolution can then fail closed instead of guessing which local
    overload a call selected.
    """

    total = _parameter_count(tokens, lparen, rparen)
    if total in {None, 0}:
        return total

    segments: list[list[Token]] = []
    start = lparen + 1
    paren = bracket = brace = angle = 0
    for index in range(start, rparen + 1):
        value = tokens[index].value if index < rparen else ","
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
            segments.append(list(tokens[start:index]))
            start = index + 1
    if paren or bracket or brace or angle or len(segments) != total:
        return None

    defaults: list[bool] = []
    for segment in segments:
        paren = bracket = brace = angle = 0
        has_default = False
        for token in segment:
            value = token.value
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
            elif value == "=" and not (paren or bracket or brace or angle):
                has_default = True
                break
        defaults.append(has_default)

    first_default = next(
        (index for index, value in enumerate(defaults) if value), total
    )
    if any(not value for value in defaults[first_default:]):
        return None
    return first_default


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
                required_parameter_count=_required_parameter_count(
                    tokens, lparen, rparen
                ),
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


def _index_is_reachable(
    tokens: Sequence[Token],
    function: Function,
    index: int,
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
) -> bool:
    def return_ends_before(return_index: int) -> bool:
        depth = 0
        for cursor in range(return_index + 1, index):
            value = tokens[cursor].value
            if value in {"(", "[", "{"}:
                depth += 1
            elif value in {")", "]", "}"}:
                depth = max(0, depth - 1)
            elif value == ";" and depth == 0:
                return True
        return False

    if _is_inside(index, lambda_ranges):
        return False
    if any(
        region.definitely_inactive and region.start <= index <= region.end
        for region in regions
    ):
        return False
    return not any(
        return_index < index
        and return_ends_before(return_index)
        and not _is_inside(return_index, lambda_ranges)
        and _context_dominates(
            _context(return_index, regions), _context(index, regions)
        )
        for return_index in range(function.body_open + 1, index)
        if tokens[return_index].value == "return"
    )


def _active_contract_sequence_indices(
    tokens: Sequence[Token],
    function: Function,
    sequence: Sequence[str],
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
) -> list[int]:
    size = len(sequence)
    return [
        index
        for index in range(function.body_open + 1, function.body_close - size + 1)
        if tuple(token.value for token in tokens[index : index + size])
        == tuple(sequence)
        and _index_is_reachable(tokens, function, index, regions, lambda_ranges)
    ]


def _active_contract_sequence(
    tokens: Sequence[Token],
    function: Function,
    sequence: Sequence[str],
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
) -> bool:
    return bool(
        _active_contract_sequence_indices(
            tokens, function, sequence, regions, lambda_ranges
        )
    )


def _return_is_explicit_failure(
    function: Function, expression: Sequence[Token]
) -> bool:
    values = [token.value for token in expression]
    if values and values[0] in FAILURE_STATUSES | {"pgaccel_kernel_failure"}:
        return True
    return not function.is_status and values in (["nullptr"], ["NULL"], ["0"])


def _contract_success_terminals(
    tokens: Sequence[Token],
    function: Function,
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
) -> list[int]:
    terminals = [
        index + len(expression) + 1
        for index, expression in _returns(tokens, function, lambda_ranges)
        if not _return_is_explicit_failure(function, expression)
        and _index_is_reachable(tokens, function, index, regions, lambda_ranges)
    ]
    if function.return_spelling == "void" and _index_is_reachable(
        tokens, function, function.body_close, regions, lambda_ranges
    ):
        terminals.append(function.body_close)
    return terminals


def _strip_condition_parentheses(condition: Sequence[str]) -> tuple[str, ...]:
    values = tuple(condition)
    while len(values) >= 2 and values[0] == "(" and values[-1] == ")":
        depth = 0
        closes_at_end = True
        for index, value in enumerate(values):
            if value == "(":
                depth += 1
            elif value == ")":
                depth -= 1
                if depth == 0 and index != len(values) - 1:
                    closes_at_end = False
                    break
        if not closes_at_end or depth != 0:
            break
        values = values[1:-1]
    return values


def _condition_matches_guard(
    condition: Sequence[str], marker: Sequence[str], polarity: str
) -> bool:
    """Match an entire simple boolean/accessor predicate, never a substring."""

    values = _strip_condition_parentheses(condition)
    negated = bool(values and values[0] == "!")
    if negated:
        values = _strip_condition_parentheses(values[1:])
    if polarity == "truthy" and negated:
        return False
    if polarity == "falsy" and not negated:
        return False

    marker_values = tuple(marker)
    if values == marker_values:
        return True
    suffix = values[len(marker_values) :]
    if values[: len(marker_values)] != marker_values or len(suffix) < 4:
        return False
    if suffix[:3] != (".", "load", "(") or suffix[-1] != ")":
        return False
    depth = 0
    for index, value in enumerate(suffix[2:]):
        if value == "(":
            depth += 1
        elif value == ")":
            depth -= 1
            if depth == 0 and index != len(suffix[2:]) - 1:
                return False
    return depth == 0


def _condition_matches_exact_guard(
    condition: Sequence[str], expected: Sequence[str]
) -> bool:
    return _strip_condition_parentheses(condition) == _strip_condition_parentheses(
        expected
    )


def _exact_argument_identifier(
    tokens: Sequence[Token], bounds: tuple[int, int]
) -> str | None:
    values = _strip_condition_parentheses(
        [token.value for token in tokens[bounds[0] : bounds[1]]]
    )
    if len(values) != 1:
        return None
    for token in tokens[bounds[0] : bounds[1]]:
        if token.value == values[0] and token.kind == "identifier":
            return values[0]
    return None


def _ordered_parameter_names(
    tokens: Sequence[Token], function: Function
) -> tuple[str, ...]:
    parameters, _ = _parameter_names(tokens, function)
    result: list[str] = []
    for left, right in _parameter_ranges(tokens, function):
        candidates = [
            token.value
            for token in tokens[left:right]
            if token.kind == "identifier" and token.value in parameters
        ]
        if not candidates:
            return ()
        result.append(candidates[-1])
    return tuple(result)


def _parameter_transfer_indices(
    tokens: Sequence[Token],
    function: Function,
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
) -> list[int]:
    """Return exact, awaited queue copies over all three transfer parameters."""

    parameter_names = _ordered_parameter_names(tokens, function)
    if len(parameter_names) != 3:
        return []
    forward, _ = _delimiter_pairs(tokens)
    result: list[int] = []
    for method_index in range(function.body_open + 1, function.body_close):
        if (
            tokens[method_index].value != "memcpy"
            or method_index < 2
            or tokens[method_index - 1].value not in {".", "->"}
            or _is_inside(method_index, lambda_ranges)
            or not _index_is_reachable(
                tokens, function, method_index, regions, lambda_ranges
            )
        ):
            continue
        receiver = tokens[method_index - 2].value
        if _receiver_kind(tokens, function, receiver, method_index, forward) != "queue":
            continue
        call = _method_lparen(tokens, method_index, function.body_close, forward)
        if call is None or not _call_is_awaited(
            tokens, call[1], function.body_close, forward
        ):
            continue
        arguments = _call_argument_ranges(tokens, call[0], call[1], forward)
        if len(arguments) != 3:
            continue
        if (
            tuple(
                _exact_argument_identifier(tokens, argument) for argument in arguments
            )
            == parameter_names
        ):
            result.append(method_index)
    return result


def _lifecycle_usm_allocation_events(
    tokens: Sequence[Token],
    function: Function,
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
) -> list[tuple[str, str, int]]:
    """Collect exact local USM allocations for support-operation contracts."""

    forward, _ = _delimiter_pairs(tokens)
    result: list[tuple[str, str, int]] = []
    allocation_kinds = {
        "malloc_shared": "shared",
        "aligned_alloc_shared": "shared",
        "malloc_device": "device",
        "aligned_alloc_device": "device",
    }

    def statement_left(index: int) -> int:
        cursor = index - 1
        while cursor > function.body_open and tokens[cursor].value not in {
            ";",
            "{",
            "}",
        }:
            cursor -= 1
        return cursor + 1

    for index in range(function.body_open + 1, function.body_close):
        kind = allocation_kinds.get(tokens[index].value)
        if (
            kind is None
            or index < 2
            or tokens[index - 2].value != "sycl"
            or tokens[index - 1].value != "::"
            or _is_inside(index, lambda_ranges)
            or not _index_is_reachable(tokens, function, index, regions, lambda_ranges)
            or _method_lparen(tokens, index, function.body_close, forward) is None
        ):
            continue
        left = statement_left(index)
        equals = next(
            (cursor for cursor in range(left, index) if tokens[cursor].value == "="),
            None,
        )
        if equals is None:
            continue
        candidates = [
            token.value
            for token in tokens[left:equals]
            if token.kind == "identifier" and token.value not in CALL_KEYWORDS
        ]
        if candidates:
            result.append((candidates[-1], kind, index))

    helper_kinds = {
        "pgaccel_expr_shared_alloc": "shared",
        "pgaccel_expr_device_alloc": "device",
    }
    for index in range(function.body_open + 1, function.body_close):
        kind = helper_kinds.get(tokens[index].value)
        if (
            kind is None
            or not _unqualified_output_identifier(tokens, index)
            or _is_inside(index, lambda_ranges)
            or not _index_is_reachable(tokens, function, index, regions, lambda_ranges)
        ):
            continue
        call = _method_lparen(tokens, index, function.body_close, forward)
        if call is None:
            continue
        arguments = _call_argument_ranges(tokens, call[0], call[1], forward)
        if not arguments:
            continue
        left, right = arguments[-1]
        values = _strip_condition_parentheses(
            [token.value for token in tokens[left:right]]
        )
        if len(values) == 2 and values[0] == "&":
            root = values[1]
            if any(
                token.kind == "identifier" and token.value == root
                for token in tokens[left:right]
            ):
                result.append((root, kind, index))
    return result


def _allocation_copy_transfer_roots(
    tokens: Sequence[Token],
    function: Function,
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
    allocations: Sequence[tuple[str, str, int]],
) -> list[tuple[int, str]]:
    parameter_names = _ordered_parameter_names(tokens, function)
    if len(parameter_names) != 3:
        return []
    source_name, size_name, _ = parameter_names
    roots = {root for root, _, _ in allocations}
    forward, _ = _delimiter_pairs(tokens)
    result: list[tuple[int, str]] = []
    for method_index in range(function.body_open + 1, function.body_close):
        if (
            tokens[method_index].value != "memcpy"
            or _is_inside(method_index, lambda_ranges)
            or not _index_is_reachable(
                tokens, function, method_index, regions, lambda_ranges
            )
        ):
            continue
        is_member = method_index >= 2 and tokens[method_index - 1].value in {
            ".",
            "->",
        }
        is_std = (
            method_index >= 2
            and tokens[method_index - 2].value == "std"
            and tokens[method_index - 1].value == "::"
        )
        if not (is_member or is_std):
            continue
        call = _method_lparen(tokens, method_index, function.body_close, forward)
        if call is None:
            continue
        arguments = _call_argument_ranges(tokens, call[0], call[1], forward)
        if len(arguments) != 3:
            continue
        destination = _exact_pointer_root(tokens, *arguments[0], roots, forward)
        if destination is None or (
            _exact_argument_identifier(tokens, arguments[1]) != source_name
            or _exact_argument_identifier(tokens, arguments[2]) != size_name
        ):
            continue
        root = destination[0]
        candidates = [
            (kind, index)
            for event_root, kind, index in allocations
            if event_root == root
            and index < method_index
            and not _pointer_root_reassigned(
                tokens, root, index + 1, method_index, lambda_ranges
            )
        ]
        if not candidates:
            continue
        if is_member:
            receiver = tokens[method_index - 2].value
            if _receiver_kind(
                tokens, function, receiver, method_index, forward
            ) != "queue" or not _call_is_awaited(
                tokens, call[1], function.body_close, forward
            ):
                continue
        elif not any(kind == "shared" for kind, _ in candidates):
            continue
        result.append((method_index, root))
    return result


def _lifecycle_proof(
    tokens: Sequence[Token],
    function: Function,
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
    host_writes: Sequence[tuple[int, int, bool]],
    control_lines: Sequence[tuple[str, int]],
    output_call_hazards: Sequence[str],
    shadow_lines: Sequence[int],
) -> _Proof | None:
    contract = LIFECYCLE_CONTRACTS.get(function.name)
    if contract is None:
        return None
    actual_signature = _full_signature(
        function.return_spelling,
        _parameter_type_spellings(tokens, function.lparen, function.rparen),
    )
    values = [
        token.value for token in tokens[function.body_open + 1 : function.body_close]
    ]
    body_is_empty = not values
    missing = [
        sequence
        for sequence in contract.required_sequences
        if not _active_contract_sequence(
            tokens, function, sequence, regions, lambda_ranges
        )
    ]
    active_any = [
        sequence
        for sequence in contract.required_any_sequences
        if _active_contract_sequence(tokens, function, sequence, regions, lambda_ranges)
    ]
    if (
        contract.required_any_sequences
        and not active_any
        and not (contract.allow_empty_body and body_is_empty)
    ):
        missing.append(contract.required_any_sequences[0])
    success_terminals = _contract_success_terminals(
        tokens, function, regions, lambda_ranges
    )
    uncovered: list[tuple[tuple[str, ...], int]] = []
    evidence_groups: list[tuple[tuple[tuple[str, ...], ...], list[int]]] = [
        (
            (sequence,),
            _active_contract_sequence_indices(
                tokens, function, sequence, regions, lambda_ranges
            ),
        )
        for sequence in contract.required_sequences
    ]
    if active_any:
        evidence_groups.append(
            (
                tuple(active_any),
                sorted(
                    {
                        index
                        for sequence in active_any
                        for index in _active_contract_sequence_indices(
                            tokens, function, sequence, regions, lambda_ranges
                        )
                    }
                ),
            )
        )
    if contract.transfer_contract == "parameters":
        transfer_indices = _parameter_transfer_indices(
            tokens, function, regions, lambda_ranges
        )
        if not transfer_indices:
            missing.append(("awaited_queue_memcpy", "(dst, src, bytes)"))
        evidence_groups.append(
            (
                (("awaited_queue_memcpy", "(dst, src, bytes)"),),
                transfer_indices,
            )
        )
    unsafe_contract_writes = list(host_writes)
    if contract.output_policy == "metadata":
        unsafe_contract_writes = []
    elif contract.output_policy == "allocation":
        forward, _ = _delimiter_pairs(tokens)
        allocations = _lifecycle_usm_allocation_events(
            tokens, function, regions, lambda_ranges
        )
        allocation_roots = {root for root, _, _ in allocations}
        copy_transfers = (
            _allocation_copy_transfer_roots(
                tokens, function, regions, lambda_ranges, allocations
            )
            if contract.transfer_contract == "allocation_copy"
            else []
        )
        publication_indices: list[int] = []
        unsafe_contract_writes = []
        for record in host_writes:
            if record[2]:
                continue
            operator = _output_write_operator(
                tokens, record[0], function.body_close, forward
            )
            if operator is None or tokens[operator].value != "=":
                unsafe_contract_writes.append(record)
                continue
            end = operator + 1
            depth = 0
            while end < function.body_close:
                value = tokens[end].value
                if value in {"(", "[", "{"}:
                    depth += 1
                elif value in {")", "]", "}"}:
                    depth = max(0, depth - 1)
                elif value == ";" and depth == 0:
                    break
                end += 1
            publication = _exact_pointer_root(
                tokens, operator + 1, end, allocation_roots, forward
            )
            if publication is None:
                unsafe_contract_writes.append(record)
                continue
            root = publication[0]
            allocation_candidates = [
                index
                for event_root, _, index in allocations
                if event_root == root
                and index < operator
                and not _pointer_root_reassigned(
                    tokens, root, index + 1, operator, lambda_ranges
                )
            ]
            transfer_candidates = [
                index
                for index, transfer_root in copy_transfers
                if transfer_root == root
                and index < operator
                and _context_dominates(
                    _context(index, regions), _context(record[0], regions)
                )
            ]
            if not allocation_candidates or (
                contract.transfer_contract == "allocation_copy"
                and not transfer_candidates
            ):
                unsafe_contract_writes.append(record)
                continue
            publication_indices.append(record[0])
        if not publication_indices:
            missing.append(("publish", "USM", "allocation", "to", "ABI", "output"))
        evidence_groups.append(
            (
                (("publish", "USM", "allocation", "to", "ABI", "output"),),
                publication_indices,
            )
        )
        if contract.transfer_contract == "allocation_copy":
            transfer_indices = [index for index, _ in copy_transfers]
            if not transfer_indices:
                missing.append(("copy", "source", "into", "published", "USM"))
            evidence_groups.append(
                (
                    (("copy", "source", "into", "published", "USM"),),
                    transfer_indices,
                )
            )
    for sequences, evidence in evidence_groups:
        for terminal in success_terminals:
            terminal_regions = [
                region for region in regions if region.start <= terminal <= region.end
            ]
            zero_noop = contract.allow_zero_success and any(
                region.zero_work for region in terminal_regions
            )
            guarded_noop = any(
                _condition_matches_guard(
                    region.condition, marker, contract.noop_guard_polarity
                )
                for region in terminal_regions
                for marker in contract.noop_guard_markers
            ) or any(
                _condition_matches_exact_guard(region.condition, condition)
                for region in terminal_regions
                for condition in contract.noop_guard_conditions
            )
            if zero_noop or guarded_noop:
                continue
            prior_evidence = [index for index in evidence if index <= terminal]
            directly_covered = any(
                _context_dominates(
                    _context(index, regions), _context(terminal, regions)
                )
                for index in prior_evidence
            )
            terminal_context = _context(terminal, regions)
            branch_covered = any(
                {"true", "false"}.issubset(
                    {
                        dict(_context(index, regions)).get(region.key)
                        for index in prior_evidence
                    }
                )
                for region in regions
                if all(
                    _context_dominates(
                        tuple(
                            pair
                            for pair in _context(index, regions)
                            if pair[0] != region.key
                        ),
                        terminal_context,
                    )
                    for index in prior_evidence
                    if region.key in dict(_context(index, regions))
                )
            )
            conditional_noop = any(
                index <= terminal
                and region.start <= index <= region.end < terminal
                and any(
                    _contains_sequence(region.condition, marker)
                    for marker in contract.conditional_evidence_markers
                )
                for index in prior_evidence
                for region in regions
            )
            if not (directly_covered or branch_covered or conditional_noop):
                uncovered.append((sequences[0], tokens[terminal].line))
    forbidden: list[str] = []
    if _canonical_signature_tokens(actual_signature) != _canonical_signature_tokens(
        contract.signature
    ):
        forbidden.append(
            f"ABI signature {actual_signature!r} does not match {contract.signature!r}"
        )
    if (
        contract.exact_body_alternatives
        and tuple(values) not in contract.exact_body_alternatives
    ):
        forbidden.append("body does not match the immutable support-operation shape")
    if not contract.allow_host_loops and set(values) & {"for", "while", "do"}:
        forbidden.append("host loop")
    if _contains_sequence(values, ("pgaccel_record_gpu_exec", "(")):
        forbidden.append("GPU execution counter")
    if _contract_contains_dispatch(tokens, function) is not None:
        forbidden.append("device dispatch")
    if unsafe_contract_writes:
        forbidden.append(
            "host ABI-output write at line(s) "
            + ", ".join(str(record[1]) for record in unsafe_contract_writes)
        )
    lifecycle_control_lines = [
        item
        for item in control_lines
        if not (contract.allow_ternary and item[0] == "?")
    ]
    if lifecycle_control_lines:
        forbidden.append(
            "ambiguous control flow "
            + ", ".join(
                f"{kind} at line {line}" for kind, line in lifecycle_control_lines
            )
        )
    if contract.output_policy != "metadata":
        forbidden.extend(output_call_hazards)
    if shadow_lines:
        forbidden.append(
            "ABI output identity is shadowed at line(s) "
            + ", ".join(map(str, shadow_lines))
        )
    if uncovered:
        forbidden.append(
            "required lifecycle evidence does not dominate success: "
            + ", ".join(
                f"{' '.join(sequence)} -> line {line}" for sequence, line in uncovered
            )
        )
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


def _zero_disjunction_names(
    condition: Sequence[str], allowed: set[str]
) -> set[str] | None:
    compact = [value for value in condition if value not in {"(", ")"}]
    chunks: list[list[str]] = [[]]
    for value in compact:
        if value == "||":
            chunks.append([])
        else:
            chunks[-1].append(value)
    names: set[str] = set()
    for chunk in chunks:
        if len(chunk) != 3 or chunk[1] != "==":
            return None
        left, _, right = chunk
        if left in allowed and right in {"0", "0u", "0U"}:
            names.add(left)
        elif right in allowed and left in {"0", "0u", "0U"}:
            names.add(right)
        else:
            return None
    return names


def _fail_only_path_errors(
    tokens: Sequence[Token],
    function: Function,
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
) -> list[str]:
    returns = [
        (index, tuple(token.value for token in expression))
        for index, expression in _returns(tokens, function, lambda_ranges)
        if _index_is_reachable(tokens, function, index, regions, lambda_ranges)
    ]
    if function.name == "pgaccel_spatial_intersects":
        parameters, _ = _parameter_names(tokens, function)
        zero_parameters = _zero_work_parameter_names(tokens, function, parameters)
        valid_regions = [
            region
            for region in regions
            if region.branch == "true"
            and _zero_disjunction_names(region.condition, zero_parameters)
            == {"count_a", "count_b"}
        ]
        success = [record for record in returns if record[1] == ("PGACCEL_OK",)]
        unsupported = [
            record for record in returns if record[1] == ("PGACCEL_UNSUPPORTED",)
        ]
        errors: list[str] = []
        if not success or any(
            not any(
                region.start <= index <= region.end
                and _context(index, regions) == ((region.key, "true"),)
                for region in valid_regions
            )
            for index, _ in success
        ):
            errors.append(
                "every OK return must be guarded only by exact typed count_a/count_b zero work"
            )
        if not unsupported or any(_context(index, regions) for index, _ in unsupported):
            errors.append(
                "the nonzero path must end in an unconditional unsupported return"
            )
        if (
            success
            and unsupported
            and min(index for index, _ in unsupported)
            < max(index for index, _ in success)
        ):
            errors.append("unsupported fallback precedes the zero-work success")
        other_success = [
            index
            for index, expression in returns
            if expression not in {("PGACCEL_OK",), ("PGACCEL_UNSUPPORTED",)}
            and not (
                expression
                and expression[0] in FAILURE_STATUSES | {"pgaccel_kernel_failure"}
            )
        ]
        if other_success:
            errors.append("contains an unclassified success return")
        return errors

    if function.name == "pgaccel_spatial_eval_resident_ex":
        guard_condition = ("request", "->", "count", "!=", "0")
        guards = [
            region
            for region in regions
            if region.branch == "true"
            and tuple(value for value in region.condition if value not in {"(", ")"})
            == guard_condition
        ]
        unsupported = [
            (index, expression)
            for index, expression in returns
            if expression == ("PGACCEL_UNSUPPORTED",)
        ]
        validator_expression = (
            "resident_validate_request_contract",
            "(",
            "request",
            ",",
            "detail",
            ")",
        )
        validators = [
            (index, expression)
            for index, expression in returns
            if expression == validator_expression
        ]
        errors = []
        if not unsupported or any(
            not any(region.start <= index <= region.end for region in guards)
            for index, _ in unsupported
        ):
            errors.append("nonempty request work is not guarded by unsupported")
        if (
            len(validators) != 1
            or _context(validators[0][0], regions)
            or (
                unsupported
                and validators[0][0] < max(index for index, _ in unsupported)
            )
        ):
            errors.append("zero-row validation is not the unconditional final path")
        other_success = [
            index
            for index, expression in returns
            if expression != validator_expression
            and not (
                expression
                and expression[0] in FAILURE_STATUSES | {"pgaccel_kernel_failure"}
            )
        ]
        if other_success:
            errors.append("contains a success outside the zero-row validator")
        return errors
    return ["unknown failure-only contract path model"]


def _fail_only_contract_proof(
    tokens: Sequence[Token],
    function: Function,
    regions: Sequence[_Region],
    lambda_ranges: Sequence[tuple[int, int]],
    host_writes: Sequence[tuple[int, int, bool]],
    control_lines: Sequence[tuple[str, int]],
    output_call_hazards: Sequence[str],
    shadow_lines: Sequence[int],
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
        if not _active_contract_sequence(
            tokens, function, sequence, regions, lambda_ranges
        )
    ]
    path_errors = _fail_only_path_errors(tokens, function, regions, lambda_ranges)
    forbidden: list[str] = []
    if set(values) & {"for", "while", "do"}:
        forbidden.append("host loop")
    if _contains_sequence(values, ("pgaccel_record_gpu_exec", "(")):
        forbidden.append("GPU execution counter")
    if _contract_contains_dispatch(tokens, function) is not None:
        forbidden.append("device dispatch")
    unsafe_writes = [record for record in host_writes if not record[2]]
    if unsafe_writes:
        forbidden.append(
            "non-neutral host ABI-output write at line(s) "
            + ", ".join(str(record[1]) for record in unsafe_writes)
        )
    if control_lines:
        forbidden.append(
            "ambiguous control flow "
            + ", ".join(f"{kind} at line {line}" for kind, line in control_lines)
        )
    forbidden.extend(path_errors)
    forbidden.extend(output_call_hazards)
    if shadow_lines:
        forbidden.append(
            "ABI output identity is shadowed at line(s) "
            + ", ".join(map(str, shadow_lines))
        )
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
    condition: tuple[str, ...] = ()


@dataclasses.dataclass(frozen=True)
class _DispatchEvidence:
    index: int
    line: int
    method: str
    detail: str
    context: tuple[tuple[str, str], ...]


@dataclasses.dataclass(frozen=True)
class _KernelWriteEvidence:
    index: int
    line: int
    method: str
    context: tuple[tuple[str, str], ...]
    roots: frozenset[str]
    awaited: bool


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
            return start - 1, cursor, cursor + 1
        cursor += 1
    return start - 1, limit, limit


def _condition_is_zero(
    values: Sequence[str],
    parameters: set[str],
    aggregate_parameters: set[str],
) -> bool:
    compact = tuple(value for value in values if value not in {"(", ")"})
    if "||" in compact and _zero_disjunction_names(compact, parameters):
        return True
    if len(compact) == 3:
        left, operator, right = compact
        return operator == "==" and (
            (left in parameters and right in {"0", "0u", "0U"})
            or (right in parameters and left in {"0", "0u", "0U"})
        )
    count_member = re.compile(
        r"(?:n|count|size|length|rows|cols|bytes|num_.+|n_.+|"
        r"count_.+|.+_(?:count|size|length|rows|cols|bytes))"
    )
    return bool(
        len(compact) == 5
        and compact[0] in aggregate_parameters
        and compact[1] in {".", "->"}
        and count_member.fullmatch(compact[2])
        and compact[3] == "=="
        and compact[4] in {"0", "0u", "0U"}
    )


def _regions(
    tokens: Sequence[Token],
    function: Function,
    forward: dict[int, int],
    zero_parameters: set[str],
    aggregate_parameters: set[str],
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
                    key=key,
                    branch="true",
                    start=start,
                    end=end,
                    zero_work=_condition_is_zero(
                        condition, zero_parameters, aggregate_parameters
                    ),
                    definitely_inactive=false_condition,
                    condition=tuple(condition),
                )
            )
            if after < function.body_close and tokens[after].value == "else":
                false_start, false_end, _ = _statement_range(
                    tokens, after + 1, function.body_close, forward
                )
                regions.append(
                    _Region(
                        key=key,
                        branch="false",
                        start=false_start,
                        end=false_end,
                        definitely_inactive=true_condition,
                        condition=tuple(condition),
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
        segment = list(tokens[left:right])
        depth = 0
        for index, token in enumerate(segment):
            if token.value in {"(", "[", "{"}:
                depth += 1
            elif token.value in {")", "]", "}"}:
                depth = max(0, depth - 1)
            elif token.value == "=" and depth == 0:
                segment = segment[:index]
                break
        values = [token.value for token in segment]
        identifiers = [
            token.value
            for token in segment
            if token.kind == "identifier" and token.value not in ignored
        ]
        if not identifiers:
            continue
        name = identifiers[-1]
        parameters.add(name)
        name_offset = max(
            index for index, token in enumerate(segment) if token.value == name
        )
        before_name = values[:name_offset]
        sycl_service_reference = "&" in before_name and any(
            before_name[index : index + 3] == ["sycl", "::", service]
            for service in ("queue", "handler")
            for index in range(max(0, len(before_name) - 2))
        )
        if (
            ("*" in before_name or "&" in before_name)
            and "const" not in before_name
            and not sycl_service_reference
        ):
            mutable.add(name)
    return parameters, mutable


def _parameter_ranges(
    tokens: Sequence[Token], function: Function
) -> list[tuple[int, int]]:
    forward, _ = _delimiter_pairs(tokens)
    ranges: list[tuple[int, int]] = []
    segment_start = function.lparen + 1
    cursor = segment_start
    while cursor < function.rparen:
        if tokens[cursor].value in {"(", "[", "{"} and cursor in forward:
            cursor = forward[cursor] + 1
            continue
        if tokens[cursor].value == ",":
            ranges.append((segment_start, cursor))
            segment_start = cursor + 1
        cursor += 1
    ranges.append((segment_start, function.rparen))
    return ranges


def _zero_work_parameter_names(
    tokens: Sequence[Token], function: Function, parameters: set[str]
) -> set[str]:
    """Return explicitly numeric ABI parameters whose names denote work counts."""

    numeric_types = {
        "size_t",
        "ptrdiff_t",
        "char",
        "short",
        "int",
        "long",
        "unsigned",
        "uint8_t",
        "uint16_t",
        "uint32_t",
        "uint64_t",
        "int8_t",
        "int16_t",
        "int32_t",
        "int64_t",
    }

    def count_name(name: str) -> bool:
        return bool(
            re.fullmatch(
                r"(?:n|count|size|length|rows|cols|bytes|num_.+|n_.+|"
                r"count_.+|.+_(?:count|size|length|rows|cols|bytes))",
                name,
            )
        )

    result: set[str] = set()
    for left, right in _parameter_ranges(tokens, function):
        values = [token.value for token in tokens[left:right]]
        names = [
            token.value
            for token in tokens[left:right]
            if token.kind == "identifier" and token.value in parameters
        ]
        if not names:
            continue
        name = names[-1]
        name_index = max(index for index, value in enumerate(values) if value == name)
        declared_type = values[:name_index]
        if (
            count_name(name)
            and not ({"*", "&", "&&", "bool"} & set(declared_type))
            and numeric_types.intersection(declared_type)
        ):
            result.add(name)
    return result


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
    if set(prefix) & CALL_KEYWORDS:
        return None
    # Do not mistake an assignment following a single-statement control body
    # for a declaration, as in ``if (out) *out = 0``.
    if ")" in prefix or "]" in prefix:
        return None
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
    outermost = [
        candidate
        for candidate in candidates
        if not any(
            other[0] < candidate[0] < candidate[1] < other[1]
            for other in candidates
            if other != candidate
        )
    ]
    return outermost[-1] if outermost else None


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


def _definitely_inactive_ranges(
    tokens: Sequence[Token],
    bounds: tuple[int, int],
    forward: dict[int, int],
) -> list[tuple[int, int]]:
    """Return literal-dead branches nested inside a lambda body."""

    left, right = bounds
    inactive: list[tuple[int, int]] = []
    cursor = left + 1
    while cursor < right:
        if tokens[cursor].value != "if" or cursor + 1 >= right:
            cursor += 1
            continue
        condition_open = cursor + 1
        if tokens[condition_open].value == "constexpr":
            condition_open += 1
        if (
            condition_open >= right
            or tokens[condition_open].value != "("
            or condition_open not in forward
        ):
            cursor += 1
            continue
        condition_close = forward[condition_open]
        condition = [
            token.value for token in tokens[condition_open + 1 : condition_close]
        ]
        start, end, after = _statement_range(
            tokens, condition_close + 1, right, forward
        )
        if condition in (["false"], ["0"]):
            inactive.append((start, end))
        if (
            condition in (["true"], ["1"])
            and after < right
            and tokens[after].value == "else"
        ):
            false_start, false_end, _ = _statement_range(
                tokens, after + 1, right, forward
            )
            inactive.append((false_start, false_end))
        cursor += 1
    return inactive


def _lambda_written_roots(tokens: Sequence[Token], bounds: tuple[int, int]) -> set[str]:
    left, right = bounds
    forward, reverse = _delimiter_pairs(tokens)
    inactive = _definitely_inactive_ranges(tokens, bounds, forward)
    nested = []
    for brace in range(left + 1, right):
        if tokens[brace].value != "{" or brace not in forward:
            continue
        cursor = brace - 1
        if cursor >= 0 and tokens[cursor].value == ")" and cursor in reverse:
            cursor = reverse[cursor] - 1
        if cursor > left and tokens[cursor].value == "]":
            nested.append((brace, forward[brace]))
    return {
        tokens[index].value
        for index in range(left + 1, right)
        if tokens[index].kind == "identifier"
        and _unqualified_output_identifier(tokens, index)
        and not _is_inside(index, nested)
        and not any(start <= index <= end for start, end in inactive)
        and _output_write_operator(tokens, index, right, forward) is not None
    }


def _lambda_contributes(
    tokens: Sequence[Token], bounds: tuple[int, int], mutable: set[str]
) -> bool:
    return bool(_lambda_written_roots(tokens, bounds) & mutable)


def _call_is_awaited(
    tokens: Sequence[Token], rparen: int, limit: int, forward: dict[int, int]
) -> bool:
    cursor = rparen + 1
    return bool(
        cursor + 2 < limit
        and tokens[cursor].value in {".", "->"}
        and tokens[cursor + 1].value in {"wait", "wait_and_throw"}
        and tokens[cursor + 2].value == "("
        and cursor + 2 in forward
    )


def _lambda_nonempty(tokens: Sequence[Token], bounds: tuple[int, int]) -> bool:
    left, right = bounds
    return any(token.value != ";" for token in tokens[left + 1 : right])


def _direct_dispatches(
    tokens: Sequence[Token],
    function: Function,
    mutable: set[str],
    regions: Sequence[_Region],
) -> tuple[
    list[_DispatchEvidence],
    list[_DispatchEvidence],
    list[str],
    set[tuple[int, int]],
    list[_KernelWriteEvidence],
]:
    forward, reverse = _delimiter_pairs(tokens)
    lambdas = _lambda_ranges(tokens, function, forward, reverse)
    evidence: list[_DispatchEvidence] = []
    raw_launches: list[_DispatchEvidence] = []
    rejected: list[str] = []
    device_lambdas: set[tuple[int, int]] = set()
    kernel_writes: list[_KernelWriteEvidence] = []
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
            written_roots: set[str] = set()
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
                        device_lambdas.add(kernel)
                        raw_inner_found = True
                        roots = _lambda_written_roots(tokens, kernel)
                        written_roots.update(roots)
                        if roots & mutable:
                            inner_found = True
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
                kernel_writes.append(
                    _KernelWriteEvidence(
                        index,
                        tokens[index + 2].line,
                        method,
                        _context(index, regions),
                        frozenset(written_roots),
                        _call_is_awaited(tokens, rparen, function.body_close, forward),
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
                device_lambdas.add(kernel)
                roots = _lambda_written_roots(tokens, kernel)
                raw_launches.append(
                    _DispatchEvidence(
                        index,
                        tokens[index + 2].line,
                        method,
                        f"typed nonempty SYCL {method} chain at line {tokens[index + 2].line}",
                        _context(index, regions),
                    )
                )
                kernel_writes.append(
                    _KernelWriteEvidence(
                        index,
                        tokens[index + 2].line,
                        method,
                        _context(index, regions),
                        frozenset(roots),
                        _call_is_awaited(tokens, rparen, function.body_close, forward),
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
    return evidence, raw_launches, rejected, device_lambdas, kernel_writes


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
        if (
            index > 1
            and tokens[index - 1].value == "<"
            and tokens[index - 2].value in DISPATCH_METHODS
        ):
            continue
        if index > 0 and tokens[index - 1].value in {".", "->"}:
            continue
        qualified = index > 0 and tokens[index - 1].value == "::"
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
        call_name = token.value
        if qualified:
            components = [token.value]
            qualifier = index - 2
            while qualifier >= function.body_open:
                if tokens[qualifier].kind != "identifier":
                    break
                components.append(tokens[qualifier].value)
                if qualifier < 2 or tokens[qualifier - 1].value != "::":
                    break
                qualifier -= 2
            call_name = "::".join(reversed(components))
        result.append(
            _IndexedCall(
                Call(
                    call_name,
                    token.line,
                    _parameter_count(tokens, cursor, close),
                    explicit,
                    qualified,
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


def _output_write_operator(
    tokens: Sequence[Token],
    index: int,
    limit: int,
    forward: dict[int, int],
) -> int | None:
    """Return the assignment operator for a write rooted at an output value."""

    if index > 0 and tokens[index - 1].value in {"++", "--"}:
        return index - 1
    cursor = index + 1
    projected = index > 0 and tokens[index - 1].value == "*"
    # In ``*(out + 0) = value`` the assignment follows the matching close,
    # rather than the output identifier itself.
    enclosing_dereference = next(
        (
            close
            for lparen, close in forward.items()
            if tokens[lparen].value == "("
            and lparen < index < close
            and lparen > 0
            and tokens[lparen - 1].value == "*"
        ),
        None,
    )
    if enclosing_dereference is not None:
        projected = True
        cursor = enclosing_dereference + 1
    while cursor < limit:
        value = tokens[cursor].value
        if value == "[" and cursor in forward:
            projected = True
            cursor = forward[cursor] + 1
            continue
        if value in {"->", "."} and cursor + 1 < limit:
            projected = True
            cursor += 2
            continue
        if value == ")" and projected:
            cursor += 1
            continue
        break
    if cursor < limit and tokens[cursor].value in {"++", "--"} and projected:
        return cursor
    if cursor < limit and tokens[cursor].value in OUTPUT_ASSIGNMENTS and projected:
        return cursor
    return None


def _unqualified_output_identifier(tokens: Sequence[Token], index: int) -> bool:
    return index == 0 or tokens[index - 1].value not in {".", "->", "::"}


def _range_references_output(
    tokens: Sequence[Token], left: int, right: int, roots: set[str]
) -> bool:
    return any(
        tokens[index].value in roots and _unqualified_output_identifier(tokens, index)
        for index in range(left, right)
    )


def _neutral_output_write(
    tokens: Sequence[Token],
    operator: int,
    limit: int,
    allowed_identity_identifiers: set[str],
) -> bool:
    if tokens[operator].value != "=":
        return False
    values: list[str] = []
    cursor = operator + 1
    depth = 0
    while cursor < limit:
        value = tokens[cursor].value
        if value in {"(", "[", "{"}:
            depth += 1
        elif value in {")", "]", "}"}:
            if depth == 0:
                break
            depth -= 1
        if value in {";", ","} and depth == 0:
            break
        values.append(value)
        cursor += 1
    compact = tuple(value for value in values if value not in {"(", ")"})

    def zero_literal(value: str) -> bool:
        return bool(
            re.fullmatch(
                r"(?:0+[uUlL]*|0[xX]0+[uUlL]*|"
                r"(?:0+\.0*|0*\.0+)(?:[eE][+-]?0+)?[fFlL]?|"
                r"0+[eE][+-]?0+[fFlL]?)",
                value,
            )
        )

    if compact in {
        ("false",),
        ("nullptr",),
        ("NULL",),
    } or (len(compact) == 1 and zero_literal(compact[0])):
        return True

    # A typed aggregate identity may be written on the host only for an exact
    # zero-work branch. It must be independent of every runtime parameter and
    # match a closed constant grammar. This admits integral forms such as
    # ``T{0}`` and ``static_cast<T>(~T{0})`` without treating ``42``,
    # ``data[0]``, or a disguised host finalizer as an identity.
    if any(
        token.kind == "identifier" and token.value not in allowed_identity_identifiers
        for token in tokens[operator + 1 : cursor]
    ):
        return False
    if any(value in {"[", "]", ".", "->", "++", "--", ","} for value in values):
        return False

    cast_keywords = {
        "static_cast",
        "const_cast",
        "reinterpret_cast",
        "dynamic_cast",
    }

    def strip_parentheses(expression: tuple[str, ...]) -> tuple[str, ...]:
        while len(expression) >= 2 and expression[0] == "(" and expression[-1] == ")":
            depth = 0
            closes_at_end = True
            for index, value in enumerate(expression):
                if value == "(":
                    depth += 1
                elif value == ")":
                    depth -= 1
                    if depth == 0 and index != len(expression) - 1:
                        closes_at_end = False
                        break
            if not closes_at_end or depth != 0:
                break
            expression = expression[1:-1]
        return expression

    def typed_zero(expression: tuple[str, ...]) -> bool:
        return (
            len(expression) == 4
            and expression[0] in allowed_identity_identifiers
            and expression[1] == "{"
            and zero_literal(expression[2])
            and expression[3] == "}"
        )

    def constant_identity(expression: tuple[str, ...]) -> bool:
        expression = strip_parentheses(expression)
        if typed_zero(expression):
            return True
        if expression[:1] == ("~",) and typed_zero(expression[1:]):
            return True
        if len(expression) < 7 or expression[0] not in cast_keywords:
            return False
        if expression[1] != "<":
            return False
        angle_depth = 1
        close = 2
        while close < len(expression) and angle_depth:
            if expression[close] == "<":
                angle_depth += 1
            elif expression[close] == ">":
                angle_depth -= 1
            elif expression[close] == ">>":
                angle_depth -= 2
            close += 1
        if angle_depth != 0 or close >= len(expression):
            return False
        cast_type = expression[2 : close - 1]
        if not cast_type or any(
            value not in allowed_identity_identifiers | {"::", "*", "&", "const"}
            for value in cast_type
        ):
            return False
        if expression[close] != "(" or expression[-1] != ")":
            return False
        return constant_identity(expression[close + 1 : -1])

    return constant_identity(tuple(values))


def _output_aliases(
    tokens: Sequence[Token],
    function: Function,
    mutable: set[str],
    lambda_ranges: Sequence[tuple[int, int]],
) -> tuple[set[str], set[int]]:
    forward, _ = _delimiter_pairs(tokens)
    roots = set(mutable)
    declarations: set[int] = set()
    pointer_locals: set[str] = set()

    def statement_left(index: int) -> int:
        initializer = [
            open_index
            for open_index, close_index in forward.items()
            if tokens[open_index].value == "{"
            and open_index < index < close_index
            and open_index > function.body_open
            and tokens[open_index - 1].kind == "identifier"
            and tokens[open_index - 1].value not in {"try", "catch", "else", "do"}
        ]
        cursor = (max(initializer) - 1) if initializer else index - 1
        while cursor > function.body_open and tokens[cursor].value not in {
            ";",
            "{",
            "}",
        }:
            cursor -= 1
        return cursor + 1

    def declaration_candidate(left: int, delimiter: int) -> tuple[int, bool] | None:
        candidates = [
            index
            for index in range(left, delimiter)
            if tokens[index].kind == "identifier"
            and tokens[index].value not in CALL_KEYWORDS
        ]
        if not candidates:
            return None
        candidate = candidates[-1]
        if tokens[candidate].value in {"try", "catch", "else", "do"}:
            return None
        prefix = [token.value for token in tokens[left:candidate]]
        if set(prefix) & CALL_KEYWORDS or any(
            value in {"(", ")", "[", "]"} for value in prefix
        ):
            return None
        return candidate, "*" in prefix or "auto" in prefix

    def member_base(left: int, equals: int) -> int | None:
        """Return ``holder`` for an assignment shaped like ``holder.ptr =``."""

        cursor = equals - 1
        if (
            cursor < left
            or tokens[cursor].kind != "identifier"
            or cursor - 1 < left
            or tokens[cursor - 1].value not in {".", "->"}
        ):
            return None
        cursor -= 2
        while (
            cursor - 2 >= left
            and tokens[cursor - 1].value in {".", "->"}
            and tokens[cursor - 2].kind == "identifier"
        ):
            cursor -= 2
        return cursor if tokens[cursor].kind == "identifier" else None

    # Remember pointer-shaped declarations for a later ``alias = out``.
    for delimiter in range(function.body_open + 1, function.body_close):
        if tokens[delimiter].value not in {"=", ";", "{"}:
            continue
        left = statement_left(delimiter)
        candidate = declaration_candidate(left, delimiter)
        if candidate is not None and candidate[1]:
            pointer_locals.add(tokens[candidate[0]].value)

    changed = True
    while changed:
        changed = False
        for occurrence in range(function.body_open + 1, function.body_close):
            if (
                tokens[occurrence].value not in roots
                or not _unqualified_output_identifier(tokens, occurrence)
                or _is_inside(occurrence, lambda_ranges)
            ):
                continue
            left = statement_left(occurrence)
            candidate: int | None = None
            equals = next(
                (
                    index
                    for index in range(left, occurrence)
                    if tokens[index].value == "="
                ),
                None,
            )
            if equals is not None:
                declaration = declaration_candidate(left, equals)
                if declaration is not None and declaration[1]:
                    candidate = declaration[0]
                elif (base := member_base(left, equals)) is not None:
                    candidate = base
                elif equals > left and tokens[equals - 1].kind == "identifier":
                    name = tokens[equals - 1].value
                    if name in pointer_locals or name in roots:
                        candidate = equals - 1

            # Direct-list initialization propagates an ABI pointer through
            # both ``auto alias{out}`` and an aggregate such as
            # ``Holder holder{out}``. Member projections from that aggregate
            # are conservatively treated as output aliases.
            if candidate is None:
                for brace in range(left, occurrence):
                    if (
                        tokens[brace].value == "{"
                        and brace in forward
                        and brace > left
                        and forward[brace] >= occurrence
                        and tokens[brace - 1].kind == "identifier"
                    ):
                        declaration = declaration_candidate(left, brace)
                        if (
                            declaration is not None
                            and declaration[0] == brace - 1
                            and any(
                                token.kind == "identifier"
                                for token in tokens[left : declaration[0]]
                            )
                        ):
                            candidate = declaration[0]
                            break
            if candidate is None:
                continue
            alias = tokens[candidate].value
            if alias in roots:
                continue
            roots.add(alias)
            declarations.add(candidate)
            pointer_locals.add(alias)
            changed = True
    return roots - mutable, declarations


def _host_output_accesses(
    tokens: Sequence[Token],
    function: Function,
    mutable: set[str],
    lambda_ranges: Sequence[tuple[int, int]],
) -> tuple[list[tuple[int, int, bool]], list[tuple[int, int]], set[str]]:
    forward, _ = _delimiter_pairs(tokens)
    identity_identifiers = {
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
        "true",
        "false",
        "nullptr",
        "NULL",
        "static_cast",
        "const_cast",
        "reinterpret_cast",
        "dynamic_cast",
    }
    if function.is_template:
        for index in range(function.signature_start, function.name_index - 1):
            if (
                tokens[index].value in {"typename", "class"}
                and tokens[index + 1].kind == "identifier"
            ):
                identity_identifiers.add(tokens[index + 1].value)
    aliases, alias_declarations = _output_aliases(
        tokens, function, mutable, lambda_ranges
    )
    roots = mutable | aliases
    writes: dict[int, tuple[int, int, bool]] = {}
    transfers: dict[int, tuple[int, int]] = {}
    for index in range(function.body_open + 1, function.body_close):
        if (
            tokens[index].value not in roots
            or not _unqualified_output_identifier(tokens, index)
            or index in alias_declarations
            or _is_inside(index, lambda_ranges)
        ):
            continue
        operator = _output_write_operator(tokens, index, function.body_close, forward)
        if operator is not None:
            writes[index] = (
                index,
                tokens[index].line,
                _neutral_output_write(
                    tokens, operator, function.body_close, identity_identifiers
                ),
            )
        if (
            index > 1
            and tokens[index - 1].value == "("
            and tokens[index - 2].value in {"memcpy", "copy"}
        ):
            if index > 2 and tokens[index - 3].value in {".", "->"}:
                method_index = index - 2
                transfers[method_index] = (method_index, tokens[index].line)
            else:
                writes[index] = (index, tokens[index].line, False)
    return sorted(writes.values()), sorted(transfers.values()), aliases


def _local_usm_provenance(
    tokens: Sequence[Token],
    function: Function,
    lambda_ranges: Sequence[tuple[int, int]],
) -> dict[str, tuple[str, int]]:
    """Track local pointers/aggregates rooted in shared or device USM."""

    forward, _ = _delimiter_pairs(tokens)
    provenance: dict[str, tuple[str, int]] = {}

    def statement_left(index: int) -> int:
        cursor = index - 1
        while cursor > function.body_open and tokens[cursor].value not in {
            ";",
            "{",
            "}",
        }:
            cursor -= 1
        return cursor + 1

    def target_before(delimiter: int, left: int) -> str | None:
        names = [
            token.value
            for token in tokens[left:delimiter]
            if token.kind == "identifier" and token.value not in CALL_KEYWORDS
        ]
        return names[-1] if names else None

    allocation_kinds = {
        "malloc_shared": "shared",
        "aligned_alloc_shared": "shared",
        "malloc_device": "device",
        "aligned_alloc_device": "device",
    }
    for index in range(function.body_open + 1, function.body_close):
        if _is_inside(index, lambda_ranges):
            continue
        kind = allocation_kinds.get(tokens[index].value)
        if (
            kind is None
            or index < 2
            or tokens[index - 1].value != "::"
            or tokens[index - 2].value != "sycl"
            or _method_lparen(tokens, index, function.body_close, forward) is None
        ):
            continue
        left = statement_left(index)
        equals = next(
            (cursor for cursor in range(left, index) if tokens[cursor].value == "="),
            None,
        )
        target = target_before(equals, left) if equals is not None else None
        if target is None:
            initializer = [
                open_index
                for open_index, close_index in forward.items()
                if tokens[open_index].value == "{"
                and open_index < index < close_index
                and open_index > function.body_open
                and tokens[open_index - 1].kind == "identifier"
                and tokens[open_index - 1].value not in {"try", "catch", "else", "do"}
            ]
            if initializer:
                target = tokens[max(initializer) - 1].value
        if target is not None:
            provenance[target] = (kind, index)

    # Exact allocation helpers expose their result through the final pointer
    # argument rather than a C++ assignment expression.
    helper_kinds = {
        "pgaccel_expr_shared_alloc": "shared",
        "pgaccel_expr_device_alloc": "device",
    }
    for index in range(function.body_open + 1, function.body_close):
        kind = helper_kinds.get(tokens[index].value)
        if (
            kind is None
            or not _unqualified_output_identifier(tokens, index)
            or _is_inside(index, lambda_ranges)
        ):
            continue
        call = _method_lparen(tokens, index, function.body_close, forward)
        if call is None:
            continue
        arguments = _call_argument_ranges(tokens, call[0], call[1], forward)
        if not arguments:
            continue
        candidates = [
            tokens[cursor].value
            for cursor in range(*arguments[-1])
            if tokens[cursor].kind == "identifier"
        ]
        if candidates:
            provenance[candidates[-1]] = (kind, index)

    # Propagate through typed/cast pointer declarations and aggregate members.
    changed = True
    while changed:
        changed = False
        for equals in range(function.body_open + 1, function.body_close):
            if tokens[equals].value != "=" or _is_inside(equals, lambda_ranges):
                continue
            left = statement_left(equals)
            target = target_before(equals, left)
            if target is None or target in provenance:
                continue
            end = equals + 1
            depth = 0
            while end < function.body_close:
                value = tokens[end].value
                if value in {"(", "[", "{"}:
                    depth += 1
                elif value in {")", "]", "}"}:
                    depth = max(0, depth - 1)
                if value == ";" and depth == 0:
                    break
                end += 1
            source = _exact_pointer_root(
                tokens, equals + 1, end, set(provenance), forward
            )
            if source is not None:
                provenance[target] = (provenance[source[0]][0], equals)
                changed = True
    return provenance


def _call_argument_ranges(
    tokens: Sequence[Token], lparen: int, rparen: int, forward: dict[int, int]
) -> list[tuple[int, int]]:
    result: list[tuple[int, int]] = []
    left = lparen + 1
    cursor = left
    while cursor < rparen:
        if tokens[cursor].value in {"(", "[", "{"} and cursor in forward:
            cursor = forward[cursor] + 1
            continue
        if tokens[cursor].value == ",":
            result.append((left, cursor))
            left = cursor + 1
        cursor += 1
    result.append((left, rparen))
    return result


def _exact_pointer_root(
    tokens: Sequence[Token],
    left: int,
    right: int,
    allowed_roots: set[str],
    forward: dict[int, int],
) -> tuple[str, int] | None:
    """Recognize one reviewable pointer expression rooted in an allowed value."""

    while left < right and tokens[left].value == "(" and forward.get(left) == right - 1:
        left += 1
        right -= 1

    casts = {"const_cast", "dynamic_cast", "reinterpret_cast", "static_cast"}
    while left + 3 < right and tokens[left].value in casts:
        if tokens[left + 1].value != "<":
            return None
        cursor = left + 2
        depth = 1
        while cursor < right and depth:
            if tokens[cursor].value == "<":
                depth += 1
            elif tokens[cursor].value == ">":
                depth -= 1
            elif tokens[cursor].value == ">>":
                depth = max(0, depth - 2)
            cursor += 1
        if (
            depth
            or cursor >= right
            or tokens[cursor].value != "("
            or forward.get(cursor) != right - 1
        ):
            return None
        left = cursor + 1
        right -= 1
        while (
            left < right
            and tokens[left].value == "("
            and forward.get(left) == right - 1
        ):
            left += 1
            right -= 1

    if left >= right:
        return None
    values = [token.value for token in tokens[left:right]]
    if set(values) & {
        ",",
        "?",
        ":",
        "&&",
        "||",
        "=",
        "+=",
        "-=",
        "*=",
        "/=",
        "%=",
        "&=",
        "|=",
        "^=",
        "++",
        "--",
    }:
        return None

    # Calls can select or replace a pointer even when an allowed root appears
    # in their argument list. Only scalar compile-time operators are harmless.
    for index in range(left, right):
        if tokens[index].value != "(" or index == left:
            continue
        previous = tokens[index - 1].value
        if previous in {"sizeof", "alignof", "decltype"}:
            continue
        if tokens[index - 1].kind == "identifier" or previous in {")", "]", ">", ">>"}:
            return None

    roots = [
        (tokens[index].value, index)
        for index in range(left, right)
        if tokens[index].value in allowed_roots
        and _unqualified_output_identifier(tokens, index)
    ]
    return roots[0] if len(roots) == 1 else None


def _transfer_size_is_positive(
    tokens: Sequence[Token], left: int, right: int, forward: dict[int, int]
) -> bool:
    while left < right and tokens[left].value == "(" and forward.get(left) == right - 1:
        left += 1
        right -= 1
    values = [token.value for token in tokens[left:right]]
    if not values or set(values) & {",", "?", ":", "false", "nullptr", "NULL"}:
        return False

    def integer_literal(value: str) -> int | None:
        rendered = re.sub(r"[uUlL]+$", "", value)
        try:
            return int(rendered, 0)
        except ValueError:
            return None

    if len(values) == 1:
        literal = integer_literal(values[0])
        return literal is None or literal > 0
    for index, value in enumerate(values):
        if integer_literal(value) != 0:
            continue
        if (index > 0 and values[index - 1] == "*") or (
            index + 1 < len(values) and values[index + 1] == "*"
        ):
            return False
    return True


def _pointer_root_reassigned(
    tokens: Sequence[Token],
    root: str,
    left: int,
    right: int,
    lambda_ranges: Sequence[tuple[int, int]],
) -> bool:
    for index in range(max(0, left), min(right, len(tokens))):
        if (
            tokens[index].value != root
            or not _unqualified_output_identifier(tokens, index)
            or _is_inside(index, lambda_ranges)
        ):
            continue
        previous = tokens[index - 1].value if index > 0 else ""
        following = tokens[index + 1].value if index + 1 < right else ""
        if previous in {"++", "--"} or following in {"++", "--"}:
            return True
        if following in OUTPUT_ASSIGNMENTS and previous != "*":
            return True
    return False


def _proven_output_transfers(
    tokens: Sequence[Token],
    function: Function,
    output_roots: set[str],
    lambda_ranges: Sequence[tuple[int, int]],
    regions: Sequence[_Region],
    kernel_writes: Sequence[_KernelWriteEvidence],
) -> tuple[
    list[_DispatchEvidence],
    set[int],
    set[int],
    dict[int, frozenset[str]],
]:
    """Prove kernel-written USM reaches an ABI output without host compute."""

    forward, _ = _delimiter_pairs(tokens)
    provenance = _local_usm_provenance(tokens, function, lambda_ranges)
    evidence: list[_DispatchEvidence] = []
    proven_destination_indices: set[int] = set()
    proven_call_indices: set[int] = set()
    destinations: dict[int, frozenset[str]] = {}

    for method_index in range(function.body_open + 1, function.body_close):
        if tokens[method_index].value != "memcpy" or _is_inside(
            method_index, lambda_ranges
        ):
            continue
        is_member = method_index >= 2 and tokens[method_index - 1].value in {
            ".",
            "->",
        }
        is_std = (
            method_index >= 2
            and tokens[method_index - 1].value == "::"
            and tokens[method_index - 2].value == "std"
        )
        if not (is_member or is_std):
            continue
        call = _method_lparen(tokens, method_index, function.body_close, forward)
        if call is None:
            continue
        lparen, rparen = call
        arguments = _call_argument_ranges(tokens, lparen, rparen, forward)
        if len(arguments) != 3 or not _transfer_size_is_positive(
            tokens, *arguments[2], forward
        ):
            continue
        destination = _exact_pointer_root(tokens, *arguments[0], output_roots, forward)
        source = _exact_pointer_root(tokens, *arguments[1], set(provenance), forward)
        if destination is None or source is None:
            continue
        destination_roots = {destination[0]}
        source_roots = {source[0]}
        if is_member:
            receiver = tokens[method_index - 2].value
            if _receiver_kind(
                tokens, function, receiver, method_index, forward
            ) != "queue" or not _call_is_awaited(
                tokens, rparen, function.body_close, forward
            ):
                continue
        elif any(provenance[root][0] != "shared" for root in source_roots):
            continue

        transfer_context = _context(method_index, regions)

        def staging_source_is_host_mutated(launch: _KernelWriteEvidence) -> bool:
            if any(
                _pointer_root_reassigned(
                    tokens,
                    root,
                    launch.index + 1,
                    method_index,
                    lambda_ranges,
                )
                for root in source_roots
            ):
                return True
            for index in range(launch.index + 1, method_index):
                if _is_inside(index, lambda_ranges):
                    continue
                if (
                    tokens[index].value in source_roots
                    and _unqualified_output_identifier(tokens, index)
                    and _output_write_operator(tokens, index, method_index, forward)
                    is not None
                ):
                    return True
                if tokens[index].kind != "identifier" or tokens[index].value in {
                    *CALL_KEYWORDS,
                    *DISPATCH_METHODS,
                    "wait",
                    "wait_and_throw",
                }:
                    continue
                nested_call = _method_lparen(tokens, index, method_index, forward)
                if nested_call is not None and _range_references_output(
                    tokens, nested_call[0] + 1, nested_call[1], source_roots
                ):
                    return True
            return False

        producers = [
            launch
            for launch in kernel_writes
            if launch.index < method_index
            and launch.awaited
            and source_roots.issubset(launch.roots)
            and all(provenance[root][1] < launch.index for root in source_roots)
            and not any(
                _pointer_root_reassigned(
                    tokens,
                    root,
                    provenance[root][1] + 1,
                    launch.index,
                    lambda_ranges,
                )
                for root in source_roots
            )
            and _context_dominates(launch.context, transfer_context)
            and not staging_source_is_host_mutated(launch)
        ]
        if not producers:
            continue
        kind = "awaited queue memcpy" if is_member else "shared-USM std::memcpy"
        roots = ", ".join(sorted(source_roots))
        evidence.append(
            _DispatchEvidence(
                method_index,
                tokens[method_index].line,
                "copyback",
                f"kernel-written {roots} reaches ABI output via {kind} at line "
                f"{tokens[method_index].line}",
                transfer_context,
            )
        )
        proven_destination_indices.add(destination[1])
        proven_call_indices.add(method_index)
        destinations[method_index] = frozenset(destination_roots)
    return evidence, proven_destination_indices, proven_call_indices, destinations


def _deferred_output_writes(
    tokens: Sequence[Token],
    function: Function,
    roots: set[str],
    lambda_ranges: Sequence[tuple[int, int]],
    device_lambdas: set[tuple[int, int]],
) -> list[int]:
    forward, _ = _delimiter_pairs(tokens)
    lines: set[int] = set()
    for index in range(function.body_open + 1, function.body_close):
        if tokens[index].value not in roots or not _unqualified_output_identifier(
            tokens, index
        ):
            continue
        containing = [
            bounds for bounds in lambda_ranges if bounds[0] < index < bounds[1]
        ]
        if not containing:
            continue
        innermost = max(containing, key=lambda bounds: bounds[0])
        if innermost in device_lambdas:
            continue
        if (
            _output_write_operator(tokens, index, function.body_close, forward)
            is not None
        ):
            lines.add(tokens[index].line)
    return sorted(lines)


def _unresolved_member_output_calls(
    tokens: Sequence[Token],
    function: Function,
    roots: set[str],
    lambda_ranges: Sequence[tuple[int, int]],
) -> list[tuple[str, int]]:
    forward, _ = _delimiter_pairs(tokens)
    calls: set[tuple[str, int]] = set()
    for method_index in range(function.body_open + 2, function.body_close):
        if (
            tokens[method_index].kind != "identifier"
            or tokens[method_index - 1].value not in {".", "->"}
            or tokens[method_index].value in DISPATCH_METHODS | {"memcpy", "copy"}
            or _is_inside(method_index, lambda_ranges)
        ):
            continue
        call = _method_lparen(tokens, method_index, function.body_close, forward)
        if call is None:
            continue
        receiver_index = method_index - 2
        receiver = tokens[receiver_index].value
        if (
            receiver in roots and _unqualified_output_identifier(tokens, receiver_index)
        ) or _range_references_output(tokens, call[0] + 1, call[1], roots):
            calls.add((tokens[method_index].value, tokens[method_index].line))
    return sorted(calls, key=lambda item: (item[1], item[0]))


def _unresolved_indirect_output_calls(
    tokens: Sequence[Token],
    function: Function,
    roots: set[str],
    lambda_ranges: Sequence[tuple[int, int]],
) -> list[tuple[str, int]]:
    """Find parenthesized callable expressions such as ``(*fn)(out)``."""

    forward, reverse = _delimiter_pairs(tokens)
    calls: set[tuple[str, int]] = set()
    for lparen in range(function.body_open + 1, function.body_close):
        if (
            tokens[lparen].value != "("
            or lparen not in forward
            or lparen == 0
            or tokens[lparen - 1].value != ")"
            or lparen - 1 not in reverse
            or _is_inside(lparen, lambda_ranges)
        ):
            continue
        callable_open = reverse[lparen - 1]
        if callable_open < function.body_open:
            continue
        close = forward[lparen]
        if not _range_references_output(tokens, lparen + 1, close, roots):
            continue
        callable_tokens = tokens[callable_open + 1 : lparen - 1]
        identifiers = [
            token.value for token in callable_tokens if token.kind == "identifier"
        ]
        name = identifiers[-1] if identifiers else "<indirect>"
        calls.add((name, tokens[lparen].line))
    return sorted(calls, key=lambda item: (item[1], item[0]))


def _output_shadow_lines(
    tokens: Sequence[Token], function: Function, mutable: set[str]
) -> list[int]:
    """Fail closed when a local declaration shadows an ABI output parameter."""

    _, reverse = _delimiter_pairs(tokens)
    lines: set[int] = set()
    for index in range(function.body_open + 1, function.body_close):
        if tokens[index].value not in mutable:
            continue
        if (
            index >= 2
            and tokens[index - 2].value == "("
            and tokens[index - 1].value == "void"
            and index + 1 < function.body_close
            and tokens[index + 1].value == ")"
        ):
            continue
        if (
            index > 0
            and tokens[index - 1].value == ")"
            and index - 1 in reverse
            and [token.value for token in tokens[reverse[index - 1] + 1 : index - 1]]
            == ["void"]
        ):
            continue
        if _declaration_kind(tokens, index, function) is not None:
            lines.add(tokens[index].line)
    return sorted(lines)


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
        if call.qualified:
            return None, "qualified_helper"
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
                or (
                    candidate.required_parameter_count is not None
                    and candidate.required_parameter_count <= call.argument_count
                    and call.argument_count <= candidate.parameter_count
                )
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

        lambda_ranges = _lambda_ranges(
            self.tokens, function, self.forward, self.reverse
        )
        parameters, mutable = _parameter_names(self.tokens, function)
        zero_parameters = _zero_work_parameter_names(self.tokens, function, parameters)
        regions = _regions(
            self.tokens,
            function,
            self.forward,
            zero_parameters,
            parameters,
        )
        host_writes, transfer_lines, aliases = _host_output_accesses(
            self.tokens, function, mutable, lambda_ranges
        )
        output_roots = mutable | aliases
        direct, raw_launches, rejected, device_lambdas, kernel_writes = (
            _direct_dispatches(self.tokens, function, output_roots, regions)
        )
        (
            copyback_evidence,
            proven_destination_indices,
            proven_transfer_calls,
            copyback_destinations,
        ) = _proven_output_transfers(
            self.tokens,
            function,
            output_roots,
            lambda_ranges,
            regions,
            kernel_writes,
        )
        host_writes = [
            record
            for record in host_writes
            if record[0] not in proven_destination_indices
        ]
        transfer_records = [
            record
            for record in transfer_lines
            if record[0] not in proven_transfer_calls
        ]
        transfer_lines = sorted({line for _, line in transfer_records})
        shadow_lines = _output_shadow_lines(self.tokens, function, mutable)
        calls = _indexed_calls(self.tokens, function, lambda_ranges)
        safe_initializer_calls: set[int] = set()
        for indexed in calls:
            if indexed.call.name.rsplit("::", 1)[-1] != "memset":
                continue
            arguments = _call_argument_ranges(
                self.tokens, indexed.lparen, indexed.rparen, self.forward
            )
            if not arguments:
                continue
            initialized = {
                self.tokens[index].value
                for index in range(*arguments[0])
                if self.tokens[index].value in output_roots
                and _unqualified_output_identifier(self.tokens, index)
            }
            if any(
                transfer_index > indexed.index
                and initialized.intersection(destinations)
                and _context_dominates(
                    next(
                        item.context
                        for item in copyback_evidence
                        if item.index == transfer_index
                    ),
                    _context(indexed.index, regions),
                )
                for transfer_index, destinations in copyback_destinations.items()
            ):
                safe_initializer_calls.add(indexed.index)
        member_output_calls = _unresolved_member_output_calls(
            self.tokens, function, output_roots, lambda_ranges
        )
        indirect_output_calls = _unresolved_indirect_output_calls(
            self.tokens, function, output_roots, lambda_ranges
        )
        contract = LIFECYCLE_CONTRACTS.get(function.name) or FAIL_ONLY_CONTRACTS.get(
            function.name
        )
        allowed_contract_calls = (
            {
                sequence[-2]
                for sequence in (
                    contract.required_sequences
                    + getattr(contract, "required_any_sequences", ())
                )
                if len(sequence) >= 2 and sequence[-1] == "("
            }
            | set(getattr(contract, "allowed_output_calls", ()))
            if contract is not None
            else set()
        )
        contract_output_call_hazards = [
            f"unresolved output method {name} at line {line}"
            for name, line in member_output_calls
        ]
        contract_output_call_hazards.extend(
            f"unresolved indirect output call {name} at line {line}"
            for name, line in indirect_output_calls
        )
        contract_output_call_hazards.extend(
            f"unapproved output call {indexed.call.name} at line {indexed.call.line}"
            for indexed in calls
            if indexed.index not in proven_transfer_calls | safe_initializer_calls
            if indexed.call.name.rsplit("::", 1)[-1] not in allowed_contract_calls
            and _range_references_output(
                self.tokens, indexed.lparen + 1, indexed.rparen, output_roots
            )
        )
        control_lines = sorted(
            (
                (token.value, token.line)
                for index, token in enumerate(self.tokens)
                if function.body_open < index < function.body_close
                and token.value in {"switch", "goto", "?"}
                and not _is_inside(index, lambda_ranges)
            ),
            key=lambda item: (item[1], item[0]),
        )

        lifecycle = _lifecycle_proof(
            self.tokens,
            function,
            regions,
            lambda_ranges,
            host_writes,
            control_lines,
            contract_output_call_hazards,
            shadow_lines,
        )
        if lifecycle is not None:
            self.cache[function] = lifecycle
            return lifecycle
        fail_contract = _fail_only_contract_proof(
            self.tokens,
            function,
            regions,
            lambda_ranges,
            host_writes,
            control_lines,
            contract_output_call_hazards,
            shadow_lines,
        )
        if fail_contract is not None:
            self.cache[function] = fail_contract
            return fail_contract

        deferred_write_lines = _deferred_output_writes(
            self.tokens,
            function,
            output_roots,
            lambda_ranges,
            device_lambdas,
        )
        classifications: set[str] = set()
        details: list[str] = []
        if direct:
            classifications.update({"device_dispatch", "large_input_gpu_chain"})
            details.extend(item.detail for item in direct)
        else:
            classifications.add("missing_device_terminal")
        if copyback_evidence:
            classifications.update(
                {"device_copyback", "device_dispatch", "large_input_gpu_chain"}
            )
            details.extend(item.detail for item in copyback_evidence)
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
        counter_indices = [
            index
            for index in range(function.body_open + 1, function.body_close - 1)
            if self.tokens[index].value == "pgaccel_record_gpu_exec"
            and self.tokens[index + 1].value == "("
        ]
        counter_lines = sorted({self.tokens[index].line for index in counter_indices})
        if counter_lines:
            classifications.add(
                "gpu_exec_observability"
                if direct or copyback_evidence
                else "fake_gpu_counter"
            )
            details.append(
                "GPU execution counter is observability at line(s) "
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

        zero_ranges = [region for region in regions if region.zero_work]
        unsafe_write_records = [
            record
            for record in host_writes
            if not (
                record[2]
                and any(
                    region.start <= record[0] <= region.end for region in zero_ranges
                )
            )
        ]
        unsafe_write_lines = sorted({record[1] for record in unsafe_write_records})
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
        if deferred_write_lines:
            classifications.update(
                {"deferred_host_output_write", "host_computation", "review_required"}
            )
            details.append(
                "non-device lambda writes ABI output at line(s) "
                + ", ".join(map(str, deferred_write_lines))
            )

        if aliases:
            classifications.add("output_alias_tracking")
            details.append("ABI output alias(es): " + ", ".join(sorted(aliases)))

        if shadow_lines:
            classifications.update({"output_identity_shadowing", "review_required"})
            details.append(
                "local declaration shadows ABI output at line(s) "
                + ", ".join(map(str, shadow_lines))
            )

        if control_lines:
            classifications.update({"ambiguous_control_flow", "review_required"})
            details.append(
                "unmodeled control flow: "
                + ", ".join(f"{kind} at line {line}" for kind, line in control_lines)
            )

        if "if" in body_values and "constexpr" in body_values:
            classifications.update(
                {"template_specialization_review", "review_required"}
            )
            details.append("if constexpr paths require a concrete specialization proof")

        # A successful helper call used as a statement can establish evidence
        # only when it receives one of this function's mutable ABI outputs.
        call_evidence: list[_DispatchEvidence] = []
        unsafe_contributing_helpers = [
            f"unresolved output method {name} at line {line} may finalize or overwrite the ABI result"
            for name, line in member_output_calls
        ]
        unsafe_contributing_helpers.extend(
            f"unresolved indirect output call {name} at line {line} may finalize or overwrite the ABI result"
            for name, line in indirect_output_calls
        )
        if member_output_calls or indirect_output_calls:
            classifications.update({"unresolved_output_helper", "review_required"})
        if indirect_output_calls:
            classifications.add("unresolved_indirect_output_call")
        call_proofs: dict[int, tuple[_IndexedCall, _Proof | None, str | None]] = {}
        for indexed in calls:
            if indexed.index in proven_transfer_calls | safe_initializer_calls:
                continue
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
                and _range_references_output(
                    self.tokens, indexed.lparen + 1, indexed.rparen, output_roots
                )
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
                and _range_references_output(
                    self.tokens, indexed.lparen + 1, indexed.rparen, output_roots
                )
            ):
                classifications.update(proof.classifications)
                unsafe_contributing_helpers.append(
                    f"output helper {candidate.name} at line {indexed.call.line} is not "
                    "independently device-proven"
                )
            if (
                proof is not None
                and proof.ok
                and _range_references_output(
                    self.tokens, indexed.lparen + 1, indexed.rparen, output_roots
                )
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
            if proof is None and _range_references_output(
                self.tokens, indexed.lparen + 1, indexed.rparen, output_roots
            ):
                classifications.update(
                    {
                        error or "unresolved_helper",
                        "unresolved_output_helper",
                        "review_required",
                    }
                )
                unsafe_contributing_helpers.append(
                    f"unresolved output helper {indexed.call.name} at line "
                    f"{indexed.call.line} may finalize or overwrite the ABI result"
                )

        all_evidence = sorted(
            direct + copyback_evidence + call_evidence,
            key=lambda item: item.index,
        )
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
            if (
                zero
                and not host_loop_lines
                and not control_lines
                and not any(
                    region.start <= index <= region.end
                    for region in zero_ranges
                    if region.start <= return_index <= region.end
                    for index in counter_indices
                )
                and not any(
                    region.start <= record[0] <= region.end and not record[2]
                    for region in zero_ranges
                    for record in host_writes
                )
            ):
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

        failure_only_hazards = bool(
            host_loop_lines
            or host_writes
            or transfer_lines
            or counter_lines
            or control_lines
            or rejected
            or deferred_write_lines
            or member_output_calls
            or indirect_output_calls
            or shadow_lines
            or unsafe_contributing_helpers
        )
        if (
            function.is_status
            and returns
            and success_paths == 0
            and not failure_only_hazards
        ):
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
            or control_lines
            or deferred_write_lines
            or shadow_lines
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


def _audit_source_literal(
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
    findings = _macro_inventory_findings(path, directives)
    ambiguous_directives = _ambiguous_preprocessor_directives(directives)
    for directive in ambiguous_directives:
        findings.append(
            Finding(
                path,
                directive.line,
                "<preprocessor>",
                f"preprocessor condition is not compiler-proven: {directive.text.splitlines()[0]}",
                ("ambiguous_preprocessor_condition", "review_required"),
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
        if ambiguous_directives:
            condition_lines = ", ".join(
                str(directive.line) for directive in ambiguous_directives
            )
            proof = _Proof(
                False,
                _bounded_detail(
                    (
                        proof.detail,
                        "ambiguous preprocessor condition(s) at line(s) "
                        + condition_lines,
                    )
                ),
                tuple(
                    sorted(
                        set(proof.classifications)
                        | {"ambiguous_preprocessor_condition", "review_required"}
                    )
                ),
            )
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


def audit_source(
    path: pathlib.Path, source: str, *, require_entrypoint: bool = True
) -> FileAudit:
    """Audit both literal and compiler-expanded main-file source views."""

    literal = _audit_source_literal(path, source, require_entrypoint=require_entrypoint)
    try:
        _, directives = normalize_preprocessor(source)
    except ParseError:
        return literal
    if not any(
        _directive_parts(directive)[0] in {"define", "include"}
        for directive in directives
    ):
        return literal
    try:
        expanded_source = _compiler_preprocess_source(path, source)
        expanded = _audit_source_literal(
            path, expanded_source, require_entrypoint=require_entrypoint
        )
    except ParseError as error:
        detail = str(error)
        entrypoint_audits = tuple(
            dataclasses.replace(
                entry,
                ok=False,
                classifications=tuple(
                    sorted(
                        set(entry.classifications)
                        | {"preprocessor_expansion_error", "review_required"}
                    )
                ),
                detail=_bounded_detail((entry.detail, detail)),
            )
            for entry in literal.entrypoint_audits
        )
        findings = list(literal.findings)
        findings.extend(
            Finding(
                path,
                entry.line,
                entry.entrypoint,
                entry.detail,
                entry.classifications,
            )
            for entry in entrypoint_audits
            if entry.ok is False
            and not any(
                finding.entrypoint == entry.entrypoint for finding in literal.findings
            )
        )
        findings.append(
            Finding(
                path,
                1,
                "<preprocessor-expansion>",
                detail,
                ("preprocessor_expansion_error", "review_required"),
            )
        )
        return dataclasses.replace(
            literal,
            entrypoint_audits=entrypoint_audits,
            findings=tuple(findings),
            lifecycle_contracts=0,
        )

    expanded_by_name = {entry.entrypoint: entry for entry in expanded.entrypoint_audits}
    merged_entries: list[EntrypointAudit] = []
    findings = list(literal.findings)
    literal_failed = {finding.entrypoint for finding in literal.findings}
    for entry in literal.entrypoint_audits:
        expanded_entry = expanded_by_name.get(entry.entrypoint)
        if expanded_entry is None:
            merged = dataclasses.replace(
                entry,
                ok=False,
                classifications=tuple(
                    sorted(
                        set(entry.classifications)
                        | {"preprocessor_expansion_mismatch", "review_required"}
                    )
                ),
                detail=_bounded_detail(
                    (
                        entry.detail,
                        "compiler-expanded source omitted this literal export",
                    )
                ),
            )
        elif expanded_entry.ok:
            merged = entry
        else:
            merged = dataclasses.replace(
                entry,
                ok=False,
                classifications=tuple(
                    sorted(
                        set(entry.classifications)
                        | set(expanded_entry.classifications)
                        | {"macro_expanded_body", "review_required"}
                    )
                ),
                detail=_bounded_detail(
                    (
                        entry.detail,
                        "compiler-expanded body: " + expanded_entry.detail,
                    )
                ),
            )
        merged_entries.append(merged)
        if not merged.ok and merged.entrypoint not in literal_failed:
            findings.append(
                Finding(
                    path,
                    merged.line,
                    merged.entrypoint,
                    merged.detail,
                    merged.classifications,
                )
            )
    literal_names = {entry.entrypoint for entry in literal.entrypoint_audits}
    for entry in expanded.entrypoint_audits:
        if entry.entrypoint in literal_names:
            continue
        findings.append(
            Finding(
                path,
                entry.line,
                entry.entrypoint,
                "compiler expansion introduced an export absent from literal inventory",
                ("macro_hidden_export", "preprocessor_expansion_mismatch"),
            )
        )
    return dataclasses.replace(
        literal,
        entrypoint_audits=tuple(merged_entries),
        findings=tuple(findings),
        lifecycle_contracts=sum(
            entry.ok and "lifecycle" in entry.classifications
            for entry in merged_entries
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


def _parameter_segments(
    tokens: Sequence[Token], lparen: int, rparen: int
) -> list[tuple[int, int]]:
    forward, _ = _delimiter_pairs(tokens)
    segments: list[tuple[int, int]] = []
    start = lparen + 1
    cursor = start
    while cursor < rparen:
        if tokens[cursor].value in {"(", "[", "{"} and cursor in forward:
            cursor = forward[cursor] + 1
            continue
        if tokens[cursor].value == ",":
            segments.append((start, cursor))
            start = cursor + 1
        cursor += 1
    segments.append((start, rparen))
    return segments


def _parameter_type_spellings(
    tokens: Sequence[Token], lparen: int, rparen: int
) -> tuple[str, ...]:
    result: list[str] = []
    for left, right in _parameter_segments(tokens, lparen, rparen):
        segment = list(tokens[left:right])
        if not segment or [token.value for token in segment] == ["void"]:
            continue
        depth = 0
        for index, token in enumerate(segment):
            if token.value in {"(", "[", "{"}:
                depth += 1
            elif token.value in {")", "]", "}"}:
                depth = max(0, depth - 1)
            elif token.value == "=" and depth == 0:
                segment = segment[:index]
                break
        identifiers = [
            index for index, token in enumerate(segment) if token.kind == "identifier"
        ]
        if len(identifiers) >= 2:
            del segment[identifiers[-1]]
        result.append(" ".join(token.value for token in segment).strip())
    return tuple(result)


def _full_signature(return_spelling: str, parameter_types: Sequence[str]) -> str:
    return (
        " ".join(return_spelling.split())
        + " ("
        + ", ".join(" ".join(parameter.split()) for parameter in parameter_types)
        + ")"
    )


def _canonical_signature_tokens(signature: str) -> tuple[str, ...]:
    return tuple(
        token.value for token in lex_cpp(signature) if token.value not in {"struct"}
    )


def _symbol_from_tokens(
    path: pathlib.Path,
    line: int,
    name: str,
    return_spelling: str,
    tokens: Sequence[Token],
    lparen: int,
    rparen: int,
    origin: str,
) -> AbiSymbol:
    parameter_types = _parameter_type_spellings(tokens, lparen, rparen)
    return AbiSymbol(
        path,
        line,
        name,
        return_spelling,
        _parameter_count(tokens, lparen, rparen),
        parameter_types,
        _full_signature(return_spelling, parameter_types),
        origin,
    )


def parse_declarations(
    path: pathlib.Path, source: str
) -> tuple[list[AbiSymbol], list[Finding]]:
    normalized, directives = normalize_preprocessor(source)
    tokens = lex_cpp(normalized)
    forward, _ = _delimiter_pairs(tokens)
    extern_braces = _extern_c_braces(tokens)
    parents = _brace_parents(tokens)
    symbols: list[AbiSymbol] = []
    findings = _macro_inventory_findings(path, directives, header=True)
    findings.extend(
        Finding(
            path,
            directive.line,
            "<preprocessor>",
            f"header ABI depends on an unproven condition: {directive.text.splitlines()[0]}",
            ("ambiguous_preprocessor_condition", "abi_inventory_mismatch"),
        )
        for directive in _ambiguous_preprocessor_directives(directives)
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
            _symbol_from_tokens(
                path,
                token.line,
                token.value,
                _declaration_return_spelling(
                    tokens, signature_start, name_index, rparen, cursor
                ),
                tokens,
                lparen,
                rparen,
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
        _symbol_from_tokens(
            path,
            function.line,
            function.name,
            function.return_spelling,
            tokens,
            function.lparen,
            function.rparen,
            "source_definition",
        )
        for function in parse_functions(tokens)
        if function.is_export
    ]


def _canonical_symbol(symbol: AbiSymbol) -> tuple[str, tuple[str, ...]]:
    return symbol.name, _canonical_signature_tokens(symbol.full_signature)


def _inventory_hash(symbols: Iterable[AbiSymbol]) -> str:
    rows = sorted(_canonical_symbol(symbol) for symbol in symbols)
    encoded = json.dumps(rows, separators=(",", ":"), ensure_ascii=True).encode("ascii")
    return hashlib.sha256(encoded).hexdigest()


def load_abi_manifest(path: pathlib.Path) -> AbiManifest:
    try:
        raw = path.read_bytes()
        text = raw.decode("utf-8")
    except (OSError, UnicodeError) as error:
        raise ParseError(f"cannot read ABI signature manifest: {error}") from error
    digest = hashlib.sha256(raw).hexdigest()
    if digest != EXPECTED_ABI_MANIFEST_SHA256:
        raise ParseError(
            "ABI signature manifest SHA-256 mismatch: "
            f"expected {EXPECTED_ABI_MANIFEST_SHA256}, got {digest}"
        )
    if not text.endswith("\n"):
        raise ParseError("ABI signature manifest must end with a newline")
    signatures: dict[str, str] = {}
    rows = text.splitlines()
    for line_number, row in enumerate(rows, 1):
        name, separator, signature = row.partition("|")
        if (
            not separator
            or not name.startswith("pgaccel_")
            or not name.isidentifier()
            or not signature
        ):
            raise ParseError(f"invalid ABI signature manifest row {line_number}")
        if name in signatures:
            raise ParseError(f"duplicate ABI signature manifest symbol {name}")
        signatures[name] = signature
    if len(signatures) != EXPECTED_ABI_MANIFEST_COUNT:
        raise ParseError(
            "ABI signature manifest count mismatch: "
            f"expected {EXPECTED_ABI_MANIFEST_COUNT}, got {len(signatures)}"
        )
    if list(signatures) != sorted(signatures):
        raise ParseError("ABI signature manifest rows must be sorted by symbol")
    return AbiManifest(path, len(signatures), digest, signatures)


def _symbol_from_full_signature(
    path: pathlib.Path,
    line: int,
    name: str,
    signature: str,
    origin: str,
) -> AbiSymbol:
    tokens = lex_cpp(signature)
    forward, _ = _delimiter_pairs(tokens)
    lparen = next(
        (index for index, token in enumerate(tokens) if token.value == "("), None
    )
    if lparen is None or lparen not in forward or forward[lparen] != len(tokens) - 1:
        raise ParseError(f"invalid full ABI signature for {name}: {signature}")
    rparen = forward[lparen]
    return_spelling = " ".join(token.value for token in tokens[:lparen])
    parameter_types = tuple(
        " ".join(token.value for token in tokens[left:right])
        for left, right in _parameter_segments(tokens, lparen, rparen)
        if left != right
    )
    return AbiSymbol(
        path,
        line,
        name,
        return_spelling,
        len(parameter_types),
        parameter_types,
        signature,
        origin,
    )


def compiler_header_inventory(
    header_paths: Sequence[pathlib.Path], compiler: str | None = None
) -> CompilerAbiInventory:
    public_headers = [
        path.resolve()
        for path in sorted(
            dict.fromkeys(header_paths), key=lambda item: item.as_posix()
        )
        if path.name not in INTERNAL_NON_ABI_HEADERS
    ]
    if not public_headers:
        raise ParseError("no public ABI headers supplied for compiler inventory")
    selected = (
        compiler or os.environ.get("PGACCEL_ABI_CLANG") or shutil.which("clang++")
    )
    if selected is None:
        raise ParseError("clang++ is required for compiler-backed ABI inventory")
    compiler_path = str(pathlib.Path(selected).resolve())
    umbrella = "".join(
        '#include "' + str(path).replace("\\", "\\\\").replace('"', '\\"') + '"\n'
        for path in public_headers
    )
    include_dirs = sorted({str(path.parent) for path in public_headers})
    command = [
        compiler_path,
        "-std=c++17",
        "-x",
        "c++",
        *(argument for directory in include_dirs for argument in ("-I", directory)),
        "-Xclang",
        "-ast-dump=json",
        "-fsyntax-only",
        "-",
    ]
    try:
        completed = subprocess.run(
            command,
            input=umbrella,
            text=True,
            capture_output=True,
            check=False,
            timeout=120,
        )
        version = subprocess.run(
            [compiler_path, "--version"],
            text=True,
            capture_output=True,
            check=False,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ParseError(f"compiler-backed ABI inventory failed: {error}") from error
    if completed.returncode != 0:
        diagnostic = completed.stderr.strip()[-4000:]
        raise ParseError(
            f"compiler-backed ABI inventory exited {completed.returncode}: {diagnostic}"
        )
    try:
        root = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise ParseError(f"compiler ABI AST was not valid JSON: {error}") from error

    discovered: dict[tuple[str, tuple[str, ...]], AbiSymbol] = {}

    def visit(node: object) -> None:
        if isinstance(node, dict):
            if node.get("kind") == "FunctionDecl":
                name = str(node.get("name", ""))
                mangled = str(node.get("mangledName", ""))
                signature = str(
                    node.get("type", {}).get("qualType", "")
                    if isinstance(node.get("type"), dict)
                    else ""
                )
                if (
                    name.startswith("pgaccel_")
                    and mangled.lstrip("_") == name
                    and signature
                ):
                    location = node.get("loc", {})
                    line = (
                        int(location.get("line", 1))
                        if isinstance(location, dict)
                        else 1
                    )
                    symbol = _symbol_from_full_signature(
                        public_headers[0],
                        line,
                        name,
                        signature,
                        "clang_ast_declaration",
                    )
                    discovered[_canonical_symbol(symbol)] = symbol
            for value in node.values():
                visit(value)
        elif isinstance(node, list):
            for value in node:
                visit(value)

    visit(root)
    symbols = tuple(
        sorted(
            discovered.values(), key=lambda symbol: (symbol.name, symbol.full_signature)
        )
    )
    if not symbols:
        raise ParseError(
            "compiler ABI AST contained no extern-C pgaccel_* declarations"
        )
    version_line = (version.stdout or version.stderr).splitlines()
    return CompilerAbiInventory(
        symbols,
        compiler_path,
        version_line[0] if version_line else "<unknown>",
        tuple(command),
        hashlib.sha256(umbrella.encode("utf-8")).hexdigest(),
        completed.stderr.strip(),
    )


def _compare_symbols_to_manifest(
    symbols: Sequence[AbiSymbol],
    manifest: AbiManifest,
    *,
    origin: str,
    fallback_path: pathlib.Path,
) -> list[Finding]:
    findings: list[Finding] = []
    actual: dict[str, set[tuple[str, ...]]] = defaultdict(set)
    locations: dict[str, AbiSymbol] = {}
    for symbol in symbols:
        actual[symbol.name].add(_canonical_signature_tokens(symbol.full_signature))
        locations.setdefault(symbol.name, symbol)
    expected = {
        name: _canonical_signature_tokens(signature)
        for name, signature in manifest.signatures.items()
    }
    for name in sorted(set(expected) - set(actual)):
        findings.append(
            Finding(
                fallback_path,
                1,
                name,
                f"{origin} is missing immutable-manifest symbol {name}",
                ("missing_manifest_symbol", "abi_inventory_mismatch"),
            )
        )
    for name in sorted(set(actual) - set(expected)):
        symbol = locations[name]
        findings.append(
            Finding(
                symbol.path,
                symbol.line,
                name,
                f"{origin} contains symbol absent from immutable manifest",
                ("extra_manifest_symbol", "abi_inventory_mismatch"),
            )
        )
    for name in sorted(set(actual) & set(expected)):
        if expected[name] not in actual[name] or len(actual[name]) != 1:
            symbol = locations[name]
            actual_signatures = sorted(
                item.full_signature for item in symbols if item.name == name
            )
            findings.append(
                Finding(
                    symbol.path,
                    symbol.line,
                    name,
                    f"{origin} signature {actual_signatures!r} does not match immutable manifest {manifest.signatures[name]!r}",
                    ("abi_signature_mismatch", "abi_inventory_mismatch"),
                )
            )
    return findings


def _object_abi_evidence(
    object_paths: Sequence[pathlib.Path], manifest: AbiManifest
) -> tuple[tuple[dict[str, object], ...], list[Finding]]:
    nm = shutil.which("nm")
    if nm is None:
        return (
            ({"status": "unavailable", "error": "nm was not found"},),
            [
                Finding(
                    object_paths[0] if object_paths else manifest.path,
                    1,
                    "<object-inventory>",
                    "nm is required for requested object ABI evidence",
                    ("object_inventory_error", "abi_inventory_mismatch"),
                )
            ],
        )
    try:
        version = subprocess.run(
            [nm, "--version"],
            text=True,
            capture_output=True,
            check=False,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        version_line = "<unknown>"
    else:
        lines = (version.stdout or version.stderr).splitlines()
        version_line = lines[0] if lines else "<unknown>"

    evidence: list[dict[str, object]] = []
    findings: list[Finding] = []
    expected = set(manifest.signatures)
    combined_names: set[str] = set()
    collected_paths: list[str] = []
    for path in sorted(dict.fromkeys(object_paths), key=lambda item: item.as_posix()):
        command = [nm, "-g", str(path)]
        try:
            completed = subprocess.run(
                command,
                text=True,
                capture_output=True,
                check=False,
                timeout=60,
            )
            binary_sha256 = hashlib.sha256(path.read_bytes()).hexdigest()
        except (OSError, subprocess.TimeoutExpired) as error:
            findings.append(
                Finding(
                    path,
                    1,
                    "<object-inventory>",
                    f"object ABI inventory failed: {error}",
                    ("object_inventory_error", "abi_inventory_mismatch"),
                )
            )
            evidence.append(
                {"path": path.as_posix(), "status": "error", "error": str(error)}
            )
            continue
        if completed.returncode != 0:
            diagnostic = completed.stderr.strip()[-2000:]
            findings.append(
                Finding(
                    path,
                    1,
                    "<object-inventory>",
                    f"nm exited {completed.returncode}: {diagnostic}",
                    ("object_inventory_error", "abi_inventory_mismatch"),
                )
            )
            evidence.append(
                {
                    "path": path.as_posix(),
                    "status": "error",
                    "command": command,
                    "stderr": diagnostic,
                }
            )
            continue
        names: set[str] = set()
        for line in completed.stdout.splitlines():
            parts = line.split()
            if len(parts) < 2:
                continue
            symbol = parts[-1].lstrip("_")
            symbol_type = parts[-2]
            if (
                symbol.startswith("pgaccel_")
                and len(symbol_type) == 1
                and symbol_type.upper() in {"B", "D", "R", "S", "T", "V", "W"}
                and symbol_type != "U"
            ):
                names.add(symbol)
        combined_names.update(names)
        collected_paths.append(path.as_posix())
        evidence.append(
            {
                "kind": "object",
                "path": path.as_posix(),
                "status": "collected",
                "command": command,
                "nm_version": version_line,
                "binary_sha256": binary_sha256,
                "count": len(names),
                "names_sha256": hashlib.sha256(
                    ("\n".join(sorted(names)) + "\n").encode("ascii")
                ).hexdigest(),
            }
        )
    if collected_paths:
        missing = sorted(expected - combined_names)
        extra = sorted(combined_names - expected)
        evidence.append(
            {
                "kind": "combined_object_inventory",
                "status": "verified" if not (missing or extra) else "mismatch",
                "paths": collected_paths,
                "count": len(combined_names),
                "names_sha256": hashlib.sha256(
                    ("\n".join(sorted(combined_names)) + "\n").encode("ascii")
                ).hexdigest(),
                "missing": missing,
                "extra": extra,
            }
        )
        if missing or extra:
            findings.append(
                Finding(
                    pathlib.Path(collected_paths[0]),
                    1,
                    "<object-inventory>",
                    f"combined object exports disagree with immutable manifest: {len(missing)} missing, {len(extra)} extra",
                    ("object_inventory_mismatch", "abi_inventory_mismatch"),
                )
            )
    return tuple(evidence), findings


def _release_object_validation(
    object_paths: Sequence[pathlib.Path],
    freshness_paths: Sequence[pathlib.Path],
    build_marker: pathlib.Path | None = None,
) -> tuple[tuple[dict[str, object], ...], list[Finding]]:
    """Validate the exact, freshly built shared kernel artifact fail-closed."""

    unique = list(dict.fromkeys(object_paths))
    findings: list[Finding] = []
    if len(unique) != 1:
        path = unique[0] if unique else pathlib.Path("pgaccel-kernels/build")
        findings.append(
            Finding(
                path,
                1,
                "<object-inventory>",
                f"release audit requires exactly one explicit shared kernel object; got {len(unique)}",
                ("missing_object_inventory", "abi_inventory_mismatch"),
            )
        )
        return (
            (
                {
                    "kind": "release_object_validation",
                    "status": "missing" if not unique else "invalid",
                    "paths": [path.as_posix() for path in unique],
                },
            ),
            findings,
        )

    path = unique[0]
    expected_name = path.name in {
        "libpgaccel_kernels_shared.dylib",
        "libpgaccel_kernels_shared.so",
    }
    try:
        object_stat = path.stat()
    except OSError as error:
        findings.append(
            Finding(
                path,
                1,
                "<object-inventory>",
                f"explicit shared kernel object is unavailable: {error}",
                ("object_inventory_error", "abi_inventory_mismatch"),
            )
        )
        return (
            (
                {
                    "kind": "release_object_validation",
                    "status": "missing",
                    "path": path.as_posix(),
                    "error": str(error),
                },
            ),
            findings,
        )

    if not expected_name or not path.is_file():
        findings.append(
            Finding(
                path,
                1,
                "<object-inventory>",
                "release object is not the exact pgaccel_kernels_shared library",
                ("wrong_release_object", "abi_inventory_mismatch"),
            )
        )

    marker_stat: os.stat_result | None = None
    if build_marker is None:
        findings.append(
            Finding(
                path,
                1,
                "<object-inventory>",
                "release audit requires the marker from the current clean rebuild",
                ("missing_build_marker", "abi_inventory_mismatch"),
            )
        )
    else:
        try:
            marker_stat = build_marker.stat()
        except OSError as error:
            findings.append(
                Finding(
                    build_marker,
                    1,
                    "<object-inventory>",
                    f"clean-rebuild marker is unavailable: {error}",
                    ("missing_build_marker", "abi_inventory_mismatch"),
                )
            )
        else:
            if (
                not build_marker.is_file()
                or object_stat.st_mtime_ns <= marker_stat.st_mtime_ns
            ):
                findings.append(
                    Finding(
                        path,
                        1,
                        "<object-inventory>",
                        "shared kernel object was not produced after the clean-rebuild marker",
                        ("stale_release_object", "abi_inventory_mismatch"),
                    )
                )

    input_mtimes: list[tuple[pathlib.Path, int]] = []
    for input_path in dict.fromkeys(freshness_paths):
        try:
            if input_path.is_file():
                input_mtimes.append((input_path, input_path.stat().st_mtime_ns))
        except OSError:
            continue
    newest_input = max(input_mtimes, key=lambda item: item[1]) if input_mtimes else None
    if newest_input is not None and object_stat.st_mtime_ns <= newest_input[1]:
        findings.append(
            Finding(
                path,
                1,
                "<object-inventory>",
                f"shared kernel object is stale relative to {newest_input[0]}",
                ("stale_release_object", "abi_inventory_mismatch"),
            )
        )

    return (
        (
            {
                "kind": "release_object_validation",
                "status": "verified" if not findings else "invalid",
                "path": path.as_posix(),
                "mtime_ns": object_stat.st_mtime_ns,
                "newest_input": (
                    newest_input[0].as_posix() if newest_input is not None else None
                ),
                "newest_input_mtime_ns": (
                    newest_input[1] if newest_input is not None else None
                ),
                "build_marker": build_marker.as_posix() if build_marker else None,
                "build_marker_mtime_ns": (
                    marker_stat.st_mtime_ns if marker_stat is not None else None
                ),
                "input_count": len(input_mtimes),
                "input_inventory_sha256": hashlib.sha256(
                    b"".join(
                        input_path.as_posix().encode("utf-8")
                        + b"\0"
                        + hashlib.sha256(input_path.read_bytes()).digest()
                        for input_path, _ in sorted(
                            input_mtimes, key=lambda item: item[0].as_posix()
                        )
                    )
                ).hexdigest(),
            },
        ),
        findings,
    )


def audit_abi(
    source_paths: Sequence[pathlib.Path],
    header_paths: Sequence[pathlib.Path],
    *,
    manifest_path: pathlib.Path | None = None,
    compiler: str | None = None,
    object_paths: Sequence[pathlib.Path] = (),
    require_release_object: bool = False,
    build_marker: pathlib.Path | None = None,
) -> AbiInventory:
    definitions: list[AbiSymbol] = []
    source_definitions: list[AbiSymbol] = []
    declarations: list[AbiSymbol] = []
    findings: list[Finding] = []
    per_file: list[dict[str, object]] = []
    manifest_evidence: dict[str, object] | None = None
    compiler_evidence: dict[str, object] | None = None
    object_evidence: tuple[dict[str, object], ...] = ()
    if require_release_object:
        freshness_paths = [*source_paths, *header_paths]
        kernel_root = pathlib.Path("pgaccel-kernels")
        for pattern in (
            "CMakeLists.txt",
            "include/**/*.h",
            "include/**/*.hpp",
            "src/**/*.h",
            "src/**/*.hpp",
        ):
            freshness_paths.extend(kernel_root.glob(pattern))
        for path in (pathlib.Path(".acpp-version"),):
            if path.is_file():
                freshness_paths.append(path)
        validation_evidence, validation_findings = _release_object_validation(
            object_paths, freshness_paths, build_marker
        )
        object_evidence += validation_evidence
        findings.extend(validation_findings)
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
        findings.extend(_macro_inventory_findings(path, directives, abi_inventory=True))
        findings.extend(
            Finding(
                path,
                directive.line,
                "<preprocessor>",
                f"source ABI depends on an unproven condition: {directive.text.splitlines()[0]}",
                ("ambiguous_preprocessor_condition", "abi_inventory_mismatch"),
            )
            for directive in _ambiguous_preprocessor_directives(directives)
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
            _canonical_signature_tokens(symbol.full_signature)
            for symbol in definitions_by_name[name]
        }
        declaration_signatures = {
            _canonical_signature_tokens(symbol.full_signature)
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

    if manifest_path is not None:
        try:
            manifest = load_abi_manifest(manifest_path)
        except ParseError as error:
            findings.append(
                Finding(
                    manifest_path,
                    1,
                    "<abi-manifest>",
                    str(error),
                    ("abi_manifest_integrity_error", "abi_inventory_mismatch"),
                )
            )
            manifest_evidence = {
                "path": manifest_path.as_posix(),
                "expected_count": EXPECTED_ABI_MANIFEST_COUNT,
                "expected_sha256": EXPECTED_ABI_MANIFEST_SHA256,
                "status": "invalid",
            }
        else:
            manifest_evidence = {
                "path": manifest.path.as_posix(),
                "count": manifest.count,
                "sha256": manifest.sha256,
                "expected_count": EXPECTED_ABI_MANIFEST_COUNT,
                "expected_sha256": EXPECTED_ABI_MANIFEST_SHA256,
                "status": "verified",
            }
            findings.extend(
                _compare_symbols_to_manifest(
                    definitions,
                    manifest,
                    origin="source definition inventory",
                    fallback_path=manifest_path,
                )
            )
            try:
                compiler_inventory = compiler_header_inventory(header_paths, compiler)
            except ParseError as error:
                findings.append(
                    Finding(
                        header_paths[0] if header_paths else manifest_path,
                        1,
                        "<compiler-inventory>",
                        str(error),
                        ("compiler_inventory_error", "abi_inventory_mismatch"),
                    )
                )
                compiler_evidence = {"status": "error", "error": str(error)}
            else:
                findings.extend(
                    _compare_symbols_to_manifest(
                        compiler_inventory.symbols,
                        manifest,
                        origin="compiler header inventory",
                        fallback_path=manifest_path,
                    )
                )
                compiler_evidence = {
                    "status": "verified",
                    "compiler_path": compiler_inventory.compiler_path,
                    "compiler_version": compiler_inventory.compiler_version,
                    "command": list(compiler_inventory.command),
                    "umbrella_sha256": compiler_inventory.umbrella_sha256,
                    "inventory_count": len(compiler_inventory.symbols),
                    "inventory_hash": _inventory_hash(compiler_inventory.symbols),
                    "stderr": compiler_inventory.stderr,
                }
            if object_paths:
                nm_evidence, object_findings = _object_abi_evidence(
                    object_paths, manifest
                )
                object_evidence += nm_evidence
                findings.extend(object_findings)
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
        manifest_evidence,
        compiler_evidence,
        object_evidence,
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
        "--abi-manifest",
        type=pathlib.Path,
        default=DEFAULT_ABI_MANIFEST,
        help="immutable full-signature ABI manifest (verified, never rewritten)",
    )
    parser.add_argument(
        "--abi-compiler",
        help="clang++ executable used for the compiler-backed header inventory",
    )
    parser.add_argument(
        "--objects",
        nargs="*",
        default=(),
        type=pathlib.Path,
        help="built library/object files whose exported symbols are checked with nm",
    )
    parser.add_argument(
        "--build-marker",
        type=pathlib.Path,
        help="marker created before the clean rebuild that produced --objects",
    )
    parser.add_argument(
        "--regenerate-abi-manifest",
        type=pathlib.Path,
        help="explicit maintainer operation that writes a compiler-derived manifest",
    )
    parser.add_argument(
        "sources", nargs="*", type=pathlib.Path, help="C++ source files to audit"
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
        "schema_version": 3,
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
                "manifest": abi.manifest,
                "compiler": abi.compiler,
                "objects": list(abi.objects),
                "per_file": list(abi.per_file),
                "definitions": [
                    {
                        "path": symbol.path.as_posix(),
                        "line": symbol.line,
                        "name": symbol.name,
                        "return_type": symbol.return_spelling,
                        "parameter_count": symbol.parameter_count,
                        "parameter_types": list(symbol.parameter_types),
                        "full_signature": symbol.full_signature,
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
                        "parameter_types": list(symbol.parameter_types),
                        "full_signature": symbol.full_signature,
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
    if args.regenerate_abi_manifest is not None:
        if not args.headers:
            print(
                "--regenerate-abi-manifest requires --headers",
                file=sys.stderr,
            )
            return 2
        try:
            compiler_inventory = compiler_header_inventory(
                args.headers, args.abi_compiler
            )
        except ParseError as error:
            print(str(error), file=sys.stderr)
            return 1
        by_name: dict[str, str] = {}
        for symbol in compiler_inventory.symbols:
            previous = by_name.setdefault(symbol.name, symbol.full_signature)
            if _canonical_signature_tokens(previous) != _canonical_signature_tokens(
                symbol.full_signature
            ):
                print(
                    f"ambiguous compiler signatures for {symbol.name}", file=sys.stderr
                )
                return 1
        rendered = "".join(
            f"{name}|{signature}\n" for name, signature in sorted(by_name.items())
        )
        args.regenerate_abi_manifest.parent.mkdir(parents=True, exist_ok=True)
        args.regenerate_abi_manifest.write_text(rendered, encoding="utf-8")
        digest = hashlib.sha256(rendered.encode("utf-8")).hexdigest()
        print(
            f"wrote {len(by_name)} signatures to {args.regenerate_abi_manifest}; "
            f"SHA-256 {digest}. Review the diff and update literal integrity constants explicitly."
        )
        return 0
    if not args.sources:
        print("at least one C++ source file is required", file=sys.stderr)
        return 2
    unique_paths = list(dict.fromkeys(args.sources))
    audits = audit_paths(unique_paths)
    findings = [finding for audit in audits for finding in audit.findings]
    object_paths = list(args.objects)
    abi = (
        audit_abi(
            unique_paths,
            args.headers,
            manifest_path=args.abi_manifest,
            compiler=args.abi_compiler,
            object_paths=object_paths,
            require_release_object=True,
            build_marker=args.build_marker,
        )
        if args.headers
        else None
    )
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
