#!/usr/bin/env python3
"""Structured helpers for the three-layer release coverage gate."""

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
import sys
from typing import Any


PASS_PREFIX = re.compile(r"^PASS:")
PASS_SUFFIX = re.compile(r"^[A-Za-z0-9_]+ PASSED$")
PASS_BANNER = re.compile(r"^=== .* PASSED ===$")
ECHO_LINE = re.compile(r"^\s*\\echo\s+(.+?)\s*$")
RUST_FUNCTION = re.compile(
    r"^\s*(?:(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?"
    r"(?:unsafe\s+)?(?:extern\s+(?:\"[^\"]+\"\s+)?)?)fn\s+[A-Za-z_][A-Za-z0-9_]*\s*(?:<[^;{]*>)?\s*\(",
    flags=re.MULTILINE,
)
CPP_FUNCTION_BODY = re.compile(
    r"\)\s*(?:const\s*)?(?:noexcept\s*)?(?:->[^\{]+)?\{", flags=re.DOTALL
)


class CoverageError(RuntimeError):
    """Coverage input is incomplete or internally inconsistent."""


def read_json(path: pathlib.Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise CoverageError(f"cannot read JSON {path}: {exc}") from exc


def write_json(path: pathlib.Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


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
        if not root.is_dir():
            raise CoverageError(f"coverage scope root does not exist: {root_name}")
        for path in root.rglob("*"):
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


def llvm_files(document: Any, repo_root: pathlib.Path) -> dict[str, dict[str, Any]]:
    if not isinstance(document, dict) or not isinstance(document.get("data"), list):
        raise CoverageError("LLVM coverage JSON has no data array")
    files: dict[str, dict[str, Any]] = {}
    for dataset in document["data"]:
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
            files[relative] = summary
    return files


def validate_threshold(value: float) -> float:
    if not math.isfinite(value):
        raise CoverageError(f"release coverage threshold must be finite; got {value}")
    if value < 90.0:
        raise CoverageError(
            f"release coverage threshold cannot be lowered below 90%; got {value:g}%"
        )
    if value > 100.0:
        raise CoverageError(f"coverage threshold cannot exceed 100%; got {value:g}%")
    return value


def validate_thresholds(args: argparse.Namespace) -> int:
    values = [validate_threshold(float(value)) for value in args.values]
    print(
        "coverage thresholds: PASS ("
        + ", ".join(f"{value:g}%" for value in values)
        + ")"
    )
    return 0


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
    reports = llvm_files(read_json(pathlib.Path(args.input)), repo_root)

    mapped = sorted(included.intersection(reports))
    missing_required = sorted(required.difference(reports))
    unexpected_owned = sorted(set(reports).difference(included))
    count = 0
    covered = 0
    rows: list[dict[str, Any]] = []
    for path in mapped:
        lines = reports[path].get("lines")
        if not isinstance(lines, dict):
            raise CoverageError(f"LLVM summary for {path} has no line metrics")
        file_count = int(lines.get("count", 0))
        file_covered = int(lines.get("covered", 0))
        if file_count < 0 or file_covered < 0 or file_covered > file_count:
            raise CoverageError(f"invalid line metrics for {path}")
        count += file_count
        covered += file_covered
        rows.append(
            {
                "file": path,
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
    passed = percent >= threshold and not missing_required and execution_status == 0
    output_dir = pathlib.Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    result = {
        "schema_version": 1,
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "layer": args.layer,
        "description": layer_scope["description"],
        "threshold_percent": threshold,
        "line_count": count,
        "covered_lines": covered,
        "uncovered_lines": count - covered,
        "line_percent": percent,
        "mapped_files": len(mapped),
        "owned_files": len(included),
        "required_files": len(required),
        "missing_required_files": missing_required,
        "unexpected_owned_report_files": unexpected_owned,
        "execution_status": execution_status,
        "passed": passed,
    }
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
        for path in missing_required:
            writer.writerow((path, "MISSING_MAPPING", "", "", ""))

    status = "PASS" if passed else "FAIL"
    summary = (
        f"{args.layer} coverage: {status}\n"
        f"lines: {covered}/{count} ({percent:.2f}%)\n"
        f"threshold: {threshold:.2f}%\n"
        f"mapped owned files: {len(mapped)}/{len(included)}\n"
        f"mapped required files: {len(required) - len(missing_required)}/{len(required)}\n"
        f"missing required mappings: {len(missing_required)}\n"
        f"test/execution status: {execution_status}\n"
    )
    (output_dir / "coverage-summary.txt").write_text(summary, encoding="utf-8")
    print(summary, end="")
    return 0 if passed else 1


def shell_echo_payload(line: str) -> str | None:
    match = ECHO_LINE.match(line)
    if match is None:
        return None
    payload = match.group(1).strip()
    if len(payload) >= 2 and payload[0] == payload[-1] and payload[0] in "'\"":
        payload = payload[1:-1]
    if (
        PASS_PREFIX.search(payload)
        or PASS_SUFFIX.fullmatch(payload)
        or PASS_BANNER.fullmatch(payload)
    ):
        return payload
    return None


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


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
    results = read_sql_results(results_path)
    files = sorted(tests_dir.glob("[0-9]*.sql"))
    if not files:
        raise CoverageError(f"no SQL tests found under {tests_dir}")

    probes: list[dict[str, Any]] = []
    file_rows: list[dict[str, Any]] = []
    errors: list[str] = []
    covered_probe_count = 0
    passed_file_count = 0
    for path in files:
        name = path.name
        result = results.get(name)
        if result is None:
            errors.append(f"missing execution result: {name}")
            result = {"status": "missing", "exit_code": "", "log": ""}
        status = result["status"]
        passed = status == "pass"
        if passed:
            passed_file_count += 1
        log_text = ""
        if result.get("log"):
            expected_log = f"logs/{name}.log"
            if result["log"] != expected_log:
                errors.append(
                    f"SQL result for {name} has unexpected log path: {result['log']}"
                )
                log_path = None
            else:
                log_path = results_path.parent / result["log"]
                try:
                    log_text = log_path.read_text(encoding="utf-8", errors="replace")
                except OSError as exc:
                    errors.append(f"cannot read SQL log for {name}: {exc}")
        else:
            log_path = None
        output_lines = set(log_text.splitlines())
        markers: list[tuple[int, str]] = []
        for line_number, line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), start=1
        ):
            payload = shell_echo_payload(line)
            if payload is not None:
                markers.append((line_number, payload))
        if not markers:
            errors.append(
                f"SQL test has no explicit PASS/PASSED behavior marker: {name}"
            )
        observed_for_file = 0
        for line_number, marker in markers:
            covered = passed and marker in output_lines
            if covered:
                observed_for_file += 1
                covered_probe_count += 1
            probes.append(
                {
                    "id": f"{name}:{line_number}",
                    "file": name,
                    "source_line": line_number,
                    "marker": marker,
                    "covered": covered,
                }
            )
        file_rows.append(
            {
                "file": name,
                "sha256": sha256(path),
                "status": status,
                "exit_code": result.get("exit_code", ""),
                "declared_behavior_probes": len(markers),
                "observed_behavior_probes": observed_for_file,
                "log": result.get("log", ""),
                "log_sha256": sha256(log_path)
                if log_path and log_path.is_file()
                else None,
            }
        )

    unknown_results = sorted(set(results).difference(path.name for path in files))
    if unknown_results:
        errors.append(
            f"results contain unknown SQL files: {', '.join(unknown_results)}"
        )
    total_probes = len(probes)
    if total_probes == 0:
        errors.append("SQL behavior inventory contains zero probes")
    complete = (
        not errors
        and passed_file_count == len(files)
        and covered_probe_count == total_probes
    )
    document = {
        "schema_version": 1,
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "scope": (
            "Execution traceability for sql/tests/[0-9]*.sql. This inventory is supporting "
            "evidence; the SQL layer threshold is instrumented extension source-line coverage, "
            "not a test-count percentage."
        ),
        "test_files": len(files),
        "passed_test_files": passed_file_count,
        "declared_behavior_probes": total_probes,
        "covered_behavior_probes": covered_probe_count,
        "complete": complete,
        "errors": errors,
        "files": file_rows,
        "probes": probes,
    }
    output = pathlib.Path(args.output)
    write_json(output, document)
    print(
        "SQL execution inventory: "
        f"{passed_file_count}/{len(files)} files, "
        f"{covered_probe_count}/{total_probes} explicit behavior markers"
    )
    for error in errors:
        print(f"error: {error}", file=sys.stderr)
    return 0 if complete else 1


def aggregate(args: argparse.Namespace) -> int:
    artifact_dir = pathlib.Path(args.artifact_dir)
    layers: dict[str, Any] = {}
    errors: list[str] = []
    for name in ("rust", "cpp", "sql"):
        path = artifact_dir / name / "layer-summary.json"
        if not path.is_file():
            errors.append(f"missing {name} layer summary: {path}")
            continue
        summary = read_json(path)
        layers[name] = summary
        if not summary.get("passed", False):
            errors.append(f"{name} layer is below threshold or has missing mappings")
    inventory_path = artifact_dir / "sql" / "test-inventory.json"
    if not inventory_path.is_file():
        errors.append(f"missing SQL execution inventory: {inventory_path}")
    else:
        inventory = read_json(inventory_path)
        if not inventory.get("complete", False):
            errors.append("SQL execution inventory is incomplete")
    passed = not errors and len(layers) == 3
    result = {
        "schema_version": 1,
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "gate": "pg_accel three-layer release coverage",
        "passed": passed,
        "layers": layers,
        "errors": errors,
    }
    write_json(artifact_dir / "gate-summary.json", result)
    lines = ["# pg_accel three-layer coverage", ""]
    for name in ("rust", "cpp", "sql"):
        summary = layers.get(name)
        if summary is None:
            lines.append(f"- {name}: MISSING")
        else:
            status = "PASS" if summary["passed"] else "FAIL"
            lines.append(
                f"- {name}: {status} - {summary['covered_lines']}/{summary['line_count']} "
                f"lines ({summary['line_percent']:.2f}%, required "
                f"{summary['threshold_percent']:.2f}%)"
            )
    if errors:
        lines.extend(("", "## Errors", ""))
        lines.extend(f"- {error}" for error in errors)
    (artifact_dir / "gate-summary.md").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )
    print("\n".join(lines))
    return 0 if passed else 1


def audit_scope(args: argparse.Namespace) -> int:
    repo_root = pathlib.Path(args.repo_root).resolve()
    document = read_json(pathlib.Path(args.scope))
    minimum = validate_threshold(float(document.get("minimum_line_percent", 0)))
    if minimum != 90.0:
        raise CoverageError(
            f"checked-in minimum must remain exactly 90%; got {minimum:g}%"
        )
    for layer in ("rust", "cpp", "sql"):
        try:
            scope = document["layers"][layer]
        except (KeyError, TypeError) as exc:
            raise CoverageError(f"scope file is missing layer {layer}") from exc
        _, required = source_inventory(repo_root, scope)
        if not required:
            raise CoverageError(f"scope layer {layer} has no required source files")

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
        missing = sorted(actual - declared)
        stale = sorted(declared - actual)
        raise CoverageError(f"KERNEL_SOURCES drift: missing={missing}, stale={stale}")
    if "PGACCEL_ENABLE_COVERAGE" not in cmake:
        raise CoverageError("CMake coverage option is absent")

    sql_files = sorted((repo_root / "sql/tests").glob("[0-9]*.sql"))
    if not sql_files:
        raise CoverageError("SQL coverage inventory resolved to zero files")
    missing_markers = []
    marker_count = 0
    for path in sql_files:
        markers = [
            shell_echo_payload(line)
            for line in path.read_text(encoding="utf-8").splitlines()
        ]
        present_markers = [marker for marker in markers if marker is not None]
        marker_count += len(present_markers)
        if not present_markers:
            missing_markers.append(path.name)
    if missing_markers:
        raise CoverageError(f"SQL tests without behavior markers: {missing_markers}")
    print(
        "coverage scope audit: PASS "
        f"({len(actual)} C++ sources, {len(sql_files)} SQL files, "
        f"{marker_count} SQL behavior probes, threshold 90%)"
    )
    return 0


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    subparsers = root.add_subparsers(dest="command", required=True)

    summarize = subparsers.add_parser("summarize", help="summarize one LLVM JSON layer")
    summarize.add_argument("--layer", choices=("rust", "cpp", "sql"), required=True)
    summarize.add_argument("--input", required=True)
    summarize.add_argument("--scope", required=True)
    summarize.add_argument("--repo-root", default=".")
    summarize.add_argument("--threshold", type=float, required=True)
    summarize.add_argument("--execution-status", type=int, default=0)
    summarize.add_argument("--output-dir", required=True)
    summarize.set_defaults(func=summarize_layer)

    inventory = subparsers.add_parser(
        "sql-inventory", help="bind SQL execution logs to checked-in behavior markers"
    )
    inventory.add_argument("--tests-dir", required=True)
    inventory.add_argument("--results", required=True)
    inventory.add_argument("--output", required=True)
    inventory.set_defaults(func=sql_inventory)

    aggregate_parser = subparsers.add_parser(
        "aggregate", help="aggregate all three layers"
    )
    aggregate_parser.add_argument("--artifact-dir", required=True)
    aggregate_parser.set_defaults(func=aggregate)

    thresholds = subparsers.add_parser(
        "validate-thresholds", help="reject release thresholds below 90 percent"
    )
    thresholds.add_argument("values", nargs="+")
    thresholds.set_defaults(func=validate_thresholds)

    audit = subparsers.add_parser(
        "audit-scope", help="validate checked-in coverage scope"
    )
    audit.add_argument("--scope", required=True)
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
