#!/usr/bin/env python3
"""Validate deterministic property/fuzz target registration.

Release fuzzing is deliberately reproducible: bounded fixed-seed corpora run in
the ordinary Rust, pgrx, and CTest gates.  This manifest audit prevents a target
from silently disappearing when a test or parser is renamed.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


REQUIRED_TARGETS = {
    "private_data_codecs",
    "postgres_lists",
    "descriptors",
    "geometry_packed_inputs",
    "raster_packed_inputs",
    "h3_packed_inputs",
    "byte_cardinality_overflow",
    "pointer_aliasing",
    "c_abi_layouts",
}
RUNNERS = {"rust-lib", "pgrx", "ctest"}


class AuditError(RuntimeError):
    pass


def _load(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise AuditError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise AuditError("fuzz contract manifest must be one JSON object")
    return value


def _text(repo: Path, relative: str) -> str:
    path = repo / relative
    if not path.is_file():
        raise AuditError(f"registered fuzz source/test does not exist: {relative}")
    return path.read_text(encoding="utf-8", errors="replace")


def audit(repo: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    if manifest.get("schema_version") != 1:
        raise AuditError("fuzz contract schema_version must be 1")
    seed = manifest.get("seed")
    if not isinstance(seed, str) or re.fullmatch(r"0x[0-9a-f]{16}", seed) is None:
        raise AuditError("fuzz contract seed must be a fixed lowercase 64-bit hex string")
    commands = manifest.get("execution_commands")
    if not isinstance(commands, dict) or set(commands) != RUNNERS:
        raise AuditError("execution_commands must define rust-lib, pgrx, and ctest")
    if any(not isinstance(commands[name], str) or not commands[name].strip() for name in RUNNERS):
        raise AuditError("every fuzz execution command must be non-empty")

    targets = manifest.get("targets")
    if not isinstance(targets, list):
        raise AuditError("fuzz contract targets must be a list")
    by_id: dict[str, dict[str, Any]] = {}
    observed_runners: set[str] = set()
    total_cases = 0
    for target in targets:
        if not isinstance(target, dict) or not isinstance(target.get("id"), str):
            raise AuditError("every fuzz target needs a string id")
        target_id = target["id"]
        if target_id in by_id:
            raise AuditError(f"duplicate fuzz target: {target_id}")
        by_id[target_id] = target
        if target.get("fails_before_allocation_or_dereference") is not True:
            raise AuditError(
                f"fuzz target {target_id} does not require pre-allocation/pre-dereference rejection"
            )
        cases = target.get("minimum_cases")
        if not isinstance(cases, int) or isinstance(cases, bool) or cases <= 0:
            raise AuditError(f"fuzz target {target_id} has invalid minimum_cases")
        total_cases += cases
        malformed = target.get("malformed_classes")
        sources = target.get("sources")
        tests = target.get("tests")
        if not isinstance(malformed, list) or not malformed or any(
            not isinstance(item, str) or not item for item in malformed
        ):
            raise AuditError(f"fuzz target {target_id} has no malformed-class inventory")
        if not isinstance(sources, list) or not sources:
            raise AuditError(f"fuzz target {target_id} has no production sources")
        if not isinstance(tests, list) or not tests:
            raise AuditError(f"fuzz target {target_id} has no executable tests")
        for source in sources:
            if not isinstance(source, dict) or not isinstance(source.get("path"), str) or not isinstance(source.get("contains"), str):
                raise AuditError(f"fuzz target {target_id} has malformed source evidence")
            if source["contains"] not in _text(repo, source["path"]):
                raise AuditError(
                    f"fuzz target {target_id} source marker {source['contains']!r} is absent from {source['path']}"
                )
        for test in tests:
            if not isinstance(test, dict) or not isinstance(test.get("path"), str) or not isinstance(test.get("symbol"), str):
                raise AuditError(f"fuzz target {target_id} has malformed test evidence")
            runner = test.get("runner")
            if runner not in RUNNERS:
                raise AuditError(f"fuzz target {target_id} has invalid runner {runner!r}")
            observed_runners.add(runner)
            if re.search(rf"\b{re.escape(test['symbol'])}\b", _text(repo, test["path"])) is None:
                raise AuditError(
                    f"fuzz target {target_id} test symbol {test['symbol']!r} is absent from {test['path']}"
                )

    missing = REQUIRED_TARGETS - set(by_id)
    extra = set(by_id) - REQUIRED_TARGETS
    if missing or extra:
        raise AuditError(
            f"fuzz target inventory drift: missing={sorted(missing)}, extra={sorted(extra)}"
        )
    if observed_runners != RUNNERS:
        raise AuditError(f"fuzz targets do not exercise every runner: {sorted(observed_runners)}")
    if total_cases < 10_000:
        raise AuditError(f"registered deterministic corpus is too small: {total_cases} < 10000")
    return {
        "kind": "pg-accel-fuzz-contract-audit",
        "schema_version": 1,
        "status": "pass",
        "target_count": len(by_id),
        "minimum_case_count": total_cases,
        "runners": sorted(observed_runners),
        "seed": seed,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path("."))
    parser.add_argument("--manifest", type=Path, default=Path("coverage/fuzz-contracts.json"))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        result = audit(args.repo_root.resolve(), _load(args.manifest))
    except AuditError as error:
        print(f"fuzz contract audit failed: {error}", file=sys.stderr)
        return 1
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
