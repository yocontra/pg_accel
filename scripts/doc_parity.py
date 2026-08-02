#!/usr/bin/env python3
"""Strict parity checks for source-backed pg_accel documentation."""

from __future__ import annotations

import argparse
import re
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Sequence


REPO_ROOT = Path(__file__).resolve().parent.parent

AUTHORITATIVE_DOCS = (
    "README.md",
    "CLAUDE.md",
    "ARCHITECTURE.md",
    "TODO.md",
    "CONTRIBUTING.md",
    "SECURITY.md",
    "docs/ADAPTER_GUIDE.md",
    "docs/BENCHMARKS.md",
    "docs/EXPLAIN_EXAMPLES.md",
    "docs/olap-abi.md",
    ".github/ISSUE_TEMPLATE/bug_report.md",
    ".github/ISSUE_TEMPLATE/bug_report.yml",
    ".github/ISSUE_TEMPLATE/feature_request.md",
    ".github/ISSUE_TEMPLATE/feature_request.yml",
)

CURRENT_SEMANTIC_DOCS = tuple(path for path in AUTHORITATIVE_DOCS if path != "TODO.md")

PATH_SUFFIXES = (
    "rs",
    "cpp",
    "cc",
    "cxx",
    "h",
    "hpp",
    "hxx",
    "c",
    "mm",
    "metal",
    "sql",
    "toml",
    "yaml",
    "yml",
    "json",
    "sh",
    "md",
    "txt",
    "py",
)
PATH_RE = rf"(?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_.-]+\.(?:{'|'.join(PATH_SUFFIXES)})"
BARE_PATH_RE = r"(?:Justfile|Makefile|CMakeLists\.txt|Cargo\.toml|Cargo\.lock)"
CITATION_RE = re.compile(
    rf"(?<![A-Za-z0-9_./-])(?P<path>{PATH_RE}|{BARE_PATH_RE}):"
    r"(?P<start>[0-9]+)(?:-(?P<end>[0-9]+))?"
)

GUC_TABLE_HEADER = ("Parameter", "Type", "Default", "Context", "Range", "Effect")
CAPABILITY_TABLE_HEADER = (
    "Capability",
    "Implementation surface",
    "Production planner",
    "Current boundary",
)
ADAPTER_TABLE_HEADER = ("Adapter", "Registered functions")

MACOS_HOMEBREW_PREREQUISITES = ("llvm@20", "lld@20", "libomp", "boost", "postgis")
MACOS_PREREQUISITE_DOCS = ("README.md", "CONTRIBUTING.md", "Justfile", "CHANGELOG.md")
BREW_INSTALL_RE = re.compile(r"\bbrew install(?P<formulas>(?: [A-Za-z0-9@+_.-]+)+)")

EXPECTED_CAPABILITIES = {
    "Resident reducing or grouped aggregate": ("Present", "Selectable"),
    "Scalar row predicate inside a resident aggregate": (
        "Present",
        "Selectable for one comparison",
    ),
    "Resident star join plus aggregate": ("Present", "Selectable"),
    "H3-derived group key inside a resident aggregate": ("Present", "Selectable"),
    "PostGIS spatial filter inside a resident aggregate": ("Present", "Selectable"),
    "Standalone PostGIS or H3 function/SRF": (
        "Aggregate primitives and adapter registry metadata remain; standalone executor removed",
        "Not selectable",
    ),
    "Base scan, WHERE filter, or projection": (
        "No registered Custom Scan executor; host-staged implementation retired",
        "Not selectable",
    ),
    "Row-returning hash or inequality join": (
        "No registered row-returning executor; host-staged implementation retired",
        "Not selectable",
    ),
    "Standalone sort or top-k": (
        "Kernel and executor removed; numeric strategy tag and descriptor retained only for fail-closed wire decoding",
        "Not selectable",
    ),
    "Window": (
        "Kernel and executor removed; numeric strategy tag and descriptor retained only for fail-closed wire decoding",
        "Not selectable",
    ),
    "Raster": ("Registered childless resident executor", "Selectable"),
}

GUC_EFFECT_MARKERS = {
    "pg_accel.enabled": ("planning master switch", "already-planned", "fails closed"),
    "pg_accel.min_batch_size": ("legacy row-fed", "resident descriptor", "device-limit"),
    "pg_accel.gpu_enabled": ("planning gpu-path switch", "does not rewrite"),
    "pg_accel.kernel_timeout_ms": (
        "after a synchronous dispatch returns",
        "between calls",
        "does not asynchronously cancel",
    ),
    "pg_accel.max_workers_total": (
        "cluster host-thread ledger",
        "current executors request no host threads",
        "postgresql parallel worker processes are not counted",
    ),
    "pg_accel.resident_memory_budget_mb": (
        "cluster-wide mib cap",
        "all charged residency bytes",
        "pins never bypass",
    ),
    "pg_accel.auto_load": ("selected resident plan", "explicit pin", "existing pin"),
    "pg_accel.cost_multiplier": ("only for resident generic aggregate candidates",),
    "pg_accel.log_level": (
        "first custom scan executes",
        "later changes do not rebuild",
        "notice",
        "warning",
        "warn",
    ),
    "pg_accel.assert_dispatch": ("reserved no-op", "neither planning nor execution"),
    "pg_accel.parallel_fused_count": ("reserved no-op", "remains native"),
    "pg_accel.planner_profiling": (
        "planner-hook monotonic-clock reads",
        "elapsed-time counters",
        "call and decline counters remain active",
    ),
    "pg_accel.otel_log_max_mb": (
        "trace cap in mib",
        "sampled at trace initialization",
        "pg_accel_trace_file_max_bytes",
    ),
    "pg_accel.otel_log_max_rotations": (
        "sampled at trace initialization",
        "discards rotated copies",
    ),
    "pg_accel.fp64_enabled": ("deprecated no-op", "does not disable fp64"),
    "pg_accel.soft_fp64_cost_multiplier": ("device lacks native fp64",),
}

STALE_SEMANTIC_PATTERNS = (
    (re.compile(r"\bpg_accel\.workers\b", re.IGNORECASE), "nonexistent pg_accel.workers GUC"),
    (re.compile(r"Custom Scan \(pg_accel\)", re.IGNORECASE), "obsolete Custom Scan name"),
    (re.compile(r"kill switch for fp64", re.IGNORECASE), "fp64_enabled is a no-op"),
    (
        re.compile(r"pg_accel\.fp64_enabled\s*=\s*(?:false|off)", re.IGNORECASE),
        "fp64_enabled cannot define a benchmark arm",
    ),
    (re.compile(r"min rows for gpu dispatch", re.IGNORECASE), "min_batch_size semantics drift"),
    (re.compile(r"\bOpenCL\b", re.IGNORECASE), "unsupported backend claim"),
    (re.compile(r"prebuilt pgrx package", re.IGNORECASE), "unproved package claim"),
    (re.compile(r"latest local full-suite run", re.IGNORECASE), "candidate benchmark claim"),
)


@dataclass(frozen=True)
class Citation:
    document: str
    document_line: int
    path: str
    start: int
    end: int


@dataclass(frozen=True)
class GucSpec:
    name: str
    value_type: str
    default: str
    context: str
    value_range: str


@dataclass
class AuditResult:
    errors: list[str]
    counts: Counter[str]
    citations: list[Citation]


def strip_code_ticks(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value.startswith("`") and value.endswith("`"):
        return value[1:-1].strip()
    return value


def table_cells(line: str) -> tuple[str, ...]:
    stripped = line.strip()
    if not stripped.startswith("|") or not stripped.endswith("|"):
        return ()
    return tuple(cell.strip() for cell in stripped[1:-1].split("|"))


def find_markdown_table(text: str, header: Sequence[str]) -> list[tuple[str, ...]]:
    lines = text.splitlines()
    expected = tuple(header)
    for index, line in enumerate(lines):
        if table_cells(line) != expected:
            continue
        if index + 1 >= len(lines):
            return []
        divider = table_cells(lines[index + 1])
        if len(divider) != len(expected) or not all(
            re.fullmatch(r":?-{3,}:?", cell) for cell in divider
        ):
            return []
        rows: list[tuple[str, ...]] = []
        for row_line in lines[index + 2 :]:
            cells = table_cells(row_line)
            if not cells:
                break
            if len(cells) != len(expected):
                raise ValueError(f"table row has {len(cells)} cells; expected {len(expected)}")
            rows.append(cells)
        return rows
    return []


def strip_rust_comments(text: str) -> str:
    """Remove Rust comments while preserving strings and line structure."""

    output: list[str] = []
    index = 0
    state = "code"
    block_depth = 0
    while index < len(text):
        char = text[index]
        nxt = text[index + 1] if index + 1 < len(text) else ""
        if state == "string":
            output.append(char)
            if char == "\\" and index + 1 < len(text):
                index += 1
                output.append(text[index])
            elif char == '"':
                state = "code"
        elif state == "line_comment":
            if char == "\n":
                output.append(char)
                state = "code"
            else:
                output.append(" ")
        elif state == "block_comment":
            if char == "/" and nxt == "*":
                output.extend((" ", " "))
                block_depth += 1
                index += 1
            elif char == "*" and nxt == "/":
                output.extend((" ", " "))
                block_depth -= 1
                index += 1
                if block_depth == 0:
                    state = "code"
            elif char == "\n":
                output.append(char)
            else:
                output.append(" ")
        elif char == '"':
            output.append(char)
            state = "string"
        elif char == "/" and nxt == "/":
            output.extend((" ", " "))
            state = "line_comment"
            index += 1
        elif char == "/" and nxt == "*":
            output.extend((" ", " "))
            state = "block_comment"
            block_depth = 1
            index += 1
        else:
            output.append(char)
        index += 1
    return "".join(output)


def balanced_call_body(text: str, open_index: int) -> str:
    depth = 0
    in_string = False
    index = open_index
    while index < len(text):
        char = text[index]
        if in_string:
            if char == "\\":
                index += 1
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return text[open_index + 1 : index]
        index += 1
    raise ValueError("unterminated function call")


def split_top_level_arguments(body: str) -> list[str]:
    arguments: list[str] = []
    start = 0
    stack: list[str] = []
    pairs = {")": "(", "]": "[", "}": "{"}
    in_string = False
    index = 0
    while index < len(body):
        char = body[index]
        if in_string:
            if char == "\\":
                index += 1
            elif char == '"':
                in_string = False
        elif char == '"':
            in_string = True
        elif char in "([{":
            stack.append(char)
        elif char in ")]}":
            if not stack or stack.pop() != pairs[char]:
                raise ValueError("unbalanced registration argument")
        elif char == "," and not stack:
            arguments.append(body[start:index].strip())
            start = index + 1
        index += 1
    tail = body[start:].strip()
    if tail:
        arguments.append(tail)
    return arguments


def resolve_source_atom(expression: str, constants: dict[str, str]) -> str:
    value = re.sub(r"\s+", "", expression)
    seen: set[str] = set()
    while re.fullmatch(r"[A-Z][A-Z0-9_]*", value) and value in constants:
        if value in seen:
            raise ValueError(f"constant cycle at {value}")
        seen.add(value)
        value = re.sub(r"\s+", "", constants[value])
    value = value.replace("_", "")
    if value == "true":
        return "on"
    if value == "false":
        return "off"
    if "::" in value:
        return value.rsplit("::", 1)[1].lower()
    return value


def parse_enum_values(source: str, enum_name: str) -> str:
    match = re.search(rf"pub\s+enum\s+{re.escape(enum_name)}\s*\{{(?P<body>.*?)\}}", source, re.S)
    if match is None:
        raise ValueError(f"enum definition not found: {enum_name}")
    variants = []
    for raw in match.group("body").split(","):
        variant = raw.strip()
        if not variant:
            continue
        variant = variant.split("=", 1)[0].strip()
        if not re.fullmatch(r"[A-Za-z][A-Za-z0-9_]*", variant):
            raise ValueError(f"unsupported enum variant syntax: {variant}")
        variants.append(variant.lower())
    return ",".join(variants)


def parse_released_gucs(root: Path) -> tuple[dict[str, GucSpec], list[str]]:
    source_paths = ("pg_accel/src/engine/gucs.rs", "pg_accel/src/lib.rs")
    sources = {path: strip_rust_comments((root / path).read_text()) for path in source_paths}
    combined = "\n".join(sources.values())
    constants = {
        match.group("name"): match.group("value").strip()
        for match in re.finditer(
            r"\bconst\s+(?P<name>[A-Z][A-Z0-9_]*)\s*:\s*[^=;]+="
            r"\s*(?P<value>[^;]+);",
            combined,
            re.S,
        )
    }
    settings: dict[str, tuple[str, str]] = {}
    setting_re = re.compile(
        r"\bstatic\s+(?P<name>[A-Z][A-Z0-9_]*)\s*:\s*"
        r"GucSetting<(?P<type>[^>]+)>\s*=\s*"
        r"GucSetting(?:\s*::\s*<[^>]+>)?\s*::\s*new\((?P<default>[^)]+)\)\s*;",
        re.S,
    )
    for match in setting_re.finditer(combined):
        settings[match.group("name")] = (
            match.group("type").strip(),
            resolve_source_atom(match.group("default"), constants),
        )

    errors: list[str] = []
    specs: dict[str, GucSpec] = {}
    registration_re = re.compile(r"GucRegistry::define_(bool|int|float|enum)_guc\s*\(")
    for source_path, source in sources.items():
        for match in registration_re.finditer(source):
            value_type = match.group(1)
            try:
                args = split_top_level_arguments(balanced_call_body(source, match.end() - 1))
            except ValueError as error:
                errors.append(f"{source_path}: cannot parse GUC registration: {error}")
                continue
            minimum_args = 8 if value_type in {"int", "float"} else 6
            if len(args) != minimum_args:
                errors.append(
                    f"{source_path}: {value_type} GUC has {len(args)} arguments; "
                    f"expected {minimum_args}"
                )
                continue
            name_match = re.fullmatch(r'c"(?P<name>pg_accel\.[a-z0-9_]+)"', args[0])
            if name_match is None:
                errors.append(f"{source_path}: cannot parse GUC name from {args[0]!r}")
                continue
            name = name_match.group("name")
            if name.startswith("pg_accel.test_"):
                continue
            setting_arg = args[3]
            setting_match = re.fullmatch(r"&(?P<name>[A-Z][A-Z0-9_]*)", setting_arg)
            if setting_match is None or setting_match.group("name") not in settings:
                errors.append(f"{source_path}: {name} has unknown setting {setting_arg!r}")
                continue
            setting_name = setting_match.group("name")
            setting_type, default = settings[setting_name]
            expected_rust_type = {
                "bool": "bool",
                "int": "i32",
                "float": "f64",
            }.get(value_type)
            if expected_rust_type is not None and setting_type != expected_rust_type:
                errors.append(
                    f"{source_path}: {name} is define_{value_type} but uses GucSetting<{setting_type}>"
                )
            context_index = 6 if value_type in {"int", "float"} else 4
            context_match = re.fullmatch(r"GucContext::(Userset|Suset)", args[context_index])
            if context_match is None:
                errors.append(f"{source_path}: {name} has unsupported context {args[context_index]!r}")
                continue
            context = "user" if context_match.group(1) == "Userset" else "superuser"
            if value_type in {"int", "float"}:
                minimum = resolve_source_atom(args[4], constants)
                maximum = resolve_source_atom(args[5], constants)
                value_range = f"{minimum}..{maximum}"
            elif value_type == "enum":
                value_range = parse_enum_values(combined, setting_type)
            else:
                value_range = "-"
            if name in specs:
                errors.append(f"duplicate released GUC registration: {name}")
                continue
            specs[name] = GucSpec(name, value_type, default, context, value_range)
    return specs, errors


def collect_citations(document: str, text: str) -> list[Citation]:
    citations: list[Citation] = []
    for document_line, line in enumerate(text.splitlines(), start=1):
        for match in CITATION_RE.finditer(line):
            start = int(match.group("start"))
            end = int(match.group("end") or start)
            citations.append(
                Citation(document, document_line, match.group("path"), start, end)
            )
    return citations


def audit_citations(root: Path, documents: Iterable[str], verbose: bool = False) -> AuditResult:
    errors: list[str] = []
    counts: Counter[str] = Counter()
    citations: list[Citation] = []
    resolved_root = root.resolve()
    for document in documents:
        doc_path = root / document
        if not doc_path.is_file():
            errors.append(f"{document}: required documentation file is missing")
            continue
        found = collect_citations(document, doc_path.read_text())
        counts[document] = len(found)
        citations.extend(found)
        for citation in found:
            label = f"{document}:{citation.document_line} {citation.path}:{citation.start}"
            if citation.end != citation.start:
                label += f"-{citation.end}"
            raw_path = citation.path
            path_parts = Path(raw_path).parts
            if raw_path.startswith("./") or Path(raw_path).is_absolute() or ".." in path_parts:
                errors.append(f"{label}: citation path must be exact and repository-relative")
                continue
            target = root / raw_path
            if not target.is_file():
                errors.append(f"{label}: cited file does not exist (no shorthand resolution)")
                continue
            resolved_target = target.resolve()
            try:
                resolved_target.relative_to(resolved_root)
            except ValueError:
                errors.append(f"{label}: cited file escapes repository root")
                continue
            line_count = len(target.read_text(errors="replace").splitlines())
            if citation.start < 1 or citation.end < citation.start:
                errors.append(f"{label}: invalid citation range")
                continue
            if citation.end > line_count:
                errors.append(f"{label}: file has exactly {line_count} lines")
                continue
            if verbose:
                print(f"ok citation {label}")
    return AuditResult(errors, counts, citations)


def parse_guc_table(text: str) -> tuple[dict[str, tuple[str, str, str, str, str]], list[str]]:
    errors: list[str] = []
    try:
        rows = find_markdown_table(text, GUC_TABLE_HEADER)
    except ValueError as error:
        return {}, [str(error)]
    if not rows:
        return {}, ["complete GUC table was not found"]
    table: dict[str, tuple[str, str, str, str, str]] = {}
    for row in rows:
        name = strip_code_ticks(row[0])
        if name in table:
            errors.append(f"duplicate GUC table row: {name}")
            continue
        table[name] = (
            strip_code_ticks(row[1]),
            strip_code_ticks(row[2]),
            strip_code_ticks(row[3]),
            strip_code_ticks(row[4]),
            row[5].strip(),
        )
    return table, errors


def validate_guc_table(
    document: str,
    table: dict[str, tuple[str, str, str, str, str]],
    specs: dict[str, GucSpec],
) -> list[str]:
    errors: list[str] = []
    source_names = set(specs)
    table_names = set(table)
    for missing in sorted(source_names - table_names):
        errors.append(f"{document}: released GUC missing from complete table: {missing}")
    for extra in sorted(table_names - source_names):
        errors.append(f"{document}: non-released GUC present in complete table: {extra}")
    for name in sorted(source_names & table_names):
        spec = specs[name]
        value_type, default, context, value_range, effect = table[name]
        observed = (value_type, default, context, value_range)
        expected = (spec.value_type, spec.default, spec.context, spec.value_range)
        if observed != expected:
            errors.append(
                f"{document}: {name} metadata {observed!r} does not match source {expected!r}"
            )
        effect_lower = effect.lower()
        for marker in GUC_EFFECT_MARKERS.get(name, ()):
            if marker.lower() not in effect_lower:
                errors.append(f"{document}: {name} effect is missing semantic marker {marker!r}")
    return errors


def audit_guc_docs(root: Path) -> tuple[list[str], dict[str, GucSpec]]:
    specs, errors = parse_released_gucs(root)
    source_names = set(specs)
    marker_names = set(GUC_EFFECT_MARKERS)
    if source_names != marker_names:
        for name in sorted(source_names - marker_names):
            errors.append(f"released GUC has no semantic parity profile: {name}")
        for name in sorted(marker_names - source_names):
            errors.append(f"semantic parity profile has no released GUC: {name}")

    tables: dict[str, dict[str, tuple[str, str, str, str, str]]] = {}
    for document in ("README.md", "CLAUDE.md"):
        table, table_errors = parse_guc_table((root / document).read_text())
        errors.extend(f"{document}: {error}" for error in table_errors)
        errors.extend(validate_guc_table(document, table, specs))
        tables[document] = table
    if tables.get("README.md") and tables.get("CLAUDE.md"):
        for name in sorted(source_names):
            readme_effect = tables["README.md"][name][4]
            claude_effect = tables["CLAUDE.md"][name][4]
            if re.sub(r"\s+", " ", readme_effect) != re.sub(r"\s+", " ", claude_effect):
                errors.append(f"README.md and CLAUDE.md disagree on {name} effect semantics")

    allowed_mentions = source_names | {"pg_accel.test_"}
    mention_re = re.compile(r"\bpg_accel\.[a-z][a-z0-9_]*")
    for document in CURRENT_SEMANTIC_DOCS:
        text = (root / document).read_text()
        for match in mention_re.finditer(text):
            name = match.group(0)
            if name not in allowed_mentions:
                line = text.count("\n", 0, match.start()) + 1
                errors.append(f"{document}:{line}: unknown or test-only GUC mention: {name}")
        for pattern, reason in STALE_SEMANTIC_PATTERNS:
            for match in pattern.finditer(text):
                line = text.count("\n", 0, match.start()) + 1
                errors.append(f"{document}:{line}: {reason}")
        for line_number, line_text in enumerate(text.splitlines(), start=1):
            if re.search(r"\b(?:CUDA|NVIDIA)\b", line_text, re.IGNORECASE) and not re.search(
                r"owner-deferred|no support claim", line_text, re.IGNORECASE
            ):
                errors.append(
                    f"{document}:{line_number}: CUDA/NVIDIA may only be described as owner-deferred"
                )
    return errors, specs


def extract_const_string_array(source: str, constant: str) -> set[str]:
    match = re.search(
        rf"\bconst\s+{re.escape(constant)}\s*:\s*&\[&str\]\s*=\s*&\[(?P<body>.*?)\];",
        source,
        re.S,
    )
    if match is None:
        raise ValueError(f"string array not found: {constant}")
    return set(re.findall(r'"([a-z][a-z0-9_]*)"', match.group("body")))


def parse_adapter_table(text: str) -> tuple[dict[str, set[str]], list[str]]:
    errors: list[str] = []
    try:
        rows = find_markdown_table(text, ADAPTER_TABLE_HEADER)
    except ValueError as error:
        return {}, [str(error)]
    if not rows:
        return {}, ["registered adapter function table was not found"]
    table: dict[str, set[str]] = {}
    for adapter, functions in rows:
        if adapter in table:
            errors.append(f"duplicate adapter table row: {adapter}")
            continue
        names = set(re.findall(r"`([a-z][a-z0-9_]*)`", functions))
        if not names:
            errors.append(f"adapter row has no exact registered function names: {adapter}")
        table[adapter] = names
    return table, errors


def validate_adapter_table(table: dict[str, set[str]], expected: dict[str, set[str]]) -> list[str]:
    if table == expected:
        return []
    return [
        "README.md: registered adapter function table does not match constructors: "
        f"documented={table!r}, source={expected!r}"
    ]


def parse_capability_table(text: str) -> tuple[dict[str, tuple[str, str]], list[str]]:
    errors: list[str] = []
    try:
        rows = find_markdown_table(text, CAPABILITY_TABLE_HEADER)
    except ValueError as error:
        return {}, [str(error)]
    if not rows:
        return {}, ["implementation-versus-planner capability table was not found"]
    table: dict[str, tuple[str, str]] = {}
    for capability, implementation, planner, _boundary in rows:
        if capability in table:
            errors.append(f"duplicate capability row: {capability}")
            continue
        table[capability] = (implementation, planner)
    return table, errors


def validate_capability_table(table: dict[str, tuple[str, str]]) -> list[str]:
    errors: list[str] = []
    expected_names = set(EXPECTED_CAPABILITIES)
    table_names = set(table)
    for missing in sorted(expected_names - table_names):
        errors.append(f"README.md: capability row missing: {missing}")
    for extra in sorted(table_names - expected_names):
        errors.append(f"README.md: unexpected capability row: {extra}")
    for name in sorted(expected_names & table_names):
        if table[name] != EXPECTED_CAPABILITIES[name]:
            errors.append(
                f"README.md: {name} status {table[name]!r} does not match "
                f"{EXPECTED_CAPABILITIES[name]!r}"
            )
    return errors


def audit_capabilities(root: Path) -> tuple[list[str], int, int]:
    errors: list[str] = []
    readme = (root / "README.md").read_text()
    capability_table, table_errors = parse_capability_table(readme)
    errors.extend(f"README.md: {error}" for error in table_errors)
    errors.extend(validate_capability_table(capability_table))

    planner_mod = (root / "pg_accel/src/engine/ffi/planner_hooks/mod.rs").read_text()
    injectors = re.findall(r"\b([a-z][a-z0-9_]*)::try_inject\s*\(", planner_mod)
    if injectors != ["generic_groupagg", "raster"]:
        errors.append(
            "production planner injector inventory changed; expected generic_groupagg and raster, "
            f"found {injectors!r}"
        )
    source_markers = {
        "aggregate hook": (planner_mod, "UPPERREL_GROUP_AGG if gucs::gpu_enabled()"),
        "window decline": (planner_mod, '"upper_paths_window"'),
        "SRF decline": (planner_mod, '"upper_paths_srf_target_list"'),
        "base resident decline": (
            (root / "pg_accel/src/engine/ffi/planner_hooks/rel_pathlist.rs").read_text(),
            "no longer injects a host-staged",
        ),
        "join resident decline": (
            (root / "pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs").read_text(),
            "no longer injects a host-staged",
        ),
        "test-only raster force": (
            (root / "pg_accel/src/engine/ffi/planner_hooks/mod.rs").read_text(),
            "raster::try_force_inject",
        ),
        "H3 parent group key": (
            (root / "pg_accel/src/engine/spec/mod.rs").read_text(),
            "H3CellToParent",
        ),
        "H3 lat/lng group key": (
            (root / "pg_accel/src/engine/spec/mod.rs").read_text(),
            "H3LatLngToCell",
        ),
    }
    for label, (source, marker) in source_markers.items():
        if marker not in source:
            errors.append(f"source capability marker changed: {label} ({marker})")

    adapter_table, adapter_errors = parse_adapter_table(readme)
    errors.extend(f"README.md: {error}" for error in adapter_errors)
    try:
        postgis_source = strip_rust_comments((root / "pg_accel/src/adapters/postgis.rs").read_text())
        h3_source = strip_rust_comments((root / "pg_accel/src/adapters/h3.rs").read_text())
        expected_adapter_rows = {
            "PostGIS": extract_const_string_array(postgis_source, "GPU_ONLY_ALLOWLIST"),
            "H3 scalar": extract_const_string_array(h3_source, "SCALAR_WINNER_GPU_NAMES"),
            "H3 variable/record output": extract_const_string_array(h3_source, "VARLEN_GPU_NAMES"),
        }
        errors.extend(validate_adapter_table(adapter_table, expected_adapter_rows))
    except ValueError as error:
        errors.append(str(error))
        expected_adapter_rows = {}

    registry = (root / "pg_accel/src/engine/registry.rs").read_text()
    registered_adapters = set(
        re.findall(r"crate::adapters::([a-z][a-z0-9_]*)::adapter\(\)", registry)
    )
    if registered_adapters != {"postgis", "h3"}:
        errors.append(
            f"registry adapter inventory changed; expected postgis/h3, found {registered_adapters!r}"
        )
    adapter_entry_count = sum(len(names) for names in expected_adapter_rows.values())
    return errors, len(capability_table), adapter_entry_count


def audit_quick_start(root: Path) -> list[str]:
    readme = (root / "README.md").read_text()
    required = (
        "CREATE EXTENSION pg_accel;",
        "SELECT pg_accel_pin('pg_accel_quickstart', ARRAY['g', 'v']);",
        "SET pg_accel.auto_load = off;",
        "Custom Scan (GpuAccelAgg)",
        "GPU Resident Operator Class: resident_groupagg",
        "GPU Kernel Dispatched: true",
        "no_gpu_resident_pipeline",
    )
    return [f"README.md quick start is missing exact evidence: {value}" for value in required if value not in readme]


def audit_macos_prerequisites(root: Path) -> list[str]:
    errors: list[str] = []
    required = set(MACOS_HOMEBREW_PREREQUISITES)
    for document in MACOS_PREREQUISITE_DOCS:
        text = (root / document).read_text()
        commands = [
            tuple(match.group("formulas").split())
            for match in BREW_INSTALL_RE.finditer(text)
            if required.intersection(match.group("formulas").split())
        ]
        if commands != [MACOS_HOMEBREW_PREREQUISITES]:
            errors.append(
                f"{document}: macOS Homebrew prerequisites must contain exactly "
                f"`brew install {' '.join(MACOS_HOMEBREW_PREREQUISITES)}`; "
                f"found {commands!r}"
            )
    return errors


def run(root: Path, verbose: bool = False) -> int:
    citation_result = audit_citations(root, AUTHORITATIVE_DOCS, verbose)
    guc_errors, gucs = audit_guc_docs(root)
    capability_errors, capability_count, adapter_entry_count = audit_capabilities(root)
    quick_start_errors = audit_quick_start(root)
    macos_prerequisite_errors = audit_macos_prerequisites(root)
    errors = (
        citation_result.errors
        + guc_errors
        + capability_errors
        + quick_start_errors
        + macos_prerequisite_errors
    )

    print("=== doc_parity summary ===")
    for document in AUTHORITATIVE_DOCS:
        print(f"  {document}: {citation_result.counts[document]} citations")
    print(f"  exact citations:       {len(citation_result.citations)}")
    print(f"  released GUCs:         {len(gucs)}")
    print(f"  capability rows:       {capability_count}")
    print(f"  registered functions:  {adapter_entry_count}")
    print(f"  macOS brew formulas:   {len(MACOS_HOMEBREW_PREREQUISITES)}")
    if errors:
        print("\n=== parity failures ===", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        print(f"\nFAIL: {len(errors)} documentation parity error(s).", file=sys.stderr)
        return 1
    print("PASS")
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Validate exact source citations, released GUC semantics, adapter inventory, "
            "macOS prerequisites, and production planner capability documentation."
        )
    )
    parser.add_argument("-v", "--verbose", action="store_true", help="print each valid citation")
    args = parser.parse_args(argv)
    return run(REPO_ROOT, args.verbose)


if __name__ == "__main__":
    raise SystemExit(main())
