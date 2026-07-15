#!/usr/bin/env python3
"""Fail-closed helpers for pg_accel release coverage evidence."""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import fnmatch
import hashlib
import json
import math
import pathlib
import re
import shutil
import subprocess
import sys
from typing import Any, Iterable


SCHEMA_VERSION = 2
FIXED_THRESHOLD = 90.0
BASELINE_SQL_FILES = 52
BASELINE_SQL_ASSERTIONS = 287
BASELINE_CPP_SOURCES = 21
BASELINE_CPP_TESTS = 29
EXPECTED_LAYERS = ("rust", "cpp", "sql")
LINE_LAYERS = ("rust", "cpp")
REQUIRED_STAGES = {
    "rust": {
        "instrumentation",
        "clean",
        "production_build",
        "production_mapping",
        "production_tests",
        "supplemental_tests",
        "pgrx_tests",
        "toolchain",
        "coverage_report",
        "coverage_summary",
        "raw_evidence",
    },
    "cpp": {
        "toolchain",
        "clean",
        "configure",
        "build",
        "ctest",
        "gpu_evidence",
        "coverage_report",
        "coverage_summary",
        "raw_evidence",
    },
    "sql": {
        "extension_install",
        "extension_init",
        "sql_tests",
        "semantic_inventory",
        "raw_evidence",
    },
}

ASSERTION_LINE = re.compile(
    r"^\s*\\echo\s+(['\"])PGACCEL_ASSERT_OK:([a-z0-9][a-z0-9_.-]*)\1\s*$"
)
ASSERTION_NOTICE_LINE = re.compile(
    r"^\s*RAISE\s+NOTICE\s+(['\"])PGACCEL_ASSERT_OK:([a-z0-9][a-z0-9_.-]*)\1\s*;\s*$",
    flags=re.IGNORECASE,
)
ASSERTION_LOG_LINE = re.compile(
    r"^(?:.*?:\s+NOTICE:\s+)?PGACCEL_ASSERT_OK:([a-z0-9][a-z0-9_.-]*)$"
)
COMPLETION_LINE = re.compile(
    r"^\s*\\echo\s+(['\"])PGACCEL_FILE_OK:([a-z0-9][a-z0-9_.-]*)\1\s*$"
)
LEGACY_PASS_LINE = re.compile(
    r"^\s*\\echo\s+.*(?:PASS:|\bPASSED\b)", flags=re.IGNORECASE
)
SUSPICIOUS_LOG = re.compile(
    r"(?:\bWARNING:|\bSKIP(?:PED)?\b|\bcaught\b[^\n]*\bexception\b)",
    flags=re.IGNORECASE,
)
RUST_FUNCTION = re.compile(
    r"^\s*(?:(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?"
    r"(?:unsafe\s+)?(?:extern\s+(?:\"[^\"]+\"\s+)?)?)fn\s+[A-Za-z_][A-Za-z0-9_]*\s*(?:<[^;{]*>)?\s*\(",
    flags=re.MULTILINE,
)
CPP_FUNCTION_BODY = re.compile(
    r"\)\s*(?:const\s*)?(?:noexcept\s*)?(?:->[^\{]+)?\{", flags=re.DOTALL
)


class CoverageError(RuntimeError):
    """Coverage input is absent, malformed, or internally inconsistent."""


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def read_json(path: pathlib.Path) -> Any:
    def reject_constant(value: str) -> None:
        raise ValueError(f"non-finite JSON number {value}")

    try:
        return json.loads(
            path.read_text(encoding="utf-8"), parse_constant=reject_constant
        )
    except (OSError, ValueError) as exc:
        raise CoverageError(f"cannot read JSON {path}: {exc}") from exc


def write_json(path: pathlib.Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, allow_nan=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_threshold(value: float) -> float:
    if not math.isfinite(value):
        raise CoverageError(f"release coverage threshold must be finite; got {value}")
    if value < FIXED_THRESHOLD:
        raise CoverageError(
            f"release coverage threshold cannot be below {FIXED_THRESHOLD:g}%; got {value:g}%"
        )
    if value > 100.0:
        raise CoverageError(f"coverage threshold cannot exceed 100%; got {value:g}%")
    return value


def validate_thresholds(args: argparse.Namespace) -> int:
    values = [validate_threshold(float(value)) for value in args.values]
    print("coverage thresholds: PASS (" + ", ".join(f"{v:g}%" for v in values) + ")")
    return 0


def normalize_repo_path(repo_root: pathlib.Path, filename: str) -> str | None:
    candidate = pathlib.Path(filename)
    if not candidate.is_absolute():
        candidate = repo_root / candidate
    try:
        return candidate.resolve().relative_to(repo_root.resolve()).as_posix()
    except ValueError:
        return None


def excluded(path: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatch(path, pattern) for pattern in patterns)


def has_executable_mapping_candidate(path: pathlib.Path) -> bool:
    text = path.read_text(encoding="utf-8", errors="replace")
    if path.suffix == ".rs":
        return RUST_FUNCTION.search(text) is not None or "extension_sql!(" in text
    if path.suffix in {".cpp", ".hpp", ".h"}:
        return CPP_FUNCTION_BODY.search(text) is not None
    return True


def source_inventory(
    repo_root: pathlib.Path, scope: dict[str, Any]
) -> tuple[set[str], set[str]]:
    extensions = set(scope["extensions"])
    required_extensions = set(scope["required_extensions"])
    patterns = list(scope.get("exclude", []))
    executable_only = bool(scope.get("require_executable_mapping_only", False))
    included: set[str] = set()
    required: set[str] = set()
    for root_name in scope["roots"]:
        root = repo_root / root_name
        if root.is_file():
            paths: Iterable[pathlib.Path] = (root,)
        elif root.is_dir():
            paths = root.rglob("*")
        else:
            raise CoverageError(f"coverage scope root does not exist: {root_name}")
        for path in paths:
            if not path.is_file() or path.suffix not in extensions:
                continue
            relative = path.relative_to(repo_root).as_posix()
            if excluded(relative, patterns):
                continue
            included.add(relative)
            if path.suffix in required_extensions and (
                not executable_only or has_executable_mapping_candidate(path)
            ):
                required.add(relative)
    if not included:
        raise CoverageError("coverage scope resolved to zero source files")
    return included, required


def llvm_json_files(
    document: Any, repo_root: pathlib.Path
) -> dict[str, dict[str, Any]]:
    if (
        not isinstance(document, dict)
        or document.get("type") != "llvm.coverage.json.export"
        or not isinstance(document.get("version"), str)
        or not isinstance(document.get("data"), list)
        or not document["data"]
    ):
        raise CoverageError("LLVM coverage JSON has no data array")
    files: dict[str, dict[str, Any]] = {}
    for dataset in document["data"]:
        if not isinstance(dataset, dict) or not isinstance(
            dataset.get("files", []), list
        ):
            raise CoverageError("LLVM coverage dataset has an invalid files array")
        for entry in dataset.get("files", []):
            filename = entry.get("filename")
            summary = entry.get("summary")
            if not isinstance(filename, str) or not isinstance(summary, dict):
                raise CoverageError("LLVM file entry is missing filename or summary")
            relative = normalize_repo_path(repo_root, filename)
            if relative is None:
                continue
            if relative in files:
                raise CoverageError(f"duplicate LLVM coverage entry for {relative}")
            files[relative] = entry
    return files


def lcov_files(
    path: pathlib.Path, repo_root: pathlib.Path
) -> dict[str, dict[int, int]]:
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as exc:
        raise CoverageError(f"cannot read LCOV report {path}: {exc}") from exc
    files: dict[str, dict[int, int]] = {}
    current: str | None = None
    record_open = False
    saw_record = False
    for raw in lines:
        if raw.startswith("SF:"):
            if record_open or not raw[3:]:
                raise CoverageError(
                    f"invalid nested or empty LCOV source record: {raw}"
                )
            record_open = True
            relative = normalize_repo_path(repo_root, raw[3:])
            current = relative
            if relative is not None:
                if relative in files:
                    raise CoverageError(f"duplicate LCOV source record for {relative}")
                files[relative] = {}
            saw_record = True
        elif raw.startswith("DA:"):
            if not record_open:
                raise CoverageError(f"LCOV DA record is outside a source record: {raw}")
            if current is None:
                continue
            fields = raw[3:].split(",")
            if len(fields) < 2:
                raise CoverageError(f"invalid LCOV DA record: {raw}")
            try:
                line = int(fields[0])
                hits = int(fields[1])
            except ValueError as exc:
                raise CoverageError(f"invalid LCOV DA record: {raw}") from exc
            if line <= 0 or hits < 0 or line in files[current]:
                raise CoverageError(f"invalid or duplicate LCOV line record: {raw}")
            files[current][line] = hits
        elif raw == "end_of_record":
            if not record_open:
                raise CoverageError("LCOV end_of_record has no open source record")
            record_open = False
            current = None
    if record_open:
        raise CoverageError("LCOV source record is not terminated")
    if not saw_record:
        raise CoverageError("LCOV report contains no source records")
    return files


def initial_execution() -> dict[str, Any]:
    return {
        "status": "not_run",
        "exit_code": None,
        "stages_complete": False,
    }


def initial_mapping() -> dict[str, Any]:
    return {
        "owned_files": 0,
        "required_files": 0,
        "mapped_files": 0,
        "missing_required_files": [],
        "unexpected_owned_report_files": [],
    }


def initial_manifest_state() -> dict[str, Any]:
    return {
        "valid": False,
        "sha256": None,
        "declared_files": 0,
        "baseline_files": BASELINE_SQL_FILES,
        "declared_assertions": 0,
        "baseline_assertions": BASELINE_SQL_ASSERTIONS,
        "duplicate_declaration_ids": [],
        "hash_drift_files": [],
        "unknown_observation_ids": [],
        "duplicate_observation_ids": [],
        "warning_lines": [],
        "skip_lines": [],
        "caught_exception_lines": [],
        "completed_files": 0,
        "passed_test_files": 0,
        "test_files": 0,
    }


def initial_layer_summary(layer: str, threshold: float) -> dict[str, Any]:
    metric = "semantic_assertions" if layer == "sql" else "source_lines"
    summary: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "kind": "coverage-layer-summary",
        "generated_at_utc": utc_now(),
        "layer_id": layer,
        "metric_kind": metric,
        "description": {
            "rust": "Owned Rust production source coverage from the compiler-derived pg18 map without pg_test.",
            "cpp": "Owned C++/SYCL host-object source coverage; GPU device correctness is separate evidence.",
            "sql": "Fixed-manifest SQL semantic assertion coverage.",
        }[layer],
        "threshold_percent": validate_threshold(threshold),
        "covered_units": 0,
        "total_units": 0,
        "uncovered_units": 0,
        "percent": 0.0,
        "execution": initial_execution(),
        "errors": ["layer has not run"],
        "passed": False,
    }
    if layer in LINE_LAYERS:
        summary["mapping"] = initial_mapping()
        summary.update(
            {
                "covered_lines": 0,
                "line_count": 0,
                "uncovered_lines": 0,
                "line_percent": 0.0,
            }
        )
    else:
        summary["manifest"] = initial_manifest_state()
        summary.update(
            {
                "covered_assertions": 0,
                "assertion_count": 0,
                "uncovered_assertions": 0,
                "assertion_percent": 0.0,
            }
        )
    return summary


def initial_stage_status(layer: str) -> dict[str, Any]:
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "coverage-stage-status",
        "generated_at_utc": utc_now(),
        "layer_id": layer,
        "complete": False,
        "stages": {},
        "errors": ["layer has not run"],
    }


def init_artifacts(args: argparse.Namespace) -> int:
    artifact_dir = pathlib.Path(args.artifact_dir)
    thresholds = {
        "rust": validate_threshold(float(args.rust_threshold)),
        "cpp": validate_threshold(float(args.cpp_threshold)),
        "sql": validate_threshold(float(args.sql_threshold)),
    }
    artifact_dir.mkdir(parents=True, exist_ok=True)
    (artifact_dir / "partial.log").touch()
    for layer in (*EXPECTED_LAYERS, "sql-reachability"):
        directory = artifact_dir / layer
        if directory.exists():
            for child in directory.iterdir():
                if child.is_dir():
                    shutil.rmtree(child)
                else:
                    child.unlink()
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "profiles").mkdir(exist_ok=True)
        (directory / "partial.log").touch()
        if layer in EXPECTED_LAYERS:
            write_json(
                directory / "layer-summary.json",
                initial_layer_summary(layer, thresholds[layer]),
            )
            write_json(directory / "stage-status.json", initial_stage_status(layer))
            write_json(
                directory / "raw-evidence.json",
                {
                    "schema_version": SCHEMA_VERSION,
                    "kind": "coverage-raw-evidence",
                    "generated_at_utc": utc_now(),
                    "layer_id": layer,
                    "commit": None,
                    "scope_sha256": None,
                    "baseline_sha256": None,
                    "files": [],
                    "errors": ["raw evidence has not been sealed"],
                    "passed": False,
                },
            )
    write_json(
        artifact_dir / "sql-reachability/reachability-summary.json",
        {
            "schema_version": SCHEMA_VERSION,
            "kind": "sql-rust-reachability-summary",
            "generated_at_utc": utc_now(),
            "metric_kind": "non_threshold_source_lines",
            "status": "not_run",
            "line_count": 0,
            "reached_lines": 0,
            "line_percent": 0.0,
            "errors": ["reachability collection has not run"],
        },
    )
    write_json(
        artifact_dir / "cpp/gpu-correctness-evidence.json",
        {
            "schema_version": SCHEMA_VERSION,
            "kind": "gpu-correctness-evidence",
            "generated_at_utc": utc_now(),
            "status": "not_run",
            "execution_status": None,
            "ctest_log": "ctest.log",
            "oom_invariant_required": True,
            "oom_invariant_observed": False,
            "oom_invariant_passed": False,
            "pinned_ctest_count": 0,
            "passed_ctest_names": [],
            "raw_test_logs": {},
            "device": None,
            "families": {},
            "errors": ["GPU correctness evidence has not run"],
            "passed": False,
        },
    )
    write_json(
        artifact_dir / "gate-summary.json",
        {
            "schema_version": SCHEMA_VERSION,
            "kind": "coverage-gate-summary",
            "generated_at_utc": utc_now(),
            "gate": "pg_accel three-layer release coverage",
            "passed": False,
            "layers": {},
            "errors": ["aggregate has not run"],
        },
    )
    write_json(
        artifact_dir / "provenance.json",
        {
            "schema_version": SCHEMA_VERSION,
            "kind": "coverage-provenance",
            "generated_at_utc": utc_now(),
            "commit": None,
            "tree": "unknown",
            "scope_sha256": None,
            "baseline_sha256": None,
            "errors": ["provenance has not been captured"],
            "passed": False,
        },
    )
    (artifact_dir / "gate-summary.md").write_text(
        "# pg_accel three-layer coverage\n\n- aggregate: NOT RUN\n", encoding="utf-8"
    )
    return 0


def update_stage(
    artifact_dir: pathlib.Path,
    layer: str,
    stage: str,
    status: str,
    exit_code: int | None,
    error: str | None = None,
) -> None:
    path = artifact_dir / layer / "stage-status.json"
    try:
        document = read_json(path)
    except CoverageError:
        document = initial_stage_status(layer)
    document["generated_at_utc"] = utc_now()
    document["stages"][stage] = {
        "status": status,
        "exit_code": exit_code,
        "error": error,
    }
    if error:
        document["errors"] = [
            entry
            for entry in document.get("errors", [])
            if entry != "layer has not run"
        ]
        if error not in document["errors"]:
            document["errors"].append(error)
    write_json(path, document)


def mark_layer_error(args: argparse.Namespace) -> int:
    artifact_dir = pathlib.Path(args.artifact_dir)
    layer = args.layer
    exit_code = int(args.exit_code)
    summary_path = artifact_dir / layer / "layer-summary.json"
    try:
        summary = read_json(summary_path)
    except CoverageError:
        summary = initial_layer_summary(layer, float(args.threshold))
    errors = [
        entry for entry in summary.get("errors", []) if entry != "layer has not run"
    ]
    if args.message not in errors:
        errors.append(args.message)
    summary["generated_at_utc"] = utc_now()
    summary["errors"] = errors
    summary["execution"] = {
        "status": "failed",
        "exit_code": exit_code,
        "stages_complete": False,
    }
    summary["passed"] = False
    write_json(summary_path, summary)
    update_stage(artifact_dir, layer, args.stage, "failed", exit_code, args.message)
    return 1


def record_stage(args: argparse.Namespace) -> int:
    exit_code = int(args.exit_code)
    update_stage(
        pathlib.Path(args.artifact_dir),
        args.layer,
        args.stage,
        "complete" if exit_code == 0 else "failed",
        exit_code,
        args.message if exit_code != 0 else None,
    )
    return 0


def capture_provenance(args: argparse.Namespace) -> int:
    repo_root = pathlib.Path(args.repo_root).resolve()
    errors: list[str] = []
    try:
        commit_result = subprocess.run(
            ["git", "rev-parse", "--verify", "HEAD"],
            cwd=repo_root,
            check=False,
            capture_output=True,
            text=True,
        )
        commit = commit_result.stdout.strip()
        status_result = subprocess.run(
            ["git", "status", "--porcelain", "--untracked-files=normal"],
            cwd=repo_root,
            check=False,
            capture_output=True,
            text=True,
        )
        if (
            commit_result.returncode != 0
            or re.fullmatch(r"[0-9a-f]{40}", commit) is None
        ):
            errors.append("exact Git commit could not be resolved")
        if status_result.returncode != 0 or status_result.stdout:
            errors.append("release coverage requires a clean Git tree")
    except OSError as exc:
        commit = "unknown"
        errors.append(f"Git provenance failed: {exc}")
    scope_path = pathlib.Path(args.scope)
    baseline_path = pathlib.Path(args.baseline)
    if not scope_path.is_file() or not baseline_path.is_file():
        errors.append("scope or release baseline is missing")
    document = {
        "schema_version": SCHEMA_VERSION,
        "kind": "coverage-provenance",
        "generated_at_utc": utc_now(),
        "commit": commit,
        "tree": "clean" if not errors else "dirty",
        "scope_sha256": sha256(scope_path) if scope_path.is_file() else None,
        "baseline_sha256": sha256(baseline_path) if baseline_path.is_file() else None,
        "errors": errors,
        "passed": not errors,
    }
    write_json(pathlib.Path(args.output), document)
    return 0 if not errors else 1


RAW_DERIVED_NAMES = {
    "layer-summary.json",
    "stage-status.json",
    "raw-evidence.json",
    "coverage-summary.txt",
    "uncovered-files.tsv",
    "partial.log",
}


def seal_layer_evidence(args: argparse.Namespace) -> int:
    artifact_dir = pathlib.Path(args.artifact_dir).resolve()
    layer_dir = artifact_dir / args.layer
    provenance = read_json(artifact_dir / "provenance.json")
    errors: list[str] = []
    entries: list[dict[str, Any]] = []
    for path in sorted(layer_dir.rglob("*")):
        if not path.is_file() or path.name in RAW_DERIVED_NAMES:
            continue
        relative = path.relative_to(artifact_dir).as_posix()
        entries.append(
            {"path": relative, "sha256": sha256(path), "size": path.stat().st_size}
        )
    names = {entry["path"] for entry in entries}
    required = {
        "rust": {
            "rust/raw-lcov.info",
            "rust/production-map.info",
            "rust/raw-coverage.json",
            "rust/raw-summary.txt",
            "rust/coverage.profdata",
            "rust/production-config.json",
            "rust/toolchain.json",
        },
        "cpp": {
            "cpp/raw-coverage.json",
            "cpp/raw-lcov.info",
            "cpp/raw-summary.txt",
            "cpp/coverage.profdata",
            "cpp/toolchain.json",
            "cpp/ctest.log",
            "cpp/gpu-correctness-evidence.json",
        },
        "sql": {
            "sql/assertion-inventory.json",
            "sql/test-run/results.tsv",
        },
    }[args.layer]
    missing = sorted(required.difference(names))
    if missing:
        errors.append(f"required raw evidence is missing: {missing}")
    entry_sizes = {entry["path"]: entry["size"] for entry in entries}
    empty = sorted(
        name for name in required.intersection(names) if entry_sizes.get(name, 0) <= 0
    )
    if empty:
        errors.append(f"required raw evidence is empty: {empty}")
    if args.layer in LINE_LAYERS and not any(
        entry["path"].startswith(f"{args.layer}/profiles/")
        and entry["path"].endswith(".profraw")
        and entry["size"] > 0
        for entry in entries
    ):
        errors.append("nonempty LLVM profraw evidence is missing")
    if args.layer == "cpp" and not any(
        entry["path"].startswith("cpp/per-test-logs/")
        and entry["path"].endswith(".log")
        for entry in entries
    ):
        errors.append("retained per-test GPU logs are missing")
    if args.layer == "sql" and not any(
        entry["path"].startswith("sql/test-run/logs/")
        and entry["path"].endswith(".log")
        for entry in entries
    ):
        errors.append("retained SQL logs are missing")
    if not isinstance(provenance, dict) or provenance.get("passed") is not True:
        errors.append("clean exact-commit provenance is missing")
    document = {
        "schema_version": SCHEMA_VERSION,
        "kind": "coverage-raw-evidence",
        "generated_at_utc": utc_now(),
        "layer_id": args.layer,
        "commit": provenance.get("commit") if isinstance(provenance, dict) else None,
        "scope_sha256": provenance.get("scope_sha256")
        if isinstance(provenance, dict)
        else None,
        "baseline_sha256": provenance.get("baseline_sha256")
        if isinstance(provenance, dict)
        else None,
        "files": entries,
        "errors": errors,
        "passed": not errors,
    }
    write_json(layer_dir / "raw-evidence.json", document)
    return 0 if not errors else 1


def retained_stage_failures(
    artifact_dir: pathlib.Path, layer: str
) -> tuple[list[str], int]:
    """Return prior failed-stage errors so a later summary cannot erase them."""

    try:
        document = read_json(artifact_dir / layer / "stage-status.json")
    except CoverageError as exc:
        return [str(exc)], 1
    stages = document.get("stages") if isinstance(document, dict) else None
    if not isinstance(stages, dict):
        return [f"{layer} stage evidence is malformed"], 1
    errors: list[str] = []
    exit_code = 0
    for name, stage in stages.items():
        if not isinstance(stage, dict):
            errors.append(f"{name} stage evidence is malformed")
            exit_code = exit_code or 1
            continue
        code = stage.get("exit_code")
        if stage.get("status") != "complete" or code != 0:
            error = stage.get("error")
            errors.append(
                error if isinstance(error, str) and error else f"{name} failed"
            )
            exit_code = exit_code or (code if _is_int(code) and code > 0 else 1)
    return errors, exit_code


def summarize_layer(args: argparse.Namespace) -> int:
    repo_root = pathlib.Path(args.repo_root).resolve()
    scope_document = read_json(pathlib.Path(args.scope))
    try:
        layer_scope = scope_document["layers"][args.layer]
    except (KeyError, TypeError) as exc:
        raise CoverageError(f"unknown layer in scope file: {args.layer}") from exc
    threshold = validate_threshold(float(args.threshold))
    execution_status = int(args.execution_status)
    if execution_status < 0:
        raise CoverageError(f"execution status cannot be negative: {execution_status}")
    included, required = source_inventory(repo_root, layer_scope)

    production_reports: dict[str, dict[int, int]] | None = None
    if args.format == "lcov":
        reports: dict[str, Any] = lcov_files(pathlib.Path(args.input), repo_root)
    else:
        reports = llvm_json_files(read_json(pathlib.Path(args.input)), repo_root)
    if args.layer == "rust":
        production_map = getattr(args, "production_map", None)
        if args.format != "lcov" or not production_map:
            raise CoverageError(
                "Rust production coverage requires compiler-derived production LCOV"
            )
        if (
            layer_scope.get("production_mapping")
            != "compiler-derived-pg18-without-pg_test"
        ):
            raise CoverageError("Rust production mapping policy drifted")
        production_reports = lcov_files(pathlib.Path(production_map), repo_root)

    declared_reports = production_reports if production_reports is not None else reports
    mapped = sorted(included.intersection(declared_reports))
    missing_required = sorted(required.difference(declared_reports))
    unexpected_owned = sorted(set(declared_reports).difference(included))
    count = 0
    covered = 0
    rows: list[dict[str, Any]] = []
    supplemental_nonproduction_lines = 0
    for relative in mapped:
        if production_reports is not None:
            declared_lines = set(production_reports[relative])
            hits = reports.get(relative, {})
            file_count = len(declared_lines)
            file_covered = sum(1 for line in declared_lines if hits.get(line, 0) > 0)
            supplemental_nonproduction_lines += len(
                set(hits).difference(declared_lines)
            )
        elif args.format == "lcov":
            hits = reports[relative]
            file_count = len(hits)
            file_covered = sum(1 for value in hits.values() if value > 0)
        else:
            lines = reports[relative].get("summary", {}).get("lines")
            if not isinstance(lines, dict):
                raise CoverageError(f"LLVM summary for {relative} has no line metrics")
            file_count = int(lines.get("count", 0))
            file_covered = int(lines.get("covered", 0))
        if file_count < 0 or file_covered < 0 or file_covered > file_count:
            raise CoverageError(f"invalid line metrics for {relative}")
        count += file_count
        covered += file_covered
        rows.append(
            {
                "file": relative,
                "lines": file_count,
                "covered": file_covered,
                "uncovered": file_count - file_covered,
                "percent": 100.0
                if file_count == 0
                else file_covered * 100.0 / file_count,
            }
        )
    if count == 0:
        raise CoverageError(f"{args.layer} report contains zero owned executable lines")

    percent = covered * 100.0 / count
    errors: list[str] = []
    if missing_required:
        errors.append(f"missing {len(missing_required)} required source mappings")
    if execution_status != 0:
        errors.append(f"test or coverage execution exited {execution_status}")
    if percent < threshold:
        errors.append(f"source-line coverage {percent:.6f}% is below {threshold:g}%")
    if args.artifact_dir:
        retained_errors, retained_exit = retained_stage_failures(
            pathlib.Path(args.artifact_dir), args.layer
        )
        errors.extend(error for error in retained_errors if error not in errors)
        execution_status = execution_status or retained_exit
    passed = not errors
    output_dir = pathlib.Path(args.output_dir)
    result = initial_layer_summary(args.layer, threshold)
    result.update(
        {
            "generated_at_utc": utc_now(),
            "description": layer_scope["description"],
            "covered_units": covered,
            "total_units": count,
            "uncovered_units": count - covered,
            "percent": percent,
            "covered_lines": covered,
            "line_count": count,
            "uncovered_lines": count - covered,
            "line_percent": percent,
            "supplemental_nonproduction_lines": supplemental_nonproduction_lines,
            "production_mapping": {
                "policy": layer_scope.get("production_mapping"),
                "compiler_derived": production_reports is not None,
                "pg_feature": "pg18" if args.layer == "rust" else None,
                "pg_test_feature": False if args.layer == "rust" else None,
            },
            "mapping": {
                "owned_files": len(included),
                "required_files": len(required),
                "mapped_files": len(mapped),
                "missing_required_files": missing_required,
                "unexpected_owned_report_files": unexpected_owned,
            },
            "execution": {
                "status": "complete" if execution_status == 0 else "failed",
                "exit_code": execution_status,
                "stages_complete": True,
            },
            "errors": errors,
            "passed": passed,
        }
    )
    write_json(output_dir / "layer-summary.json", result)
    rows.sort(key=lambda row: (-row["uncovered"], row["file"]))
    with (output_dir / "uncovered-files.tsv").open(
        "w", encoding="utf-8", newline=""
    ) as handle:
        writer = csv.writer(handle, delimiter="\t", lineterminator="\n")
        writer.writerow(("file", "lines", "covered", "uncovered", "percent"))
        for row in rows:
            writer.writerow(
                (
                    row["file"],
                    row["lines"],
                    row["covered"],
                    row["uncovered"],
                    f"{row['percent']:.2f}",
                )
            )
        for relative in missing_required:
            writer.writerow((relative, "MISSING_MAPPING", "", "", ""))
    text = (
        f"{args.layer} source coverage: {'PASS' if passed else 'FAIL'}\n"
        f"lines: {covered}/{count} ({percent:.2f}%)\n"
        f"threshold: {threshold:.2f}%\n"
        f"mapped required files: {len(required) - len(missing_required)}/{len(required)}\n"
        f"supplemental non-production mapped lines: {supplemental_nonproduction_lines}\n"
        f"execution status: {execution_status}\n"
    )
    (output_dir / "coverage-summary.txt").write_text(text, encoding="utf-8")
    print(text, end="")
    if args.artifact_dir:
        update_stage(
            pathlib.Path(args.artifact_dir),
            args.layer,
            "coverage_summary",
            "complete" if passed else "failed",
            execution_status,
            None if passed else "; ".join(errors),
        )
        stage_path = pathlib.Path(args.artifact_dir) / args.layer / "stage-status.json"
        stage = read_json(stage_path)
        stage["complete"] = True
        stage["errors"] = errors
        write_json(stage_path, stage)
    return 0 if passed else 1


def summarize_reachability(args: argparse.Namespace) -> int:
    repo_root = pathlib.Path(args.repo_root).resolve()
    scope = read_json(pathlib.Path(args.scope))["layers"]["sql_reachability"]
    included, _ = source_inventory(repo_root, scope)
    reports = lcov_files(pathlib.Path(args.input), repo_root)
    count = 0
    reached = 0
    for relative in included.intersection(reports):
        count += len(reports[relative])
        reached += sum(1 for hits in reports[relative].values() if hits > 0)
    errors = (
        [] if count else ["reachability report contains zero owned executable lines"]
    )
    output = {
        "schema_version": SCHEMA_VERSION,
        "kind": "sql-rust-reachability-summary",
        "generated_at_utc": utc_now(),
        "metric_kind": "non_threshold_source_lines",
        "status": "complete" if not errors else "failed",
        "line_count": count,
        "reached_lines": reached,
        "line_percent": 0.0 if count == 0 else reached * 100.0 / count,
        "errors": errors,
    }
    write_json(pathlib.Path(args.output), output)
    return 0 if not errors else 1


def sql_executable_code(text: str) -> str:
    """Keep executable SQL/PLpgSQL tokens while erasing comments and literals."""

    output: list[str] = []
    index = 0
    block_depth = 0
    while index < len(text):
        if block_depth:
            if text.startswith("/*", index):
                block_depth += 1
                output.extend("  ")
                index += 2
            elif text.startswith("*/", index):
                block_depth -= 1
                output.extend("  ")
                index += 2
            else:
                output.append("\n" if text[index] == "\n" else " ")
                index += 1
            continue
        if text.startswith("--", index):
            newline = text.find("\n", index + 2)
            if newline < 0:
                output.extend(" " * (len(text) - index))
                break
            output.extend(" " * (newline - index))
            output.append("\n")
            index = newline + 1
            continue
        if text.startswith("/*", index):
            block_depth = 1
            output.extend("  ")
            index += 2
            continue
        char = text[index]
        if char in {"'", '"'}:
            quote = char
            output.append(" ")
            index += 1
            while index < len(text):
                if text[index] == quote:
                    output.append(" ")
                    if index + 1 < len(text) and text[index + 1] == quote:
                        output.append(" ")
                        index += 2
                        continue
                    index += 1
                    break
                output.append("\n" if text[index] == "\n" else " ")
                if text[index] == "\\" and index + 1 < len(text):
                    index += 1
                    output.append("\n" if text[index] == "\n" else " ")
                index += 1
            continue
        dollar = re.match(r"\$[A-Za-z_][A-Za-z0-9_]*\$|\$\$", text[index:])
        if dollar:
            delimiter = dollar.group(0)
            body_start = index + len(delimiter)
            body_end = text.find(delimiter, body_start)
            if body_end < 0:
                output.extend(" " * len(delimiter))
                index = body_start
                continue
            prior_code = "".join(output)
            executable_body = (
                re.search(r"\b(?:DO|AS)\s*$", prior_code, flags=re.IGNORECASE)
                is not None
            )
            output.extend(" " * len(delimiter))
            body = text[body_start:body_end]
            if executable_body:
                output.extend(sql_executable_code(body))
            else:
                output.extend("\n" if value == "\n" else " " for value in body)
            output.extend(" " * len(delimiter))
            index = body_end + len(delimiter)
            continue
        output.append(char)
        index += 1
    return "".join(output)


def read_sql_source_markers(
    path: pathlib.Path,
) -> tuple[list[dict[str, Any]], str | None, list[str]]:
    assertions: list[dict[str, Any]] = []
    completion: str | None = None
    errors: list[str] = []
    lines = path.read_text(encoding="utf-8").splitlines()
    completion_line = 0
    prior_assertion_line = 0
    for number, line in enumerate(lines, start=1):
        assertion = ASSERTION_LINE.fullmatch(line)
        assertion_notice = ASSERTION_NOTICE_LINE.fullmatch(line)
        file_marker = COMPLETION_LINE.fullmatch(line)
        if assertion or assertion_notice:
            declaration = assertion or assertion_notice
            assert declaration is not None
            guarded_source = sql_executable_code(
                "\n".join(lines[prior_assertion_line : number - 1])
            )
            if (
                re.search(r"\bRAISE\s+EXCEPTION\b", guarded_source, re.IGNORECASE)
                is None
            ):
                errors.append(
                    f"{path.name}:{number}: semantic ID has no distinct preceding failure guard"
                )
            assertions.append(
                {
                    "id": declaration.group(2),
                    "source_line": number,
                    "emission": "echo" if assertion else "notice",
                }
            )
            prior_assertion_line = number
        elif file_marker:
            if completion is not None:
                errors.append(f"{path.name}: duplicate file completion marker")
            completion = file_marker.group(2)
            completion_line = number
        elif "PGACCEL_ASSERT_OK:" in line or "PGACCEL_FILE_OK:" in line:
            errors.append(f"{path.name}:{number}: malformed or multi-ID evidence line")
        elif LEGACY_PASS_LINE.match(line):
            errors.append(
                f"{path.name}:{number}: legacy PASS/PASSED marker is not semantic evidence"
            )
    if completion is None:
        errors.append(f"{path.name}: missing file completion marker")
    elif completion != path.stem:
        errors.append(
            f"{path.name}: completion ID {completion!r} does not match filename"
        )
    else:
        for number, line in enumerate(
            lines[completion_line:], start=completion_line + 1
        ):
            stripped = line.strip()
            if stripped and not stripped.startswith("--"):
                errors.append(
                    f"{path.name}:{number}: SQL appears after file completion marker"
                )
    if not assertions:
        errors.append(f"{path.name}: no semantic assertion declarations")
    return assertions, completion, errors


def build_sql_manifest(args: argparse.Namespace) -> int:
    tests_dir = pathlib.Path(args.tests_dir).resolve()
    files = sorted(tests_dir.glob("[0-9]*.sql"))
    declarations: list[dict[str, Any]] = []
    errors: list[str] = []
    seen: set[str] = set()
    for path in files:
        assertions, completion, marker_errors = read_sql_source_markers(path)
        errors.extend(marker_errors)
        for assertion in assertions:
            identifier = assertion["id"]
            if identifier in seen:
                errors.append(f"duplicate assertion ID: {identifier}")
            seen.add(identifier)
        declarations.append(
            {
                "file": path.name,
                "sha256": sha256(path),
                "completion_id": completion,
                "assertions": assertions,
            }
        )
    if len(files) < BASELINE_SQL_FILES:
        errors.append(f"SQL file floor violated: {len(files)} < {BASELINE_SQL_FILES}")
    if len(seen) < BASELINE_SQL_ASSERTIONS:
        errors.append(
            f"SQL assertion floor violated: {len(seen)} < {BASELINE_SQL_ASSERTIONS}"
        )
    if errors:
        raise CoverageError("; ".join(errors))
    document = {
        "schema_version": SCHEMA_VERSION,
        "kind": "sql-semantic-assertion-manifest",
        "test_root": "sql/tests",
        "baseline_files": BASELINE_SQL_FILES,
        "baseline_assertions": BASELINE_SQL_ASSERTIONS,
        "declared_files": len(files),
        "declared_assertions": len(seen),
        "files": declarations,
    }
    baseline_path_value = getattr(args, "baseline", None)
    if hasattr(args, "baseline"):
        if not isinstance(baseline_path_value, str) or not baseline_path_value:
            raise CoverageError(
                "normal manifest generation requires a release baseline"
            )
        baseline_path = pathlib.Path(baseline_path_value)
        if not baseline_path.is_file():
            raise CoverageError(
                "release baseline is missing; use the explicit baseline update command"
            )
        baseline = read_json(baseline_path)
        sql_baseline = baseline.get("sql") if isinstance(baseline, dict) else None
        baseline_files = (
            sql_baseline.get("files") if isinstance(sql_baseline, dict) else None
        )
        baseline_ids = (
            sql_baseline.get("assertion_ids")
            if isinstance(sql_baseline, dict)
            else None
        )
        if baseline_files != [entry["file"] for entry in declarations]:
            raise CoverageError(
                "SQL file inventory differs from the immutable release baseline"
            )
        if baseline_ids != sorted(seen):
            raise CoverageError(
                "SQL assertion IDs differ from the immutable release baseline"
            )
    write_json(pathlib.Path(args.output), document)
    print(f"wrote SQL semantic manifest: {len(files)} files, {len(seen)} assertions")
    return 0


def validate_sql_manifest(
    manifest_path: pathlib.Path, tests_dir: pathlib.Path
) -> tuple[dict[str, Any], dict[str, tuple[str, int]], list[str]]:
    errors: list[str] = []
    document = read_json(manifest_path)
    if not isinstance(document, dict):
        raise CoverageError("SQL assertion manifest must be an object")
    if (
        document.get("schema_version") != SCHEMA_VERSION
        or document.get("kind") != "sql-semantic-assertion-manifest"
    ):
        errors.append("SQL manifest schema/kind mismatch")
    if document.get("baseline_files") != BASELINE_SQL_FILES:
        errors.append("SQL manifest file baseline changed")
    if document.get("baseline_assertions") != BASELINE_SQL_ASSERTIONS:
        errors.append("SQL manifest assertion baseline changed")
    entries = document.get("files")
    if not isinstance(entries, list):
        raise CoverageError("SQL manifest files must be an array")
    actual_files = sorted(path.name for path in tests_dir.glob("[0-9]*.sql"))
    manifest_names: list[str] = []
    declarations: dict[str, tuple[str, int]] = {}
    duplicate_ids: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("file"), str):
            errors.append("SQL manifest has an invalid file entry")
            continue
        name = entry["file"]
        manifest_names.append(name)
        path = tests_dir / name
        if not path.is_file():
            errors.append(f"manifest SQL file is missing: {name}")
            continue
        if entry.get("sha256") != sha256(path):
            errors.append(f"SQL source hash drift: {name}")
        source_assertions, completion, source_errors = read_sql_source_markers(path)
        errors.extend(source_errors)
        if entry.get("completion_id") != completion:
            errors.append(f"SQL completion declaration drift: {name}")
        listed = entry.get("assertions")
        if not isinstance(listed, list) or listed != source_assertions:
            errors.append(f"SQL assertion declaration/line drift: {name}")
            continue
        for assertion in listed:
            identifier = assertion.get("id")
            source_line = assertion.get("source_line")
            if not isinstance(identifier, str) or not isinstance(source_line, int):
                errors.append(f"invalid SQL assertion declaration in {name}")
                continue
            if identifier in declarations:
                duplicate_ids.add(identifier)
            declarations[identifier] = (name, source_line)
    if len(manifest_names) != len(set(manifest_names)):
        errors.append("SQL manifest contains duplicate file entries")
    if sorted(manifest_names) != actual_files:
        errors.append("SQL file set differs from the fixed manifest")
    if duplicate_ids:
        errors.append(f"duplicate SQL assertion IDs: {sorted(duplicate_ids)}")
    declared_files = len(set(manifest_names))
    declared_assertions = len(declarations)
    if (
        declared_files < BASELINE_SQL_FILES
        or document.get("declared_files") != declared_files
    ):
        errors.append("SQL manifest file count is below baseline or inconsistent")
    if (
        declared_assertions < BASELINE_SQL_ASSERTIONS
        or document.get("declared_assertions") != declared_assertions
    ):
        errors.append("SQL manifest assertion count is below baseline or inconsistent")
    return document, declarations, errors


def cmake_registered_tests(cmake: str) -> list[str]:
    return sorted(
        re.findall(
            r"^add_pgaccel_gpu_test\(([A-Za-z0-9_.-]+)(?:\s+TIMEOUT\s+[0-9]+)?\)\s*$",
            cmake,
            flags=re.MULTILINE,
        )
    )


def release_baseline_document(
    repo_root: pathlib.Path, scope_path: pathlib.Path, manifest_path: pathlib.Path
) -> dict[str, Any]:
    scope = read_json(scope_path)
    rust_scope = scope["layers"]["rust"]
    rust_files, _ = source_inventory(repo_root, rust_scope)
    cpp_sources = sorted(
        path.relative_to(repo_root).as_posix()
        for path in (repo_root / "pgaccel-kernels/src").glob("*.cpp")
    )
    cmake = (repo_root / "pgaccel-kernels/CMakeLists.txt").read_text(encoding="utf-8")
    manifest, declarations, manifest_errors = validate_sql_manifest(
        manifest_path, repo_root / "sql/tests"
    )
    if manifest_errors:
        raise CoverageError("; ".join(manifest_errors))
    manifest_files = manifest.get("files", [])
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "coverage-release-baseline",
        "minimum_percent": FIXED_THRESHOLD,
        "rust": {
            "roots": rust_scope["roots"],
            "exclude": rust_scope.get("exclude", []),
            "production_feature": "pg18",
            "forbidden_production_feature": "pg_test",
            "mapping_policy": "compiler-derived-pg18-without-pg_test",
            "owned_files": sorted(rust_files),
            "required_mapping_files": sorted(rust_files),
        },
        "cpp": {
            "sources": cpp_sources,
            "ctest_names": cmake_registered_tests(cmake),
            "oom_families": [
                "reduce_f64",
                "sort_f64",
                "hashagg_f64",
                "spatial_f64",
                "h3_f64",
            ],
        },
        "sql": {
            "files": [entry["file"] for entry in manifest_files],
            "assertion_ids": sorted(declarations),
        },
    }


def update_release_baseline(args: argparse.Namespace) -> int:
    if args.acknowledge_review_visible_update != "UPDATE-RELEASE-BASELINE":
        raise CoverageError(
            "baseline updates require --acknowledge-review-visible-update "
            "UPDATE-RELEASE-BASELINE"
        )
    document = release_baseline_document(
        pathlib.Path(args.repo_root).resolve(),
        pathlib.Path(args.scope),
        pathlib.Path(args.manifest),
    )
    write_json(pathlib.Path(args.output), document)
    print(
        "updated review-visible release baseline: "
        f"{len(document['rust']['owned_files'])} Rust files, "
        f"{len(document['cpp']['sources'])} C++ sources, "
        f"{len(document['cpp']['ctest_names'])} CTests, "
        f"{len(document['sql']['files'])} SQL files, "
        f"{len(document['sql']['assertion_ids'])} SQL assertions"
    )
    return 0


def read_sql_results(path: pathlib.Path) -> dict[str, dict[str, str]]:
    try:
        with path.open(encoding="utf-8", newline="") as handle:
            reader = csv.DictReader(handle, delimiter="\t")
            required = {"file", "status", "exit_code", "log"}
            if reader.fieldnames is None or not required.issubset(reader.fieldnames):
                raise CoverageError(f"SQL results {path} has an invalid header")
            rows = list(reader)
    except OSError as exc:
        raise CoverageError(f"cannot read SQL results {path}: {exc}") from exc
    results: dict[str, dict[str, str]] = {}
    for row in rows:
        name = row["file"]
        if name in results:
            raise CoverageError(f"duplicate SQL result for {name}")
        results[name] = row
    return results


def sql_inventory(args: argparse.Namespace) -> int:
    tests_dir = pathlib.Path(args.tests_dir).resolve()
    results_path = pathlib.Path(args.results).resolve()
    manifest_path = pathlib.Path(args.manifest).resolve()
    output_dir = pathlib.Path(args.output_dir)
    errors: list[str] = []
    manifest_validation_errors: list[str] = []
    try:
        manifest, declarations, manifest_errors = validate_sql_manifest(
            manifest_path, tests_dir
        )
        manifest_validation_errors.extend(manifest_errors)
        errors.extend(manifest_errors)
    except CoverageError as exc:
        manifest = {"files": [], "declared_files": 0, "declared_assertions": 0}
        declarations = {}
        manifest_validation_errors.append(str(exc))
        errors.append(str(exc))
    try:
        results = read_sql_results(results_path)
    except CoverageError as exc:
        results = {}
        errors.append(str(exc))

    observed: set[str] = set()
    duplicate_observations: set[str] = set()
    unknown_observations: set[str] = set()
    warning_lines: list[str] = []
    skip_lines: list[str] = []
    caught_lines: list[str] = []
    file_rows: list[dict[str, Any]] = []
    passed_files = 0
    completed_files = 0
    expected_files = [
        entry.get("file")
        for entry in manifest.get("files", [])
        if isinstance(entry, dict)
    ]
    for name in expected_files:
        result = results.get(name)
        if result is None:
            errors.append(f"missing execution result: {name}")
            continue
        expected_log = f"logs/{name}.log"
        exit_text = result.get("exit_code", "")
        try:
            exit_code = int(exit_text)
        except ValueError:
            exit_code = -1
            errors.append(f"invalid SQL exit code for {name}: {exit_text!r}")
        passed = result.get("status") == "pass" and exit_code == 0
        if passed:
            passed_files += 1
        if result.get("log") != expected_log:
            errors.append(
                f"SQL result for {name} has unexpected log path: {result.get('log')!r}"
            )
            log_text = ""
            log_path = None
        else:
            log_path = results_path.parent / expected_log
            try:
                log_text = log_path.read_text(encoding="utf-8", errors="replace")
            except OSError as exc:
                errors.append(f"cannot read SQL log for {name}: {exc}")
                log_text = ""
        file_assertions: set[str] = set()
        completion_count = 0
        for line_number, line in enumerate(log_text.splitlines(), start=1):
            assertion = ASSERTION_LOG_LINE.fullmatch(line)
            completion = re.fullmatch(r"PGACCEL_FILE_OK:([a-z0-9][a-z0-9_.-]*)", line)
            prefix_count = line.count("PGACCEL_ASSERT_OK:") + line.count(
                "PGACCEL_FILE_OK:"
            )
            if prefix_count > 1 or (
                prefix_count == 1 and assertion is None and completion is None
            ):
                errors.append(
                    f"{name} log line {line_number} credits malformed or multiple declarations"
                )
            if assertion:
                identifier = assertion.group(1)
                owner = declarations.get(identifier)
                if owner is None or owner[0] != name:
                    unknown_observations.add(identifier)
                elif identifier in file_assertions or identifier in observed:
                    duplicate_observations.add(identifier)
                elif passed:
                    file_assertions.add(identifier)
                    observed.add(identifier)
            if completion:
                if completion.group(1) == pathlib.Path(name).stem:
                    completion_count += 1
                else:
                    errors.append(
                        f"wrong completion ID in {name} log line {line_number}"
                    )
            suspicious = SUSPICIOUS_LOG.search(line)
            if suspicious:
                label = suspicious.group(0).lower()
                rendered = f"{name}:{line_number}:{line}"
                if "warning" in label:
                    warning_lines.append(rendered)
                elif "skip" in label:
                    skip_lines.append(rendered)
                else:
                    caught_lines.append(rendered)
        if completion_count == 1 and passed:
            completed_files += 1
        else:
            errors.append(
                f"{name} has {completion_count} successful file completion markers"
            )
        file_rows.append(
            {
                "file": name,
                "status": result.get("status"),
                "exit_code": exit_code,
                "observed_assertions": len(file_assertions),
                "completion_markers": completion_count,
                "log": result.get("log"),
                "log_sha256": sha256(log_path)
                if log_path and log_path.is_file()
                else None,
            }
        )
    unknown_results = sorted(set(results).difference(expected_files))
    if unknown_results:
        errors.append(f"results contain unknown SQL files: {unknown_results}")
    if unknown_observations:
        errors.append(
            f"unknown or wrong-file SQL assertion observations: {sorted(unknown_observations)}"
        )
    if duplicate_observations:
        errors.append(
            f"duplicate SQL assertion observations: {sorted(duplicate_observations)}"
        )
    if warning_lines:
        errors.append(f"SQL logs contain {len(warning_lines)} warning lines")
    if skip_lines:
        errors.append(f"SQL logs contain {len(skip_lines)} skip lines")
    if caught_lines:
        errors.append(f"SQL logs contain {len(caught_lines)} caught-exception lines")

    total = len(declarations)
    covered = len(observed)
    percent = 0.0 if total == 0 else covered * 100.0 / total
    threshold = validate_threshold(float(args.threshold))
    execution_status = int(args.execution_status)
    if execution_status != 0:
        errors.append(f"SQL execution exited {execution_status}")
    if passed_files != len(expected_files) or completed_files != len(expected_files):
        errors.append("not every manifest SQL file completed successfully")
    if percent < threshold:
        errors.append(
            f"semantic assertion coverage {percent:.6f}% is below {threshold:g}%"
        )
    if args.artifact_dir:
        retained_errors, retained_exit = retained_stage_failures(
            pathlib.Path(args.artifact_dir), "sql"
        )
        errors.extend(error for error in retained_errors if error not in errors)
        execution_status = execution_status or retained_exit
    manifest_state = initial_manifest_state()
    manifest_state.update(
        {
            "valid": not manifest_validation_errors
            and total >= BASELINE_SQL_ASSERTIONS
            and len(expected_files) >= BASELINE_SQL_FILES,
            "sha256": sha256(manifest_path) if manifest_path.is_file() else None,
            "declared_files": len(expected_files),
            "declared_assertions": total,
            "duplicate_declaration_ids": [],
            "hash_drift_files": [
                entry for entry in errors if entry.startswith("SQL source hash drift:")
            ],
            "unknown_observation_ids": sorted(unknown_observations),
            "duplicate_observation_ids": sorted(duplicate_observations),
            "warning_lines": warning_lines,
            "skip_lines": skip_lines,
            "caught_exception_lines": caught_lines,
            "completed_files": completed_files,
            "passed_test_files": passed_files,
            "test_files": len(expected_files),
        }
    )
    passed = not errors and total > 0
    summary = initial_layer_summary("sql", threshold)
    summary.update(
        {
            "generated_at_utc": utc_now(),
            "covered_units": covered,
            "total_units": total,
            "uncovered_units": total - covered,
            "percent": percent,
            "covered_assertions": covered,
            "assertion_count": total,
            "uncovered_assertions": total - covered,
            "assertion_percent": percent,
            "execution": {
                "status": "complete"
                if execution_status == 0 and passed_files == len(expected_files)
                else "failed",
                "exit_code": execution_status,
                "stages_complete": True,
            },
            "manifest": manifest_state,
            "errors": errors,
            "passed": passed,
        }
    )
    inventory = {
        "schema_version": SCHEMA_VERSION,
        "kind": "sql-semantic-assertion-inventory",
        "generated_at_utc": utc_now(),
        "manifest_sha256": manifest_state["sha256"],
        "declared_assertions": total,
        "successful_assertions": covered,
        "assertion_percent": percent,
        "declared_files": len(expected_files),
        "passed_files": passed_files,
        "completed_files": completed_files,
        "successful_assertion_ids": sorted(observed),
        "errors": errors,
        "complete": passed,
        "files": file_rows,
    }
    output_dir.mkdir(parents=True, exist_ok=True)
    write_json(output_dir / "layer-summary.json", summary)
    write_json(output_dir / "assertion-inventory.json", inventory)
    text = (
        f"SQL semantic assertion coverage: {'PASS' if passed else 'FAIL'}\n"
        f"assertions: {covered}/{total} ({percent:.2f}%)\n"
        f"files completed: {completed_files}/{len(expected_files)}\n"
        f"threshold: {threshold:.2f}%\n"
    )
    (output_dir / "coverage-summary.txt").write_text(text, encoding="utf-8")
    print(text, end="")
    if args.artifact_dir:
        update_stage(
            pathlib.Path(args.artifact_dir),
            "sql",
            "semantic_inventory",
            "complete" if passed else "failed",
            execution_status,
            None if passed else "; ".join(errors),
        )
        stage_path = pathlib.Path(args.artifact_dir) / "sql/stage-status.json"
        stage = read_json(stage_path)
        stage["complete"] = True
        stage["errors"] = errors
        write_json(stage_path, stage)
    return 0 if passed else 1


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def validate_layer_summary(summary: Any, expected: str) -> list[str]:
    errors: list[str] = []
    prefix = f"{expected}:"
    if not isinstance(summary, dict):
        return [f"{prefix} summary is not an object"]
    if (
        summary.get("schema_version") != SCHEMA_VERSION
        or summary.get("kind") != "coverage-layer-summary"
    ):
        errors.append(f"{prefix} wrong summary schema/kind")
    if summary.get("layer_id") != expected:
        errors.append(f"{prefix} layer ID mismatch")
    expected_metric = "semantic_assertions" if expected == "sql" else "source_lines"
    if summary.get("metric_kind") != expected_metric:
        errors.append(f"{prefix} metric kind mismatch")
    threshold = 101.0
    try:
        threshold = validate_threshold(float(summary.get("threshold_percent")))
    except (CoverageError, TypeError, ValueError):
        errors.append(f"{prefix} invalid threshold")
    total = summary.get("total_units")
    covered = summary.get("covered_units")
    uncovered = summary.get("uncovered_units")
    percent = summary.get("percent")
    if not all(_is_int(value) and value >= 0 for value in (total, covered, uncovered)):
        errors.append(f"{prefix} invalid unit arithmetic types")
        total = covered = uncovered = 0
    elif covered > total or uncovered != total - covered:
        errors.append(f"{prefix} inconsistent unit arithmetic")
    expected_percent = 0.0 if total == 0 else covered * 100.0 / total
    if (
        not isinstance(percent, (int, float))
        or not math.isfinite(float(percent))
        or not math.isclose(float(percent), expected_percent, rel_tol=0.0, abs_tol=1e-9)
    ):
        errors.append(f"{prefix} inconsistent percentage")
    execution = summary.get("execution")
    execution_ok = False
    if not isinstance(execution, dict):
        errors.append(f"{prefix} missing execution object")
    else:
        status = execution.get("status")
        exit_code = execution.get("exit_code")
        complete = execution.get("stages_complete")
        if (
            status not in {"not_run", "failed", "complete"}
            or (exit_code is not None and not _is_int(exit_code))
            or not isinstance(complete, bool)
        ):
            errors.append(f"{prefix} invalid execution state")
        execution_ok = status == "complete" and exit_code == 0 and complete is True
    listed_errors = summary.get("errors")
    if not isinstance(listed_errors, list) or not all(
        isinstance(value, str) for value in listed_errors
    ):
        errors.append(f"{prefix} invalid errors array")
        listed_errors = ["invalid"]

    mapping_ok = True
    manifest_ok = True
    if expected in LINE_LAYERS:
        mapping = summary.get("mapping")
        if not isinstance(mapping, dict):
            errors.append(f"{prefix} missing mapping object")
            mapping_ok = False
        else:
            owned = mapping.get("owned_files")
            required = mapping.get("required_files")
            mapped = mapping.get("mapped_files")
            missing = mapping.get("missing_required_files")
            unexpected = mapping.get("unexpected_owned_report_files")
            if (
                not all(_is_int(v) and v >= 0 for v in (owned, required, mapped))
                or not isinstance(missing, list)
                or not isinstance(unexpected, list)
            ):
                errors.append(f"{prefix} invalid mapping counts")
                mapping_ok = False
            elif (
                required > owned
                or mapped > owned
                or len(missing) > required
                or not all(isinstance(value, str) for value in missing)
                or len(set(missing)) != len(missing)
                or required - len(missing) > mapped
            ):
                errors.append(f"{prefix} impossible mapping summary")
                mapping_ok = False
            elif missing:
                errors.append(f"{prefix} required source mappings are missing")
                mapping_ok = False
        aliases = (
            summary.get("line_count"),
            summary.get("covered_lines"),
            summary.get("uncovered_lines"),
            summary.get("line_percent"),
        )
        if (
            aliases[:3] != (total, covered, uncovered)
            or not isinstance(aliases[3], (int, float))
            or not math.isclose(float(aliases[3]), expected_percent, abs_tol=1e-9)
        ):
            errors.append(f"{prefix} inconsistent line aliases")
    else:
        manifest = summary.get("manifest")
        if not isinstance(manifest, dict):
            errors.append(f"{prefix} missing manifest state")
            manifest_ok = False
        else:
            required_lists = (
                "duplicate_declaration_ids",
                "hash_drift_files",
                "unknown_observation_ids",
                "duplicate_observation_ids",
                "warning_lines",
                "skip_lines",
                "caught_exception_lines",
            )
            if (
                manifest.get("valid") is not True
                or manifest.get("baseline_files") != BASELINE_SQL_FILES
                or manifest.get("baseline_assertions") != BASELINE_SQL_ASSERTIONS
            ):
                errors.append(f"{prefix} SQL manifest is invalid")
                manifest_ok = False
            manifest_hash = manifest.get("sha256")
            if (
                not isinstance(manifest_hash, str)
                or re.fullmatch(r"[0-9a-f]{64}", manifest_hash) is None
            ):
                errors.append(f"{prefix} SQL manifest hash is invalid")
                manifest_ok = False
            declared_files = manifest.get("declared_files")
            declared_assertions = manifest.get("declared_assertions")
            if (
                not _is_int(declared_files)
                or declared_files < BASELINE_SQL_FILES
                or declared_assertions != total
            ):
                errors.append(f"{prefix} SQL manifest counts are invalid")
                manifest_ok = False
            if any(
                not isinstance(manifest.get(key), list) or manifest.get(key)
                for key in required_lists
            ):
                errors.append(
                    f"{prefix} SQL manifest/evidence has drift or forbidden observations"
                )
                manifest_ok = False
            test_files = manifest.get("test_files")
            if (
                not _is_int(test_files)
                or test_files != declared_files
                or manifest.get("passed_test_files") != test_files
                or manifest.get("completed_files") != test_files
            ):
                errors.append(f"{prefix} SQL execution is incomplete")
                manifest_ok = False
        aliases = (
            summary.get("assertion_count"),
            summary.get("covered_assertions"),
            summary.get("uncovered_assertions"),
            summary.get("assertion_percent"),
        )
        if (
            aliases[:3] != (total, covered, uncovered)
            or not isinstance(aliases[3], (int, float))
            or not math.isclose(float(aliases[3]), expected_percent, abs_tol=1e-9)
        ):
            errors.append(f"{prefix} inconsistent assertion aliases")

    computed_pass = (
        total > 0
        and expected_percent >= threshold
        and execution_ok
        and not listed_errors
        and mapping_ok
        and manifest_ok
    )
    if summary.get("passed") is not computed_pass:
        errors.append(f"{prefix} passed flag does not match recomputed result")
    if not computed_pass:
        errors.append(f"{prefix} layer did not satisfy the release gate")
    return errors


def validate_stage_status(document: Any, expected: str) -> list[str]:
    if not isinstance(document, dict):
        return [f"{expected}: stage status is not an object"]
    errors = []
    if (
        document.get("schema_version") != SCHEMA_VERSION
        or document.get("kind") != "coverage-stage-status"
        or document.get("layer_id") != expected
    ):
        errors.append(f"{expected}: invalid stage status schema/identity")
    if document.get("complete") is not True:
        errors.append(f"{expected}: stage execution is incomplete")
    stages = document.get("stages")
    if not isinstance(stages, dict) or not stages:
        errors.append(f"{expected}: no stage records")
    else:
        missing = sorted(REQUIRED_STAGES[expected].difference(stages))
        if missing:
            errors.append(f"{expected}: required stages are missing: {missing}")
        if any(
            not isinstance(value, dict)
            or value.get("status") != "complete"
            or value.get("exit_code") != 0
            for value in stages.values()
        ):
            errors.append(f"{expected}: a stage failed or is incomplete")
    return errors


def validate_aggregate_provenance(
    artifact_dir: pathlib.Path, repo_root: pathlib.Path
) -> tuple[dict[str, Any], list[str]]:
    errors: list[str] = []
    try:
        document = read_json(artifact_dir / "provenance.json")
    except CoverageError as exc:
        return {}, [str(exc)]
    if not isinstance(document, dict):
        return {}, ["coverage provenance is not an object"]
    commit = document.get("commit")
    if (
        document.get("schema_version") != SCHEMA_VERSION
        or document.get("kind") != "coverage-provenance"
        or document.get("tree") != "clean"
        or document.get("passed") is not True
        or document.get("errors") != []
        or not isinstance(commit, str)
        or re.fullmatch(r"[0-9a-f]{40}", commit) is None
        or commit.startswith("deadbeef")
        or len(set(commit)) == 1
    ):
        errors.append("coverage provenance is invalid, dirty, or non-exact")
    try:
        commit_result = subprocess.run(
            ["git", "rev-parse", "--verify", "HEAD"],
            cwd=repo_root,
            check=False,
            capture_output=True,
            text=True,
        )
        status_result = subprocess.run(
            ["git", "status", "--porcelain", "--untracked-files=normal"],
            cwd=repo_root,
            check=False,
            capture_output=True,
            text=True,
        )
        actual_commit = commit_result.stdout.strip()
        if commit != actual_commit:
            errors.append("coverage provenance commit does not match the checkout")
        if (
            commit_result.returncode != 0
            or status_result.returncode != 0
            or status_result.stdout
        ):
            errors.append("aggregate requires the exact clean provenance checkout")
    except OSError as exc:
        errors.append(f"cannot verify aggregate Git commit: {exc}")
    for name, field in (
        ("scope.json", "scope_sha256"),
        ("release-baseline.json", "baseline_sha256"),
    ):
        copied = artifact_dir / name
        current = repo_root / "coverage" / name
        if (
            not copied.is_file()
            or not current.is_file()
            or document.get(field) != sha256(copied)
            or sha256(copied) != sha256(current)
        ):
            errors.append(f"coverage provenance {name} hash drifted")
    return document, errors


def validate_raw_evidence_manifest(
    artifact_dir: pathlib.Path,
    layer: str,
    provenance: dict[str, Any],
) -> list[str]:
    errors: list[str] = []
    try:
        document = read_json(artifact_dir / layer / "raw-evidence.json")
    except CoverageError as exc:
        return [str(exc)]
    if (
        not isinstance(document, dict)
        or document.get("schema_version") != SCHEMA_VERSION
        or document.get("kind") != "coverage-raw-evidence"
        or document.get("layer_id") != layer
        or document.get("commit") != provenance.get("commit")
        or document.get("scope_sha256") != provenance.get("scope_sha256")
        or document.get("baseline_sha256") != provenance.get("baseline_sha256")
        or document.get("errors") != []
        or document.get("passed") is not True
    ):
        errors.append(f"{layer}: raw evidence manifest is invalid")
    entries = document.get("files") if isinstance(document, dict) else None
    if not isinstance(entries, list) or not entries:
        return errors + [f"{layer}: raw evidence file inventory is empty"]
    seen: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict) or not isinstance(entry.get("path"), str):
            errors.append(f"{layer}: malformed raw evidence entry")
            continue
        relative = entry["path"]
        path = artifact_dir / relative
        try:
            path.resolve().relative_to((artifact_dir / layer).resolve())
        except ValueError:
            errors.append(f"{layer}: raw evidence path escapes its layer: {relative}")
            continue
        if relative in seen:
            errors.append(f"{layer}: duplicate raw evidence path: {relative}")
            continue
        seen.add(relative)
        if (
            not path.is_file()
            or not _is_int(entry.get("size"))
            or entry.get("size") < 0
            or path.stat().st_size != entry.get("size")
            or entry.get("sha256") != sha256(path)
        ):
            errors.append(f"{layer}: raw evidence hash/size mismatch: {relative}")
    required = {
        "rust": {
            "rust/raw-lcov.info",
            "rust/production-map.info",
            "rust/raw-coverage.json",
            "rust/raw-summary.txt",
            "rust/coverage.profdata",
            "rust/production-config.json",
            "rust/toolchain.json",
        },
        "cpp": {
            "cpp/raw-coverage.json",
            "cpp/raw-lcov.info",
            "cpp/raw-summary.txt",
            "cpp/coverage.profdata",
            "cpp/toolchain.json",
            "cpp/ctest.log",
            "cpp/gpu-correctness-evidence.json",
        },
        "sql": {"sql/assertion-inventory.json", "sql/test-run/results.tsv"},
    }[layer]
    if not required.issubset(seen):
        errors.append(f"{layer}: required raw evidence files are absent")
    entry_sizes = {
        entry.get("path"): entry.get("size")
        for entry in entries
        if isinstance(entry, dict)
    }
    if any(
        not _is_int(entry_sizes.get(name)) or entry_sizes[name] <= 0
        for name in required.intersection(seen)
    ):
        errors.append(f"{layer}: required raw evidence files are empty")
    if layer in LINE_LAYERS and not any(
        name.startswith(f"{layer}/profiles/")
        and name.endswith(".profraw")
        and _is_int(entry_sizes.get(name))
        and entry_sizes[name] > 0
        for name in seen
    ):
        errors.append(f"{layer}: raw profraw evidence is absent")
    return errors


def recompute_raw_line_layer(
    layer: str,
    artifact_dir: pathlib.Path,
    repo_root: pathlib.Path,
    scope: dict[str, Any],
    baseline: dict[str, Any],
) -> tuple[dict[str, Any], list[str]]:
    errors: list[str] = []
    layer_scope = scope["layers"][layer]
    included, scope_required = source_inventory(repo_root, layer_scope)
    if layer == "rust":
        expected_files = set(baseline["rust"]["owned_files"])
        required = set(baseline["rust"]["required_mapping_files"])
        if included != expected_files or len(required) <= 1:
            errors.append(
                "rust: owned production scope differs from the release baseline"
            )
        production_config = read_json(artifact_dir / "rust/production-config.json")
        if production_config != {
            "postgres_major": 18,
            "default_features": False,
            "features": ["pg18"],
            "pg_test": False,
        }:
            errors.append(
                "rust: production compiler configuration is not pg18 without pg_test"
            )
        declared = lcov_files(artifact_dir / "rust/production-map.info", repo_root)
        hits = lcov_files(artifact_dir / "rust/raw-lcov.info", repo_root)
        exported = llvm_json_files(
            read_json(artifact_dir / "rust/raw-coverage.json"), repo_root
        )
        if included.intersection(hits) != included.intersection(exported):
            errors.append("rust: raw LCOV and JSON source mappings differ")
        for relative in included.intersection(hits).intersection(exported):
            lines = exported[relative].get("summary", {}).get("lines")
            if (
                not isinstance(lines, dict)
                or lines.get("count") != len(hits[relative])
                or lines.get("covered")
                != sum(1 for value in hits[relative].values() if value > 0)
            ):
                errors.append(f"rust: raw LCOV and JSON totals differ for {relative}")
        mapped = included.intersection(declared)
        missing = required.difference(declared)
        total = 0
        covered = 0
        for relative in mapped:
            source_lines = (repo_root / relative).read_text(
                encoding="utf-8", errors="replace"
            ).count("\n") + 1
            invalid = [line for line in declared[relative] if line > source_lines]
            if invalid:
                errors.append(
                    f"rust: production mapping has invalid lines in {relative}"
                )
            declared_lines = set(declared[relative])
            total += len(declared_lines)
            covered += sum(
                1 for line in declared_lines if hits.get(relative, {}).get(line, 0) > 0
            )
        unexpected = sorted(set(declared).difference(included))
    else:
        pinned_sources = set(baseline["cpp"]["sources"])
        required = scope_required
        if len(pinned_sources) != BASELINE_CPP_SOURCES or not pinned_sources.issubset(
            required
        ):
            errors.append(
                "cpp: owned sources differ from the 21-source release baseline"
            )
        reports = llvm_json_files(
            read_json(artifact_dir / "cpp/raw-coverage.json"), repo_root
        )
        lcov_reports = lcov_files(artifact_dir / "cpp/raw-lcov.info", repo_root)
        if included.intersection(reports) != included.intersection(lcov_reports):
            errors.append("cpp: raw LCOV and JSON source mappings differ")
        mapped = included.intersection(reports)
        missing = required.difference(reports)
        unexpected = sorted(set(reports).difference(included))
        total = 0
        covered = 0
        for relative in mapped:
            lines = reports[relative].get("summary", {}).get("lines")
            if not isinstance(lines, dict):
                errors.append(f"cpp: raw report has no line metrics for {relative}")
                continue
            file_total = lines.get("count")
            file_covered = lines.get("covered")
            if (
                not _is_int(file_total)
                or not _is_int(file_covered)
                or file_total < 0
                or file_covered < 0
                or file_covered > file_total
            ):
                errors.append(f"cpp: invalid raw line metrics for {relative}")
                continue
            if (
                relative not in lcov_reports
                or len(lcov_reports[relative]) != file_total
                or sum(1 for value in lcov_reports[relative].values() if value > 0)
                != file_covered
            ):
                errors.append(f"cpp: raw LCOV and JSON totals differ for {relative}")
            total += file_total
            covered += file_covered
    if missing:
        errors.append(f"{layer}: required raw source mappings are missing")
    if total <= 0:
        errors.append(f"{layer}: raw report has zero production lines")
    return (
        {
            "total": total,
            "covered": covered,
            "uncovered": total - covered,
            "percent": 0.0 if total == 0 else covered * 100.0 / total,
            "owned_files": len(included),
            "required_files": len(required),
            "mapped_files": len(mapped),
            "missing_required_files": sorted(missing),
            "unexpected_owned_report_files": unexpected,
        },
        errors,
    )


def compare_summary_to_raw(summary: Any, layer: str, raw: dict[str, Any]) -> list[str]:
    if not isinstance(summary, dict):
        return [f"{layer}: summary cannot be compared to raw evidence"]
    errors: list[str] = []
    if (
        summary.get("total_units") != raw["total"]
        or summary.get("covered_units") != raw["covered"]
        or summary.get("uncovered_units") != raw["uncovered"]
        or summary.get("percent") != raw["percent"]
    ):
        errors.append(f"{layer}: summary totals differ from recomputed raw evidence")
    mapping = summary.get("mapping")
    for key in (
        "owned_files",
        "required_files",
        "mapped_files",
        "missing_required_files",
        "unexpected_owned_report_files",
    ):
        if not isinstance(mapping, dict) or mapping.get(key) != raw[key]:
            errors.append(f"{layer}: summary mapping differs from raw evidence")
            break
    return errors


def inspect_aggregate_sql_manifest(
    document: Any,
) -> tuple[dict[str, str], dict[str, str], list[str]]:
    """Validate the retained manifest without trusting the SQL layer summary."""

    errors: list[str] = []
    owners: dict[str, str] = {}
    completions: dict[str, str] = {}
    if not isinstance(document, dict):
        return owners, completions, ["copied SQL manifest is not an object"]
    if (
        document.get("schema_version") != SCHEMA_VERSION
        or document.get("kind") != "sql-semantic-assertion-manifest"
        or document.get("test_root") != "sql/tests"
        or document.get("baseline_files") != BASELINE_SQL_FILES
        or document.get("baseline_assertions") != BASELINE_SQL_ASSERTIONS
    ):
        errors.append("copied SQL manifest schema or baseline is invalid")
    entries = document.get("files")
    if not isinstance(entries, list):
        return owners, completions, errors + ["copied SQL manifest files are invalid"]
    for entry in entries:
        if not isinstance(entry, dict):
            errors.append("copied SQL manifest contains a non-object file entry")
            continue
        name = entry.get("file")
        source_hash = entry.get("sha256")
        completion = entry.get("completion_id")
        assertions = entry.get("assertions")
        if (
            not isinstance(name, str)
            or pathlib.PurePosixPath(name).name != name
            or not name.endswith(".sql")
            or not isinstance(source_hash, str)
            or re.fullmatch(r"[0-9a-f]{64}", source_hash) is None
            or completion != pathlib.PurePosixPath(name).stem
            or not isinstance(assertions, list)
            or not assertions
        ):
            errors.append(f"copied SQL manifest file entry is invalid: {name!r}")
            continue
        if name in completions:
            errors.append(f"copied SQL manifest duplicates file {name}")
            continue
        completions[name] = completion
        for assertion in assertions:
            if not isinstance(assertion, dict):
                errors.append(f"copied SQL manifest assertion is invalid in {name}")
                continue
            identifier = assertion.get("id")
            source_line = assertion.get("source_line")
            emission = assertion.get("emission")
            if (
                not isinstance(identifier, str)
                or re.fullmatch(r"[a-z0-9][a-z0-9_.-]*", identifier) is None
                or not _is_int(source_line)
                or source_line <= 0
                or emission not in {"echo", "notice"}
            ):
                errors.append(f"copied SQL manifest assertion is invalid in {name}")
                continue
            if identifier in owners:
                errors.append(f"copied SQL manifest duplicates assertion {identifier}")
            owners[identifier] = name
    if (
        len(completions) < BASELINE_SQL_FILES
        or document.get("declared_files") != len(completions)
        or len(owners) < BASELINE_SQL_ASSERTIONS
        or document.get("declared_assertions") != len(owners)
    ):
        errors.append("copied SQL manifest counts are below baseline or inconsistent")
    return owners, completions, errors


def validate_retained_sql_evidence(
    artifact_dir: pathlib.Path,
    inventory: Any,
    owners: dict[str, str],
    completions: dict[str, str],
) -> list[str]:
    """Recount retained SQL logs rather than trusting inventory arithmetic."""

    if not isinstance(inventory, dict):
        return ["retained SQL inventory is not an object"]
    rows = inventory.get("files")
    successful_ids = inventory.get("successful_assertion_ids")
    if not isinstance(rows, list) or not isinstance(successful_ids, list):
        return ["retained SQL inventory file or assertion evidence is invalid"]
    row_by_name: dict[str, dict[str, Any]] = {}
    errors: list[str] = []
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("file"), str):
            errors.append("retained SQL inventory contains an invalid file row")
            continue
        name = row["file"]
        if name in row_by_name:
            errors.append(f"retained SQL inventory duplicates file row {name}")
            continue
        row_by_name[name] = row
    if set(row_by_name) != set(completions):
        errors.append("retained SQL inventory file set differs from its manifest")

    observed: set[str] = set()
    for name, completion in completions.items():
        row = row_by_name.get(name)
        if row is None:
            continue
        expected_log = f"logs/{name}.log"
        observed_count = row.get("observed_assertions")
        if (
            row.get("status") != "pass"
            or row.get("exit_code") != 0
            or row.get("completion_markers") != 1
            or row.get("log") != expected_log
            or not _is_int(observed_count)
            or observed_count < 0
        ):
            errors.append(f"retained SQL inventory execution row is invalid: {name}")
            continue
        log_path = artifact_dir / "sql/test-run" / expected_log
        if not log_path.is_file():
            errors.append(f"retained SQL log is missing: {name}")
            continue
        if row.get("log_sha256") != sha256(log_path):
            errors.append(f"retained SQL log hash is inconsistent: {name}")
        log_text = log_path.read_text(encoding="utf-8", errors="replace")
        file_ids: set[str] = set()
        completion_count = 0
        for line_number, line in enumerate(log_text.splitlines(), start=1):
            assertion = ASSERTION_LOG_LINE.fullmatch(line)
            file_completion = re.fullmatch(
                r"PGACCEL_FILE_OK:([a-z0-9][a-z0-9_.-]*)", line
            )
            prefix_count = line.count("PGACCEL_ASSERT_OK:") + line.count(
                "PGACCEL_FILE_OK:"
            )
            if prefix_count > 1 or (
                prefix_count == 1 and assertion is None and file_completion is None
            ):
                errors.append(
                    f"retained SQL log has malformed or multi-ID evidence: {name}:{line_number}"
                )
            if SUSPICIOUS_LOG.search(line):
                errors.append(
                    f"retained SQL log has forbidden evidence: {name}:{line_number}"
                )
            if assertion:
                identifier = assertion.group(1)
                if owners.get(identifier) != name:
                    errors.append(
                        f"retained SQL log has unknown or wrong-file ID: {name}:{identifier}"
                    )
                elif identifier in file_ids or identifier in observed:
                    errors.append(
                        f"retained SQL log duplicates assertion ID: {identifier}"
                    )
                else:
                    file_ids.add(identifier)
                    observed.add(identifier)
            if file_completion:
                completion_count += 1
                if file_completion.group(1) != completion:
                    errors.append(f"retained SQL log has wrong completion ID: {name}")
        if completion_count != 1:
            errors.append(f"retained SQL log completion count is invalid: {name}")
        if len(file_ids) != observed_count:
            errors.append(f"retained SQL inventory assertion count is invalid: {name}")

    if not all(isinstance(identifier, str) for identifier in successful_ids):
        errors.append("retained SQL successful assertion IDs are invalid")
    elif successful_ids != sorted(observed):
        errors.append("retained SQL successful assertion IDs differ from the logs")
    if not observed.issubset(owners):
        errors.append("retained SQL successful assertion IDs differ from the manifest")
    return errors


def validate_retained_toolchain_and_profiles(
    artifact_dir: pathlib.Path, layer: str
) -> list[str]:
    errors: list[str] = []
    toolchain_path = artifact_dir / layer / "toolchain.json"
    try:
        document = read_json(toolchain_path)
    except CoverageError as exc:
        return [f"{layer}: {exc}"]
    expected_kind = (
        "rust-llvm-toolchain-evidence" if layer == "rust" else "llvm-toolchain-evidence"
    )
    expected_names = (
        {"rustc", "llvm_cov", "llvm_profdata"}
        if layer == "rust"
        else {"clang", "llvm_cov", "llvm_profdata"}
    )
    tools = document.get("tools") if isinstance(document, dict) else None
    if (
        not isinstance(document, dict)
        or document.get("schema_version") != SCHEMA_VERSION
        or document.get("kind") != expected_kind
        or document.get("passed") is not True
        or document.get("errors") != []
        or not isinstance(tools, dict)
        or set(tools) != expected_names
        or not all(isinstance(entry, dict) for entry in tools.values())
    ):
        return [f"{layer}: LLVM toolchain raw evidence is invalid"]

    observed_majors: list[int] = []
    for name, entry in tools.items():
        executable = entry.get("path")
        recorded_major = entry.get("major")
        if not isinstance(executable, str) or not _is_int(recorded_major):
            errors.append(f"{layer}: LLVM toolchain entry is malformed: {name}")
            continue
        version_args = ["-vV"] if name == "rustc" else ["--version"]
        try:
            completed = subprocess.run(
                [executable, *version_args],
                check=False,
                text=True,
                capture_output=True,
            )
            actual_major = extract_llvm_major(completed.stdout + completed.stderr)
            if completed.returncode != 0 or actual_major != recorded_major:
                errors.append(f"{layer}: retained LLVM tool version drifted: {name}")
            observed_majors.append(actual_major)
        except (OSError, CoverageError) as exc:
            errors.append(f"{layer}: cannot revalidate {name}: {exc}")
    if len(observed_majors) != 3 or len(set(observed_majors)) != 1:
        errors.append(f"{layer}: retained LLVM tool majors do not match")

    profdata_entry = tools.get("llvm_profdata", {})
    llvm_profdata = profdata_entry.get("path")
    profile_paths = [artifact_dir / layer / "coverage.profdata"]
    profile_paths.extend(sorted((artifact_dir / layer / "profiles").glob("*.profraw")))
    if not isinstance(llvm_profdata, str) or len(profile_paths) < 2:
        errors.append(f"{layer}: retained LLVM profiles are incomplete")
    else:
        for profile in profile_paths:
            try:
                completed = subprocess.run(
                    [llvm_profdata, "show", str(profile)],
                    check=False,
                    text=True,
                    capture_output=True,
                )
                if completed.returncode != 0:
                    errors.append(
                        f"{layer}: retained LLVM profile is malformed: {profile.name}"
                    )
            except OSError as exc:
                errors.append(f"{layer}: cannot parse retained LLVM profile: {exc}")
    return errors


def aggregate(args: argparse.Namespace) -> int:
    artifact_dir = pathlib.Path(args.artifact_dir)
    repo_root = pathlib.Path(getattr(args, "repo_root", ".")).resolve()
    errors: list[str] = []
    layers: dict[str, Any] = {}
    found_ids: dict[str, str] = {}
    inventory: Any = None
    provenance, provenance_errors = validate_aggregate_provenance(
        artifact_dir, repo_root
    )
    errors.extend(provenance_errors)
    try:
        copied_scope = read_json(artifact_dir / "scope.json")
        copied_baseline = read_json(artifact_dir / "release-baseline.json")
        if (
            not isinstance(copied_scope, dict)
            or copied_scope.get("schema_version") != SCHEMA_VERSION
            or not isinstance(copied_baseline, dict)
            or copied_baseline.get("schema_version") != SCHEMA_VERSION
            or copied_baseline.get("kind") != "coverage-release-baseline"
        ):
            errors.append("copied coverage scope or release baseline is invalid")
    except CoverageError as exc:
        copied_scope = {"layers": {}}
        copied_baseline = {}
        errors.append(str(exc))
    summary_paths = sorted(artifact_dir.glob("*/layer-summary.json"))
    for path in summary_paths:
        directory_id = path.parent.name
        try:
            summary = read_json(path)
        except CoverageError as exc:
            errors.append(str(exc))
            continue
        layer_id = summary.get("layer_id") if isinstance(summary, dict) else None
        if directory_id not in EXPECTED_LAYERS:
            errors.append(f"unknown layer summary directory: {directory_id}")
        if layer_id not in EXPECTED_LAYERS:
            errors.append(f"unknown layer ID in {path}: {layer_id!r}")
            continue
        if layer_id in found_ids:
            errors.append(
                f"duplicate layer ID {layer_id}: {found_ids[layer_id]} and {path}"
            )
            continue
        found_ids[layer_id] = str(path)
        layers[layer_id] = summary
        if directory_id != layer_id:
            errors.append(f"layer directory/ID mismatch: {directory_id}/{layer_id}")
    for layer in EXPECTED_LAYERS:
        if layer not in layers:
            errors.append(f"missing {layer} layer summary")
            continue
        errors.extend(validate_layer_summary(layers[layer], layer))
        errors.extend(validate_raw_evidence_manifest(artifact_dir, layer, provenance))
        if layer in LINE_LAYERS:
            errors.extend(validate_retained_toolchain_and_profiles(artifact_dir, layer))
        stage_path = artifact_dir / layer / "stage-status.json"
        try:
            errors.extend(validate_stage_status(read_json(stage_path), layer))
        except CoverageError as exc:
            errors.append(str(exc))
        if layer in LINE_LAYERS:
            try:
                raw, raw_errors = recompute_raw_line_layer(
                    layer,
                    artifact_dir,
                    repo_root,
                    copied_scope,
                    copied_baseline,
                )
                errors.extend(raw_errors)
                errors.extend(compare_summary_to_raw(layers[layer], layer, raw))
            except (CoverageError, KeyError, TypeError, ValueError) as exc:
                errors.append(f"{layer}: raw evidence recomputation failed: {exc}")

    inventory_path = artifact_dir / "sql/assertion-inventory.json"
    try:
        inventory = read_json(inventory_path)
        sql = layers.get("sql", {})
        sql_manifest = sql.get("manifest")
        if not isinstance(sql_manifest, dict):
            sql_manifest = {}
        if (
            not isinstance(inventory, dict)
            or inventory.get("schema_version") != SCHEMA_VERSION
            or inventory.get("kind") != "sql-semantic-assertion-inventory"
        ):
            errors.append("SQL assertion inventory schema is invalid")
        elif (
            inventory.get("declared_assertions") != sql.get("assertion_count")
            or inventory.get("successful_assertions") != sql.get("covered_assertions")
            or inventory.get("assertion_percent") != sql.get("assertion_percent")
            or inventory.get("manifest_sha256") != sql_manifest.get("sha256")
            or inventory.get("declared_files") != sql_manifest.get("declared_files")
            or inventory.get("passed_files") != sql_manifest.get("passed_test_files")
            or inventory.get("completed_files") != sql_manifest.get("completed_files")
            or inventory.get("complete") is not True
            or inventory.get("errors") != []
        ):
            errors.append("SQL assertion inventory is inconsistent or incomplete")
        else:
            successful_ids = inventory.get("successful_assertion_ids")
            if not isinstance(successful_ids, list):
                errors.append("SQL assertion inventory ID arithmetic is impossible")
            elif (
                len(successful_ids) != inventory.get("successful_assertions")
                or not all(isinstance(value, str) for value in successful_ids)
                or len(set(successful_ids)) != len(successful_ids)
            ):
                errors.append("SQL assertion inventory ID arithmetic is impossible")
    except CoverageError as exc:
        errors.append(str(exc))

    copied_manifest = artifact_dir / "sql-semantic-assertions.json"
    sql_summary_manifest = layers.get("sql", {}).get("manifest", {})
    if not copied_manifest.is_file():
        errors.append("copied SQL semantic assertion manifest is missing")
    else:
        try:
            current_manifest = repo_root / "coverage/sql-semantic-assertions.json"
            if not current_manifest.is_file() or sha256(copied_manifest) != sha256(
                current_manifest
            ):
                errors.append(
                    "copied SQL semantic assertion manifest drifted from the checkout"
                )
            manifest_document = read_json(copied_manifest)
            owners, completions, manifest_errors = inspect_aggregate_sql_manifest(
                manifest_document
            )
            errors.extend(manifest_errors)
            sql_baseline = copied_baseline.get("sql", {})
            if (
                not isinstance(sql_baseline, dict)
                or sorted(completions) != sql_baseline.get("files")
                or sorted(owners) != sql_baseline.get("assertion_ids")
            ):
                errors.append(
                    "retained SQL manifest differs from the immutable baseline"
                )
            if (
                not isinstance(sql_summary_manifest, dict)
                or sql_summary_manifest.get("sha256") != sha256(copied_manifest)
                or sql_summary_manifest.get("declared_files") != len(completions)
                or sql_summary_manifest.get("declared_assertions") != len(owners)
            ):
                errors.append(
                    "copied SQL semantic assertion manifest is inconsistent with the summary"
                )
            errors.extend(
                validate_retained_sql_evidence(
                    artifact_dir, inventory, owners, completions
                )
            )
        except CoverageError as exc:
            errors.append(str(exc))

    gpu_path = artifact_dir / "cpp/gpu-correctness-evidence.json"
    try:
        gpu = read_json(gpu_path)
        ctest_log = artifact_dir / "cpp/ctest.log"
        recomputed_gpu = inspect_gpu_evidence(
            status=0,
            log=ctest_log,
            per_test_dir=artifact_dir / "cpp/per-test-logs",
            baseline=copied_baseline,
        )
        if (
            not isinstance(gpu, dict)
            or recomputed_gpu.get("passed") is not True
            or {key: value for key, value in gpu.items() if key != "generated_at_utc"}
            != {
                key: value
                for key, value in recomputed_gpu.items()
                if key != "generated_at_utc"
            }
        ):
            errors.append("C++ GPU correctness/OOM evidence is invalid or incomplete")
    except CoverageError as exc:
        errors.append(str(exc))

    passed = not errors and len(layers) == len(EXPECTED_LAYERS)
    result = {
        "schema_version": SCHEMA_VERSION,
        "kind": "coverage-gate-summary",
        "generated_at_utc": utc_now(),
        "gate": "pg_accel three-layer release coverage",
        "passed": passed,
        "layers": layers,
        "errors": errors,
    }
    write_json(artifact_dir / "gate-summary.json", result)
    lines = ["# pg_accel three-layer coverage", ""]
    for layer in EXPECTED_LAYERS:
        summary = layers.get(layer)
        if not isinstance(summary, dict):
            lines.append(f"- {layer}: MISSING")
            continue
        unit = "assertions" if layer == "sql" else "lines"
        percent = summary.get("percent")
        threshold = summary.get("threshold_percent")
        display_percent = (
            float(percent)
            if isinstance(percent, (int, float)) and math.isfinite(float(percent))
            else 0.0
        )
        display_threshold = (
            float(threshold)
            if isinstance(threshold, (int, float)) and math.isfinite(float(threshold))
            else 0.0
        )
        lines.append(
            f"- {layer}: {'PASS' if not validate_layer_summary(summary, layer) else 'FAIL'} - "
            f"{summary.get('covered_units', 0)}/{summary.get('total_units', 0)} {unit} "
            f"({display_percent:.2f}%, required {display_threshold:.2f}%)"
        )
    if errors:
        lines.extend(("", "## Errors", ""))
        lines.extend(f"- {error}" for error in errors)
    (artifact_dir / "gate-summary.md").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )
    print("\n".join(lines))
    return 0 if passed else 1


def extract_llvm_major(output: str) -> int:
    match = re.search(
        r"(?:Apple\s+)?(?:clang|LLVM)\s+version(?:\s*:\s*|\s+)([0-9]+)",
        output,
        flags=re.IGNORECASE,
    )
    if match is None:
        raise CoverageError(
            f"cannot parse LLVM major from version output: {output.splitlines()[:2]}"
        )
    return int(match.group(1))


def validate_toolchain(args: argparse.Namespace) -> int:
    tools = {
        "clang": args.clang,
        "llvm_cov": args.llvm_cov,
        "llvm_profdata": args.llvm_profdata,
    }
    versions: dict[str, Any] = {}
    errors: list[str] = []
    for name, executable in tools.items():
        try:
            completed = subprocess.run(
                [executable, "--version"], check=False, text=True, capture_output=True
            )
            output = completed.stdout + completed.stderr
            major = extract_llvm_major(output)
            if completed.returncode != 0:
                errors.append(f"{name} --version exited {completed.returncode}")
            versions[name] = {
                "path": executable,
                "major": major,
                "output": output.splitlines()[:4],
            }
        except (OSError, CoverageError) as exc:
            errors.append(f"{name}: {exc}")
    majors = {entry["major"] for entry in versions.values() if "major" in entry}
    if len(versions) != 3 or len(majors) != 1:
        errors.append("clang, llvm-cov, and llvm-profdata major versions do not match")
    document = {
        "schema_version": SCHEMA_VERSION,
        "kind": "llvm-toolchain-evidence",
        "generated_at_utc": utc_now(),
        "tools": versions,
        "errors": errors,
        "passed": not errors,
    }
    write_json(pathlib.Path(args.output), document)
    return 0 if not errors else 1


def validate_rust_toolchain(args: argparse.Namespace) -> int:
    tools = {
        "rustc": (args.rustc, ["-vV"]),
        "llvm_cov": (args.llvm_cov, ["--version"]),
        "llvm_profdata": (args.llvm_profdata, ["--version"]),
    }
    versions: dict[str, Any] = {}
    errors: list[str] = []
    for name, (executable, version_args) in tools.items():
        try:
            completed = subprocess.run(
                [executable, *version_args],
                check=False,
                text=True,
                capture_output=True,
            )
            output = completed.stdout + completed.stderr
            major = extract_llvm_major(output)
            if completed.returncode != 0:
                errors.append(f"{name} version command exited {completed.returncode}")
            versions[name] = {
                "path": executable,
                "major": major,
                "output": output.splitlines()[:12],
            }
        except (OSError, CoverageError) as exc:
            errors.append(f"{name}: {exc}")
    majors = {entry["major"] for entry in versions.values() if "major" in entry}
    if len(versions) != 3 or len(majors) != 1:
        errors.append("rustc, llvm-cov, and llvm-profdata major versions do not match")
    document = {
        "schema_version": SCHEMA_VERSION,
        "kind": "rust-llvm-toolchain-evidence",
        "generated_at_utc": utc_now(),
        "tools": versions,
        "errors": errors,
        "passed": not errors,
    }
    write_json(pathlib.Path(args.output), document)
    return 0 if not errors else 1


def inspect_gpu_evidence(
    *, status: int, log: pathlib.Path, per_test_dir: pathlib.Path, baseline: Any
) -> dict[str, Any]:
    log_text = (
        log.read_text(encoding="utf-8", errors="replace") if log.is_file() else ""
    )
    cpp_baseline = baseline.get("cpp") if isinstance(baseline, dict) else None
    pinned_tests = (
        cpp_baseline.get("ctest_names") if isinstance(cpp_baseline, dict) else None
    )
    pinned_families = (
        cpp_baseline.get("oom_families") if isinstance(cpp_baseline, dict) else None
    )
    errors: list[str] = []
    if not isinstance(pinned_tests, list) or len(pinned_tests) != BASELINE_CPP_TESTS:
        pinned_tests = []
        errors.append("pinned CTest inventory is invalid")
    if not isinstance(pinned_families, list) or len(pinned_families) != 5:
        pinned_families = []
        errors.append("pinned OOM family inventory is invalid")
    passed_tests = re.findall(
        r"Test\s+#\d+:\s+([A-Za-z0-9_.-]+)\s+\.+\s+Passed\b", log_text
    )
    if sorted(passed_tests) != sorted(pinned_tests) or len(passed_tests) != len(
        set(passed_tests)
    ):
        errors.append("full pinned CTest inventory did not pass exactly once")

    raw_logs: dict[str, dict[str, Any]] = {}
    for test_name in pinned_tests:
        matches = sorted(per_test_dir.glob(f"{test_name}-*.log"))
        if len(matches) != 1:
            errors.append(f"expected one retained raw log for {test_name}")
            continue
        raw_logs[test_name] = {
            "path": matches[0].name,
            "sha256": sha256(matches[0]),
            "size": matches[0].stat().st_size,
        }
    oom_matches = sorted(per_test_dir.glob("test_oom_invariant-*.log"))
    oom_text = (
        oom_matches[0].read_text(encoding="utf-8", errors="replace")
        if len(oom_matches) == 1
        else ""
    )
    if (
        "PGACCEL_UNSUPPORTED" in oom_text
        or "unsupported before GPU dispatch" in oom_text
    ):
        errors.append("OOM invariant accepted an unsupported path")
    device_match = re.search(
        r'PGACCEL_DEVICE_PROOF device="([^"]+)" backend="([^"]+)" '
        r"compute_units=([0-9]+) max_alloc_bytes=([0-9]+) real_device=1",
        oom_text,
    )
    device: dict[str, Any] | None = None
    if device_match:
        device = {
            "name": device_match.group(1),
            "backend": device_match.group(2),
            "compute_units": int(device_match.group(3)),
            "max_alloc_bytes": int(device_match.group(4)),
        }
        backend = device["backend"].lower()
        if (
            device["compute_units"] <= 0
            or device["max_alloc_bytes"] <= 0
            or not any(
                name in backend for name in ("metal", "cuda", "hip", "level_zero")
            )
        ):
            errors.append("OOM proof did not identify a real accelerator")
    else:
        errors.append("OOM proof has no real-device record")

    families: dict[str, dict[str, Any]] = {}
    family_pattern = re.compile(
        r"PGACCEL_OOM_FAMILY family=([A-Za-z0-9_]+) result=(PASS|FAIL) "
        r"dispatches=([0-9]+) peak_rss_bytes=([0-9]+) "
        r"rss_delta_bytes=([0-9]+) rss_limit_bytes=([0-9]+)"
    )
    for match in family_pattern.finditer(oom_text):
        name = match.group(1)
        if name in families:
            errors.append(f"OOM proof duplicates family {name}")
            continue
        families[name] = {
            "result": match.group(2),
            "dispatches": int(match.group(3)),
            "peak_rss_bytes": int(match.group(4)),
            "rss_delta_bytes": int(match.group(5)),
            "rss_limit_bytes": int(match.group(6)),
        }
    if set(families) != set(pinned_families):
        errors.append("OOM proof family set differs from the pinned inventory")
    for name, family in families.items():
        if (
            family["result"] != "PASS"
            or family["dispatches"] <= 0
            or family["peak_rss_bytes"] <= 0
            or family["rss_limit_bytes"] <= 0
            or family["rss_delta_bytes"] > family["rss_limit_bytes"]
        ):
            errors.append(f"OOM proof is invalid for family {name}")
    invariant_passed = (
        re.search(
            r"PGACCEL_OOM_INVARIANT result=PASS families=5 "
            r"max_alloc_bytes=[1-9][0-9]* input_doubles=[1-9][0-9]* "
            r"rss_limit_bytes=[1-9][0-9]*",
            oom_text,
        )
        is not None
    )
    if not invariant_passed:
        errors.append("exact OOM invariant completion proof is absent")
    if status != 0:
        errors.append(f"CTest execution exited {status}")
    passed = not errors
    document = {
        "schema_version": SCHEMA_VERSION,
        "kind": "gpu-correctness-evidence",
        "generated_at_utc": utc_now(),
        "status": "complete" if passed else "failed",
        "execution_status": status,
        "ctest_log": log.name,
        "ctest_log_sha256": sha256(log) if log.is_file() else None,
        "pinned_ctest_count": len(pinned_tests),
        "passed_ctest_names": sorted(passed_tests),
        "raw_test_logs": raw_logs,
        "device": device,
        "families": families,
        "oom_invariant_required": True,
        "oom_invariant_observed": bool(oom_text),
        "oom_invariant_passed": invariant_passed,
        "errors": errors,
        "passed": passed,
    }
    return document


def gpu_evidence(args: argparse.Namespace) -> int:
    document = inspect_gpu_evidence(
        status=int(args.execution_status),
        log=pathlib.Path(args.ctest_log),
        per_test_dir=pathlib.Path(args.per_test_log_dir),
        baseline=read_json(pathlib.Path(args.baseline)),
    )
    write_json(pathlib.Path(args.output), document)
    return 0 if document["passed"] else 1


def audit_scope(args: argparse.Namespace) -> int:
    repo_root = pathlib.Path(args.repo_root).resolve()
    document = read_json(pathlib.Path(args.scope))
    baseline_path = pathlib.Path(args.baseline)
    baseline = read_json(baseline_path)
    if document.get("schema_version") != SCHEMA_VERSION:
        raise CoverageError("coverage scope schema version mismatch")
    minimum = validate_threshold(float(document.get("minimum_percent", 0)))
    if minimum != FIXED_THRESHOLD:
        raise CoverageError(
            f"checked-in minimum must remain exactly {FIXED_THRESHOLD:g}%"
        )
    for layer in (*LINE_LAYERS, "sql_reachability"):
        try:
            scope = document["layers"][layer]
        except (KeyError, TypeError) as exc:
            raise CoverageError(f"scope file is missing layer {layer}") from exc
        _, required = source_inventory(repo_root, scope)
        if not required:
            raise CoverageError(f"scope layer {layer} has no required source files")
    rust_scope = document["layers"]["rust"]
    rust_roots = rust_scope["roots"]
    if (
        "pg_accel/build.rs" not in rust_roots
        or rust_scope.get("production_mapping")
        != "compiler-derived-pg18-without-pg_test"
    ):
        raise CoverageError(
            "Rust scope must include build.rs and compiler-derived pg18 production mapping"
        )
    coverage_gate = (repo_root / "scripts/coverage_gate.sh").read_text(encoding="utf-8")
    if coverage_gate.count("--include-build-script") != 6:
        raise CoverageError(
            "Rust coverage gate must instrument and report pg_accel/build.rs explicitly"
        )
    for required_text in (
        "cargo build --workspace --locked --no-default-features",
        '--features "pg${pg}"',
        '"pg_test":false',
        '--production-map "$output_dir/production-map.info"',
        "record_stage rust supplemental_tests",
        "capture-provenance",
        "validate-rust-toolchain",
        "cargo llvm-cov clean --workspace",
        'copy_profiles "$build_dir" "$profile_dir"',
        'cmake -E remove_directory "$build_dir"',
        '--per-test-log-dir "$per_test_log_dir"',
        '--baseline "$baseline_file"',
        'aggregate --artifact-dir "$artifact_dir"',
    ):
        if required_text not in coverage_gate:
            raise CoverageError(
                f"Rust compiler-derived production coverage invariant is absent: {required_text}"
            )
    if coverage_gate.count("seal-layer-evidence") != len(EXPECTED_LAYERS):
        raise CoverageError("every coverage layer must seal retained raw evidence")

    cmake_path = repo_root / "pgaccel-kernels/CMakeLists.txt"
    cmake = cmake_path.read_text(encoding="utf-8")
    match = re.search(r"set\(KERNEL_SOURCES\s+(.*?)\n\)", cmake, flags=re.DOTALL)
    if match is None:
        raise CoverageError(
            "cannot find KERNEL_SOURCES in pgaccel-kernels/CMakeLists.txt"
        )
    declared = {
        token for token in re.findall(r"src/[A-Za-z0-9_./-]+\.cpp", match.group(1))
    }
    actual = {
        path.relative_to(repo_root / "pgaccel-kernels").as_posix()
        for path in (repo_root / "pgaccel-kernels/src").glob("*.cpp")
    }
    if declared != actual:
        raise CoverageError(
            f"KERNEL_SOURCES drift: missing={sorted(actual - declared)}, stale={sorted(declared - actual)}"
        )
    _, cpp_required = source_inventory(repo_root, document["layers"]["cpp"])
    pinned_cpp_sources = {f"pgaccel-kernels/{relative}" for relative in actual}
    if not pinned_cpp_sources.issubset(cpp_required):
        raise CoverageError(
            "C++ scope must require all 21 owned implementation sources; headers alone are insufficient"
        )
    for required_text in (
        "PGACCEL_ENABLE_COVERAGE",
        "-fprofile-instr-generate",
        "-fcoverage-mapping",
        "add_pgaccel_gpu_test(test_oom_invariant TIMEOUT 900)",
    ):
        if required_text not in cmake:
            raise CoverageError(f"CMake release invariant is absent: {required_text}")

    oom_source = (repo_root / "pgaccel-kernels/test/test_oom_invariant.cpp").read_text(
        encoding="utf-8"
    )
    for required_text in (
        "const size_t N = (2 * max_alloc) / sizeof(double);",
        "std::min<size_t>(N, size_t{256} * 1024 * 1024)",
        "const size_t rss_ceiling = 3 * max_alloc;",
        "run_reduce_family(N_capped, rss_ceiling)",
        "run_sort_family(N_capped / 2, rss_ceiling)",
        "run_hashagg_family(N_capped / 2, rss_ceiling)",
        "run_spatial_family(N_capped / 2, rss_ceiling)",
        "run_h3_family(N_capped / 2, rss_ceiling)",
        "PGACCEL_DEVICE_PROOF",
        "PGACCEL_OOM_FAMILY",
        "r.gpu_dispatches > 0",
    ):
        if required_text not in oom_source:
            raise CoverageError(
                f"OOM-never release invariant was reduced or removed: {required_text}"
            )
    if "status == PGACCEL_UNSUPPORTED" in oom_source:
        raise CoverageError(
            "OOM invariant must not accept unsupported/zero-dispatch paths"
        )

    manifest_path = repo_root / document.get("sql_manifest", "")
    _, declarations, manifest_errors = validate_sql_manifest(
        manifest_path, repo_root / "sql/tests"
    )
    if manifest_errors:
        raise CoverageError("; ".join(manifest_errors))
    if len(declarations) < BASELINE_SQL_ASSERTIONS:
        raise CoverageError("SQL assertion baseline was reduced")
    sql_files = sorted((repo_root / "sql/tests").glob("[0-9]*.sql"))
    if len(sql_files) < BASELINE_SQL_FILES:
        raise CoverageError("SQL file baseline was reduced")
    expected_baseline = release_baseline_document(
        repo_root, pathlib.Path(args.scope), manifest_path
    )
    if baseline != expected_baseline:
        raise CoverageError(
            "checked-in release baseline drifted; use the explicit review-visible update command"
        )
    if (
        len(baseline["rust"]["owned_files"]) <= 1
        or baseline["rust"]["required_mapping_files"] != baseline["rust"]["owned_files"]
        or len(baseline["cpp"]["sources"]) != BASELINE_CPP_SOURCES
        or len(baseline["cpp"]["ctest_names"]) != BASELINE_CPP_TESTS
    ):
        raise CoverageError("release baseline was weakened")
    print(
        "coverage scope audit: PASS "
        f"({len(baseline['rust']['owned_files'])} Rust files, "
        f"{len(actual)} C++ sources, {len(baseline['cpp']['ctest_names'])} CTests, "
        f"{len(sql_files)} SQL files, "
        f"{len(declarations)} SQL semantic assertions, threshold {minimum:g}%)"
    )
    return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)

    thresholds = commands.add_parser("validate-thresholds")
    thresholds.add_argument("values", nargs="+")
    thresholds.set_defaults(func=validate_thresholds)

    initialize = commands.add_parser("init-artifacts")
    initialize.add_argument("--artifact-dir", required=True)
    initialize.add_argument("--rust-threshold", required=True)
    initialize.add_argument("--cpp-threshold", required=True)
    initialize.add_argument("--sql-threshold", required=True)
    initialize.set_defaults(func=init_artifacts)

    mark = commands.add_parser("mark-layer-error")
    mark.add_argument("--artifact-dir", required=True)
    mark.add_argument("--layer", choices=EXPECTED_LAYERS, required=True)
    mark.add_argument("--stage", required=True)
    mark.add_argument("--message", required=True)
    mark.add_argument("--exit-code", type=int, default=1)
    mark.add_argument("--threshold", type=float, required=True)
    mark.set_defaults(func=mark_layer_error)

    stage = commands.add_parser("record-stage")
    stage.add_argument("--artifact-dir", required=True)
    stage.add_argument("--layer", choices=EXPECTED_LAYERS, required=True)
    stage.add_argument("--stage", required=True)
    stage.add_argument("--exit-code", type=int, required=True)
    stage.add_argument("--message", default="stage failed")
    stage.set_defaults(func=record_stage)

    provenance = commands.add_parser("capture-provenance")
    provenance.add_argument("--repo-root", default=".")
    provenance.add_argument("--scope", required=True)
    provenance.add_argument("--baseline", required=True)
    provenance.add_argument("--output", required=True)
    provenance.set_defaults(func=capture_provenance)

    seal = commands.add_parser("seal-layer-evidence")
    seal.add_argument("--artifact-dir", required=True)
    seal.add_argument("--layer", choices=EXPECTED_LAYERS, required=True)
    seal.set_defaults(func=seal_layer_evidence)

    summarize = commands.add_parser("summarize")
    summarize.add_argument("--layer", choices=LINE_LAYERS, required=True)
    summarize.add_argument("--input", required=True)
    summarize.add_argument("--production-map")
    summarize.add_argument("--format", choices=("json", "lcov"), required=True)
    summarize.add_argument("--scope", required=True)
    summarize.add_argument("--repo-root", default=".")
    summarize.add_argument("--threshold", type=float, required=True)
    summarize.add_argument("--execution-status", type=int, default=0)
    summarize.add_argument("--output-dir", required=True)
    summarize.add_argument("--artifact-dir")
    summarize.set_defaults(func=summarize_layer)

    reachability = commands.add_parser("summarize-reachability")
    reachability.add_argument("--input", required=True)
    reachability.add_argument("--scope", required=True)
    reachability.add_argument("--repo-root", default=".")
    reachability.add_argument("--output", required=True)
    reachability.set_defaults(func=summarize_reachability)

    build_manifest = commands.add_parser("build-sql-manifest")
    build_manifest.add_argument("--tests-dir", required=True)
    build_manifest.add_argument("--output", required=True)
    build_manifest.add_argument("--baseline", default="coverage/release-baseline.json")
    build_manifest.set_defaults(func=build_sql_manifest)

    baseline = commands.add_parser("update-release-baseline")
    baseline.add_argument("--repo-root", default=".")
    baseline.add_argument("--scope", default="coverage/scope.json")
    baseline.add_argument("--manifest", default="coverage/sql-semantic-assertions.json")
    baseline.add_argument("--output", default="coverage/release-baseline.json")
    baseline.add_argument("--acknowledge-review-visible-update", required=True)
    baseline.set_defaults(func=update_release_baseline)

    inventory = commands.add_parser("sql-inventory")
    inventory.add_argument("--tests-dir", required=True)
    inventory.add_argument("--manifest", required=True)
    inventory.add_argument("--results", required=True)
    inventory.add_argument("--output-dir", required=True)
    inventory.add_argument("--threshold", type=float, required=True)
    inventory.add_argument("--execution-status", type=int, default=0)
    inventory.add_argument("--artifact-dir")
    inventory.set_defaults(func=sql_inventory)

    aggregate_parser = commands.add_parser("aggregate")
    aggregate_parser.add_argument("--artifact-dir", required=True)
    aggregate_parser.add_argument("--repo-root", default=".")
    aggregate_parser.set_defaults(func=aggregate)

    toolchain = commands.add_parser("validate-toolchain")
    toolchain.add_argument("--clang", required=True)
    toolchain.add_argument("--llvm-cov", required=True)
    toolchain.add_argument("--llvm-profdata", required=True)
    toolchain.add_argument("--output", required=True)
    toolchain.set_defaults(func=validate_toolchain)

    rust_toolchain = commands.add_parser("validate-rust-toolchain")
    rust_toolchain.add_argument("--rustc", required=True)
    rust_toolchain.add_argument("--llvm-cov", required=True)
    rust_toolchain.add_argument("--llvm-profdata", required=True)
    rust_toolchain.add_argument("--output", required=True)
    rust_toolchain.set_defaults(func=validate_rust_toolchain)

    gpu = commands.add_parser("gpu-evidence")
    gpu.add_argument("--execution-status", type=int, required=True)
    gpu.add_argument("--ctest-log", required=True)
    gpu.add_argument("--per-test-log-dir", required=True)
    gpu.add_argument("--baseline", required=True)
    gpu.add_argument("--output", required=True)
    gpu.set_defaults(func=gpu_evidence)

    audit = commands.add_parser("audit-scope")
    audit.add_argument("--scope", required=True)
    audit.add_argument("--baseline", default="coverage/release-baseline.json")
    audit.add_argument("--repo-root", default=".")
    audit.set_defaults(func=audit_scope)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        return int(args.func(args))
    except CoverageError as exc:
        print(f"coverage error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
