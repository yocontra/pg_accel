#!/usr/bin/env python3
"""Fail-closed audit for pg_accel's high-risk lifecycle coverage.

The ordinary coverage percentage treats a codec branch and an FFI cleanup
branch alike.  This audit adds a separate contract: every production Rust
unsafe site must be assigned to a reviewed risk domain, every domain must
name executable test evidence, and exact-candidate LCOV must independently
cover the registered high-risk anchors and at least 90% of executable lines
adjacent to unsafe syntax.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import re
import sys
from pathlib import Path
from typing import Any


REQUIRED_DOMAINS = {
    "multi_session_residency",
    "executor_reset_drop",
    "planner_private_data",
    "allocation_free",
    "copy_wait",
    "cancellation",
    "output_materialization",
    "postgis_calls",
    "derived_publication",
    "unsafe_ffi",
    "lifetime_ownership",
    "cleanup",
    "invalidation",
}
UNSAFE_SYNTAX = re.compile(r"\bunsafe\s*(?:\{|fn\b|impl\b|trait\b|extern\b)")
SOURCE_ROOT_MARKERS = (
    "pg_accel/src/",
    "pg_accel_bench/src/",
    "pgaccel-kernels/src/",
    "pgaccel-kernels/include/",
    "pgaccel-kernels/test/",
)


class AuditError(RuntimeError):
    pass


def _load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise AuditError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise AuditError(f"{path} must contain one JSON object")
    return value


def _repo_path(raw: str) -> str | None:
    normalized = raw.replace("\\", "/")
    for marker in SOURCE_ROOT_MARKERS:
        index = normalized.rfind(marker)
        if index >= 0:
            return normalized[index:]
    if normalized.endswith("pg_accel/build.rs"):
        return "pg_accel/build.rs"
    return None


def _parse_lcov(path: Path) -> dict[str, dict[int, int]]:
    records: dict[str, dict[int, int]] = {}
    current: str | None = None
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as error:
        raise AuditError(f"cannot read LCOV {path}: {error}") from error
    for line in lines:
        if line.startswith("SF:"):
            current = _repo_path(line[3:])
            if current is not None:
                records.setdefault(current, {})
        elif line.startswith("DA:") and current is not None:
            fields = line[3:].split(",")
            if len(fields) < 2:
                raise AuditError(f"malformed LCOV DA record in {path}: {line}")
            try:
                number, hits = int(fields[0]), int(fields[1])
            except ValueError as error:
                raise AuditError(f"malformed LCOV DA record in {path}: {line}") from error
            records[current][number] = max(records[current].get(number, 0), hits)
    if not records:
        raise AuditError(f"LCOV {path} contains no pg_accel-owned source records")
    return records


def _source_lines(repo: Path, relative: str) -> list[str]:
    path = repo / relative
    if not path.is_file():
        raise AuditError(f"registered source/test file does not exist: {relative}")
    return path.read_text(encoding="utf-8", errors="replace").splitlines()


def _matching_files(repo: Path, pattern: str) -> list[str]:
    parts = pattern.split("/")
    literal_prefix: list[str] = []
    for part in parts:
        if part == "**" or any(character in part for character in "*?["):
            break
        literal_prefix.append(part)

    search_root = repo.joinpath(*literal_prefix)
    if search_root.is_file():
        candidates = [search_root]
    elif search_root.is_dir():
        candidates = search_root.rglob("*")
    else:
        return []

    return sorted(
        str(path.relative_to(repo)).replace("\\", "/")
        for path in candidates
        if path.is_file()
        and _glob_matches(str(path.relative_to(repo)).replace("\\", "/"), pattern)
    )


def _glob_matches(path: str, pattern: str) -> bool:
    """Match normalized repository paths using the registry's stable semantics."""
    return fnmatch.fnmatchcase(path, pattern)


def _validate_registry(repo: Path, registry: dict[str, Any]) -> tuple[dict[str, Any], list[dict[str, str]]]:
    if registry.get("schema_version") != 1:
        raise AuditError("risk register schema_version must be 1")
    domains = registry.get("domains")
    if not isinstance(domains, list):
        raise AuditError("risk register domains must be a list")
    by_id: dict[str, Any] = {}
    for domain in domains:
        if not isinstance(domain, dict) or not isinstance(domain.get("id"), str):
            raise AuditError("every risk domain must be an object with a string id")
        domain_id = domain["id"]
        if domain_id in by_id:
            raise AuditError(f"duplicate risk domain: {domain_id}")
        by_id[domain_id] = domain
    missing = REQUIRED_DOMAINS - set(by_id)
    extra = set(by_id) - REQUIRED_DOMAINS
    if missing or extra:
        raise AuditError(
            f"risk domain inventory drift: missing={sorted(missing)}, extra={sorted(extra)}"
        )

    for domain_id, domain in by_id.items():
        description = domain.get("description")
        sources = domain.get("sources")
        tests = domain.get("tests")
        anchors = domain.get("coverage_anchors")
        if not isinstance(description, str) or not description.strip():
            raise AuditError(f"risk domain {domain_id} has no description")
        if not isinstance(sources, list) or not sources:
            raise AuditError(f"risk domain {domain_id} has no production sources")
        if not isinstance(tests, list) or not tests:
            raise AuditError(f"risk domain {domain_id} has no test evidence")
        if not isinstance(anchors, list) or not anchors:
            raise AuditError(f"risk domain {domain_id} has no LCOV anchor")

        for source in sources:
            if not isinstance(source, dict) or not isinstance(source.get("glob"), str):
                raise AuditError(f"risk domain {domain_id} has malformed source evidence")
            matches = _matching_files(repo, source["glob"])
            if not matches:
                raise AuditError(
                    f"risk domain {domain_id} source glob matches nothing: {source['glob']}"
                )
            contains = source.get("contains")
            if contains is not None:
                if not isinstance(contains, str) or not any(
                    contains in "\n".join(_source_lines(repo, path)) for path in matches
                ):
                    raise AuditError(
                        f"risk domain {domain_id} source marker is absent: {contains!r}"
                    )

        for test in tests:
            if not isinstance(test, dict) or not isinstance(test.get("path"), str) or not isinstance(test.get("symbol"), str):
                raise AuditError(f"risk domain {domain_id} has malformed test evidence")
            text = "\n".join(_source_lines(repo, test["path"]))
            symbol = re.escape(test["symbol"])
            if re.search(rf"\b{symbol}\b", text) is None:
                raise AuditError(
                    f"risk domain {domain_id} test symbol {test['symbol']!r} is absent from {test['path']}"
                )

        for anchor in anchors:
            if not isinstance(anchor, dict):
                raise AuditError(f"risk domain {domain_id} has malformed coverage anchor")
            if anchor.get("layer") not in {"rust", "cpp"}:
                raise AuditError(f"risk domain {domain_id} anchor has invalid layer")
            if not isinstance(anchor.get("path"), str) or not isinstance(anchor.get("regex"), str):
                raise AuditError(f"risk domain {domain_id} anchor lacks path/regex")
            lines = _source_lines(repo, anchor["path"])
            try:
                expression = re.compile(anchor["regex"])
            except re.error as error:
                raise AuditError(f"risk domain {domain_id} has invalid anchor regex: {error}") from error
            if not any(expression.search(line) for line in lines):
                raise AuditError(
                    f"risk domain {domain_id} anchor regex matches nothing in {anchor['path']}"
                )

    rules = registry.get("unsafe_rules")
    if not isinstance(rules, list) or not rules:
        raise AuditError("risk register unsafe_rules must be a non-empty list")
    normalized_rules: list[dict[str, str]] = []
    for rule in rules:
        if not isinstance(rule, dict) or not isinstance(rule.get("glob"), str) or not isinstance(rule.get("domain"), str):
            raise AuditError("every unsafe rule needs string glob/domain fields")
        if rule["domain"] not in by_id:
            raise AuditError(f"unsafe rule references unknown domain {rule['domain']}")
        if not _matching_files(repo, rule["glob"]):
            raise AuditError(f"unsafe rule glob matches no file: {rule['glob']}")
        normalized_rules.append({"glob": rule["glob"], "domain": rule["domain"]})
    return by_id, normalized_rules


def _unsafe_sites(repo: Path, baseline: dict[str, Any]) -> list[tuple[str, int]]:
    try:
        owned = baseline["rust"]["owned_files"]
    except (KeyError, TypeError) as error:
        raise AuditError("release baseline lacks rust.owned_files") from error
    if not isinstance(owned, list):
        raise AuditError("release baseline rust.owned_files must be a list")
    sites: list[tuple[str, int]] = []
    for relative in owned:
        if not isinstance(relative, str) or not relative.endswith(".rs"):
            continue
        for number, line in enumerate(_source_lines(repo, relative), 1):
            stripped = line.lstrip()
            if stripped.startswith(("//", "/*", "*")):
                continue
            if UNSAFE_SYNTAX.search(line):
                sites.append((relative, number))
    if not sites:
        raise AuditError("release baseline contains no Rust unsafe syntax sites")
    return sites


def _validate_unsafe_mapping(sites: list[tuple[str, int]], rules: list[dict[str, str]]) -> None:
    missing = [
        f"{path}:{line}"
        for path, line in sites
        if not any(_glob_matches(path, rule["glob"]) for rule in rules)
    ]
    if missing:
        preview = ", ".join(missing[:20])
        suffix = " ..." if len(missing) > 20 else ""
        raise AuditError(f"unregistered production unsafe sites: {preview}{suffix}")


def _validate_declaration_only_unsafe_sites(
    repo: Path,
    registry: dict[str, Any],
    sites: list[tuple[str, int]],
) -> set[tuple[str, int]]:
    entries = registry.get("declaration_only_unsafe_sites", [])
    if not isinstance(entries, list):
        raise AuditError("risk register declaration_only_unsafe_sites must be a list")
    registered: set[tuple[str, int]] = set()
    for entry in entries:
        if (
            not isinstance(entry, dict)
            or not isinstance(entry.get("path"), str)
            or not isinstance(entry.get("regex"), str)
            or not isinstance(entry.get("reason"), str)
            or not entry["reason"].strip()
        ):
            raise AuditError(
                "every declaration-only unsafe site needs path, regex, and reason strings"
            )
        path = entry["path"]
        try:
            expression = re.compile(entry["regex"])
        except re.error as error:
            raise AuditError(f"invalid declaration-only unsafe regex for {path}: {error}") from error
        source = _source_lines(repo, path)
        matches = [
            (path, line)
            for candidate_path, line in sites
            if candidate_path == path and expression.search(source[line - 1])
        ]
        if len(matches) != 1:
            raise AuditError(
                f"declaration-only unsafe registration must match exactly one unsafe site: "
                f"path={path} matches={matches}"
            )
        site = matches[0]
        declaration = source[site[1] - 1].strip()
        if "unsafe fn" not in declaration or not declaration.endswith(";"):
            raise AuditError(
                f"registered declaration-only unsafe site has an executable body: {path}:{site[1]}"
            )
        if site in registered:
            raise AuditError(f"duplicate declaration-only unsafe registration: {path}:{site[1]}")
        registered.add(site)
    return registered


def _validate_anchor_coverage(
    repo: Path,
    by_id: dict[str, Any],
    lcov: dict[str, dict[str, dict[int, int]]],
) -> dict[str, int]:
    covered: dict[str, int] = {}
    for domain_id, domain in by_id.items():
        count = 0
        for anchor in domain["coverage_anchors"]:
            layer = anchor["layer"]
            if layer not in lcov:
                continue
            path = anchor["path"]
            source = _source_lines(repo, path)
            expression = re.compile(anchor["regex"])
            matches = [index for index, line in enumerate(source, 1) if expression.search(line)]
            record = lcov[layer].get(path)
            if record is None:
                raise AuditError(f"{layer} LCOV has no registered risk source: {path}")
            window = int(anchor.get("window", 5))
            minimum = int(anchor.get("minimum_hits", 1))
            best = 0
            instrumented = False
            for line in matches:
                for candidate in range(max(1, line - window), line + window + 1):
                    if candidate in record:
                        instrumented = True
                        best = max(best, record[candidate])
            if not instrumented:
                raise AuditError(
                    f"risk anchor is absent from executable {layer} mapping: {path}:{matches}"
                )
            if best < minimum:
                raise AuditError(
                    f"risk anchor was not covered: domain={domain_id} path={path} regex={anchor['regex']!r} hits={best}"
                )
            count += 1
        if count == 0:
            raise AuditError(
                f"no supplied LCOV layer exercised a registered anchor for risk domain {domain_id}"
            )
        covered[domain_id] = count
    return covered


def _validate_unsafe_coverage(
    sites: list[tuple[str, int]],
    rust_lcov: dict[str, dict[int, int]],
    minimum: float,
    declaration_only_sites: set[tuple[str, int]] | None = None,
) -> tuple[int, int, float, int]:
    declaration_only_sites = declaration_only_sites or set()
    files = {path for path, _ in sites}
    missing_files = sorted(
        path
        for path in files
        if path not in rust_lcov
        and any(
            site_path == path and (site_path, line) not in declaration_only_sites
            for site_path, line in sites
        )
    )
    if missing_files:
        raise AuditError(f"Rust LCOV omits unsafe production files: {missing_files}")
    executable = 0
    covered = 0
    for path, line in sites:
        if (path, line) in declaration_only_sites:
            continue
        record = rust_lcov[path]
        nearby = [
            record[candidate]
            for candidate in range(max(1, line - 3), line + 4)
            if candidate in record
        ]
        # Function declarations and unsafe block braces often have no LLVM
        # region.  Their file still must map above; only executable-adjacent
        # sites enter the independent percentage denominator.
        if not nearby:
            continue
        executable += 1
        if any(hits > 0 for hits in nearby):
            covered += 1
    if executable == 0:
        raise AuditError("Rust LCOV maps no executable-adjacent unsafe sites")
    percent = covered * 100.0 / executable
    if percent + 1e-12 < minimum:
        raise AuditError(
            f"unsafe-adjacent coverage {percent:.3f}% is below {minimum:.3f}% ({covered}/{executable})"
        )
    return covered, executable, percent, len(declaration_only_sites)


def audit(args: argparse.Namespace) -> dict[str, Any]:
    repo = args.repo_root.resolve()
    registry = _load_json(args.registry)
    baseline = _load_json(args.baseline)
    by_id, rules = _validate_registry(repo, registry)
    sites = _unsafe_sites(repo, baseline)
    _validate_unsafe_mapping(sites, rules)
    declaration_only_sites = _validate_declaration_only_unsafe_sites(repo, registry, sites)

    lcov: dict[str, dict[str, dict[int, int]]] = {}
    if args.rust_lcov is not None:
        lcov["rust"] = _parse_lcov(args.rust_lcov)
    if args.cpp_lcov is not None:
        lcov["cpp"] = _parse_lcov(args.cpp_lcov)

    result: dict[str, Any] = {
        "kind": "pg-accel-risk-coverage-audit",
        "schema_version": 1,
        "status": "pass",
        "domain_count": len(by_id),
        "unsafe_site_count": len(sites),
        "unsafe_file_count": len({path for path, _ in sites}),
        "declaration_only_unsafe_site_count": len(declaration_only_sites),
        "lcov_checked": bool(lcov),
    }
    if lcov:
        result["covered_anchors"] = _validate_anchor_coverage(repo, by_id, lcov)
        if "rust" not in lcov:
            raise AuditError("risk-weighted coverage requires Rust LCOV")
        covered, executable, percent, declaration_only = _validate_unsafe_coverage(
            sites,
            lcov["rust"],
            args.minimum_unsafe_percent,
            declaration_only_sites,
        )
        result["unsafe_executable_sites"] = executable
        result["unsafe_covered_sites"] = covered
        result["unsafe_coverage_percent"] = round(percent, 6)
        result["minimum_unsafe_percent"] = args.minimum_unsafe_percent
        result["declaration_only_unsafe_sites"] = declaration_only
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path("."))
    parser.add_argument("--registry", type=Path, default=Path("coverage/risk-register.json"))
    parser.add_argument("--baseline", type=Path, default=Path("coverage/release-baseline.json"))
    parser.add_argument("--rust-lcov", type=Path)
    parser.add_argument("--cpp-lcov", type=Path)
    parser.add_argument("--minimum-unsafe-percent", type=float, default=90.0)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        result = audit(args)
    except AuditError as error:
        print(f"risk coverage audit failed: {error}", file=sys.stderr)
        return 1
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
