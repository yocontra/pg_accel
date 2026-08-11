#!/usr/bin/env python3
"""Audit historical crash bands and their structural retirement contracts."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


REQUIRED_LANES = {
    "retired_grouped_hash": ("HISTORICAL_UNSAFE_GROUPED_HASH_INPUT_ROWS", 100_000),
    "retired_row_returning_hash_join": (
        "HISTORICAL_UNSAFE_ROW_JOIN_BUILD_ROWS",
        100_000,
    ),
}


class AuditError(RuntimeError):
    pass


def _load(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise AuditError(f"cannot read {path}: {error}") from error
    if not isinstance(value, dict):
        raise AuditError("crash-band manifest must be one JSON object")
    return value


def _text(repo: Path, relative: str) -> str:
    path = repo / relative
    if not path.is_file():
        raise AuditError(f"registered crash-band source does not exist: {relative}")
    return path.read_text(encoding="utf-8", errors="replace")


def _function_body(source: str, name: str) -> str:
    match = re.search(rf"\b(?:const\s+)?fn\s+{re.escape(name)}\b[^{{]*\{{", source)
    if match is None:
        raise AuditError(f"required structural function is absent: {name}")
    depth = 1
    cursor = match.end()
    while cursor < len(source) and depth:
        if source[cursor] == "{":
            depth += 1
        elif source[cursor] == "}":
            depth -= 1
        cursor += 1
    if depth:
        raise AuditError(f"cannot parse structural function body: {name}")
    return source[match.end() : cursor - 1]


def audit(repo: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    if manifest.get("schema_version") != 1:
        raise AuditError("crash-band schema_version must be 1")
    lanes = manifest.get("historical_lanes")
    if not isinstance(lanes, list):
        raise AuditError("historical_lanes must be a list")
    by_id: dict[str, dict[str, Any]] = {}
    for lane in lanes:
        if not isinstance(lane, dict) or not isinstance(lane.get("id"), str):
            raise AuditError("every historical lane needs a string id")
        if lane["id"] in by_id:
            raise AuditError(f"duplicate historical lane: {lane['id']}")
        by_id[lane["id"]] = lane
    if set(by_id) != set(REQUIRED_LANES):
        raise AuditError(
            f"historical lane inventory drift: expected={sorted(REQUIRED_LANES)}, observed={sorted(by_id)}"
        )

    limits_source = _text(repo, "pg_accel/src/engine/cost/device_limits.rs")
    formulas_source = _text(repo, "pg_accel/src/engine/cost/formulas.rs")
    for lane_id, (constant, expected_rows) in REQUIRED_LANES.items():
        lane = by_id[lane_id]
        if lane.get("first_unsafe_rows") != expected_rows:
            raise AuditError(f"{lane_id} first unsafe row changed")
        if lane.get("limit_constant") != constant:
            raise AuditError(f"{lane_id} does not name its immutable limit constant")
        constant_match = re.search(
            rf"\b{re.escape(constant)}\s*:\s*usize\s*=\s*([0-9_]+)\s*;",
            limits_source,
        )
        if constant_match is None or int(constant_match.group(1).replace("_", "")) != expected_rows:
            raise AuditError(f"{constant} is absent or no longer equals {expected_rows}")
        for field in ("predicate", "boundary_test", "replacement_test", "planner_exposure"):
            if not isinstance(lane.get(field), str) or not lane[field]:
                raise AuditError(f"{lane_id} lacks {field}")
        if re.search(rf"\b{re.escape(lane['predicate'])}\b", formulas_source) is None:
            raise AuditError(f"{lane_id} safety predicate is absent")
        if re.search(rf"\b{re.escape(lane['boundary_test'])}\b", formulas_source) is None:
            raise AuditError(f"{lane_id} adjacent/extreme boundary test is absent")
        for expression in (f"{constant} - 1", constant, f"{constant} + 1"):
            if expression not in formulas_source:
                raise AuditError(
                    f"{lane_id} crash-band test does not pin boundary expression {expression!r}"
                )
    if "usize::MAX" not in formulas_source:
        raise AuditError("crash-band test does not pin usize::MAX")

    join_source = _text(repo, "pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs")
    row_join_body = _function_body(join_source, "selected_row_returning_gpu_join_available")
    if re.sub(r"//[^\n]*", "", row_join_body).strip() != "false":
        raise AuditError("row-returning GPU join structural gate is not literal false")
    hook_body = _function_body(join_source, "pgaccel_set_join_pathlist")
    hook_code = re.sub(r"//[^\n]*", "", hook_body)
    if "add_gpu_path" in hook_code or "CustomPath" in hook_code:
        raise AuditError("join pathlist hook contains a row-returning GPU path injector")

    custom_scan = _text(repo, "pg_accel/src/engine/ffi/custom_scan/mod.rs")
    begin_body = _function_body(custom_scan, "begin_custom_scan")
    if "GpuStrategy::Agg | GpuStrategy::Raster" not in begin_body:
        raise AuditError("resident executor does not explicitly restrict strategies to Agg/Raster")
    if "retired {:?} strategy has no registered executor" not in begin_body:
        raise AuditError("retired strategy execution no longer fails closed")

    evidence = manifest.get("required_evidence")
    if not isinstance(evidence, list) or not evidence:
        raise AuditError("required_evidence must be a non-empty list")
    for item in evidence:
        if not isinstance(item, dict) or not isinstance(item.get("path"), str) or not isinstance(item.get("symbol"), str):
            raise AuditError("malformed crash-band evidence entry")
        if re.search(rf"\b{re.escape(item['symbol'])}\b", _text(repo, item["path"])) is None:
            raise AuditError(
                f"crash-band evidence symbol {item['symbol']!r} is absent from {item['path']}"
            )

    cmake = _text(repo, "pgaccel-kernels/CMakeLists.txt")
    if "add_pgaccel_gpu_test(test_oom_invariant TIMEOUT 900)" not in cmake:
        raise AuditError("fixed 900-second OOM invariant is not registered")
    commands = manifest.get("execution_commands")
    if not isinstance(commands, list) or len(commands) < 4 or any(
        not isinstance(command, str) or not command for command in commands
    ):
        raise AuditError("crash-band execution command inventory is incomplete")

    return {
        "kind": "pg-accel-crash-band-audit",
        "schema_version": 1,
        "status": "pass",
        "historical_lane_count": len(by_id),
        "first_unsafe_rows": expected_rows,
        "row_returning_join_planner_selectable": False,
        "required_evidence_count": len(evidence),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", type=Path, default=Path("."))
    parser.add_argument("--manifest", type=Path, default=Path("coverage/crash-bands.json"))
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    try:
        result = audit(args.repo_root.resolve(), _load(args.manifest))
    except AuditError as error:
        print(f"crash-band audit failed: {error}", file=sys.stderr)
        return 1
    rendered = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
