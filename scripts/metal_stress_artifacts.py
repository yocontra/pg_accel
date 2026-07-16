#!/usr/bin/env python3
"""Build and validate the machine-readable Metal stress evidence contract."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import re
import sys
from pathlib import Path
from typing import Any, TextIO


SCHEMA_VERSION = 1
CACHE_SCOPES = (
    "all_cache_files",
    "metal_binary_archive",
    "metallib",
    "metalar",
    "jit",
    "other",
)
KERNEL_CLASSES = (
    ("reduce_f32", "reduce"),
    ("h3_lat_lng_fp64", "h3"),
    ("point_in_polygon_f32", "pip"),
)
LATENCY_KEYS = {
    "worker",
    "init_us",
    "cold_iteration_us",
    "cold_reduce_us",
    "cold_h3_us",
    "cold_pip_us",
    "warm_iterations",
    "warm_iteration_total_us",
    "warm_iteration_max_us",
    "warm_reduce_total_us",
    "warm_reduce_max_us",
    "warm_h3_total_us",
    "warm_h3_max_us",
    "warm_pip_total_us",
    "warm_pip_max_us",
    "wall_us",
}
CORE_ARTIFACTS = (
    ("metadata.txt", "environment_metadata"),
    ("summary.txt", "gate_summary"),
    ("gpu-build.log", "build_log"),
    ("install.log", "gate_log"),
    ("extension-smoke.log", "gate_log"),
    ("sql-tests.log", "gate_log"),
    ("clean-logs.log", "gate_log"),
    ("standalone-gpu-tests.log", "gate_log"),
    ("archive-cache-clear.log", "archive_cache_log"),
    ("archive-cache-before.log", "archive_cache_log"),
    ("archive-fork-stress.log", "archive_stress_log"),
    ("archive-fork-stress-raw.log", "archive_stress_bound_raw_log"),
    ("archive-cache-after.log", "archive_cache_log"),
    ("archive-artifacts.log", "artifact_validation_log"),
    ("metal-cache-before-archive.json", "cache_snapshot"),
    ("metal-cache-after-archive.json", "cache_snapshot"),
    ("metal-stress-metrics.json", "stress_metrics"),
    ("metal-stress-cache.tsv", "cache_metrics"),
    ("metal-stress-latency.tsv", "latency_metrics"),
    ("metal-stress-metrics-summary.txt", "metrics_summary"),
    ("artifact-index.log", "artifact_validation_log"),
    ("bench-gpu_reduce_sum-100000.log", "benchmark_log"),
    ("bench-gpu_nlj_between-50000.log", "benchmark_log"),
    ("bench-gpu_sort_topk_wide-100000.log", "benchmark_log"),
    ("bench-h3_bulk-100000.log", "benchmark_log"),
    ("bench-spatial_filter-100000.log", "benchmark_log"),
    ("bench-raster_reclass-100.log", "benchmark_log"),
    ("cancellation.log", "gate_log"),
    ("postgres-log-tail.txt", "postgres_log_audit"),
)
BENCHMARK_DIRS = (
    "bench-gpu_reduce_sum-100000",
    "bench-gpu_nlj_between-50000",
    "bench-gpu_sort_topk_wide-100000",
    "bench-h3_bulk-100000",
    "bench-spatial_filter-100000",
    "bench-raster_reclass-100",
)


class ArtifactContractError(ValueError):
    """The stress evidence is incomplete, malformed, or contradictory."""


def _write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def resolve_cache_dir(explicit: str | None = None) -> Path:
    if explicit:
        return Path(explicit).expanduser().resolve()
    appdb = os.environ.get("ACPP_APPDB_DIR")
    if appdb:
        return (Path(appdb).expanduser() / "global" / "jit-cache").resolve()
    home = os.environ.get("HOME")
    if not home:
        raise ArtifactContractError(
            "cannot resolve AdaptiveCpp cache: HOME and ACPP_APPDB_DIR are unset"
        )
    return (
        Path(home).expanduser() / ".acpp" / "apps" / "global" / "jit-cache"
    ).resolve()


def measure_cache(cache_dir: Path, point: str) -> dict[str, Any]:
    cache_dir = cache_dir.expanduser().resolve()
    counts = {scope: {"file_count": 0, "total_bytes": 0} for scope in CACHE_SCOPES}
    directory_exists = cache_dir.exists()
    if directory_exists and not cache_dir.is_dir():
        raise ArtifactContractError(
            f"AdaptiveCpp cache path is not a directory: {cache_dir}"
        )

    if directory_exists:
        try:
            entries = sorted(cache_dir.iterdir(), key=lambda entry: entry.name)
            for entry in entries:
                if entry.is_symlink() or not entry.is_file():
                    raise ArtifactContractError(
                        f"unsupported non-regular cache entry: {entry}"
                    )
                size = entry.stat().st_size
                suffix = entry.suffix
                if suffix == ".metallib":
                    scope = "metallib"
                elif suffix == ".metalar":
                    scope = "metalar"
                elif suffix == ".jit":
                    scope = "jit"
                else:
                    scope = "other"
                counts[scope]["file_count"] += 1
                counts[scope]["total_bytes"] += size
                counts["all_cache_files"]["file_count"] += 1
                counts["all_cache_files"]["total_bytes"] += size
                if scope in {"metallib", "metalar"}:
                    counts["metal_binary_archive"]["file_count"] += 1
                    counts["metal_binary_archive"]["total_bytes"] += size
        except OSError as error:
            raise ArtifactContractError(
                f"failed to scan AdaptiveCpp cache {cache_dir}: {error}"
            ) from error

    return {
        "artifact_type": "metal_cache_snapshot",
        "cache_dir": str(cache_dir),
        "directory_exists": directory_exists,
        "measurements": counts,
        "point": point,
        "schema_version": SCHEMA_VERSION,
    }


def _require_nonnegative_integer(value: Any, context: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ArtifactContractError(f"{context} must be a non-negative integer")
    return value


def load_snapshot(path: Path, expected_point: str) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArtifactContractError(
            f"cannot read cache snapshot {path}: {error}"
        ) from error
    if not isinstance(payload, dict):
        raise ArtifactContractError(f"cache snapshot is not a JSON object: {path}")
    if payload.get("schema_version") != SCHEMA_VERSION:
        raise ArtifactContractError(f"unsupported cache snapshot schema: {path}")
    if payload.get("artifact_type") != "metal_cache_snapshot":
        raise ArtifactContractError(f"wrong cache snapshot artifact_type: {path}")
    if payload.get("point") != expected_point:
        raise ArtifactContractError(
            f"cache snapshot point mismatch: expected {expected_point}, got {payload.get('point')!r}"
        )
    measurements = payload.get("measurements")
    if not isinstance(payload.get("cache_dir"), str) or not payload["cache_dir"]:
        raise ArtifactContractError(f"cache snapshot cache_dir is missing: {path}")
    if not isinstance(payload.get("directory_exists"), bool):
        raise ArtifactContractError(
            f"cache snapshot directory_exists is not boolean: {path}"
        )
    if not isinstance(measurements, dict):
        raise ArtifactContractError(f"cache snapshot measurements are missing: {path}")
    for scope in CACHE_SCOPES:
        measurement = measurements.get(scope)
        if not isinstance(measurement, dict):
            raise ArtifactContractError(
                f"cache snapshot scope {scope} is missing: {path}"
            )
        _require_nonnegative_integer(
            measurement.get("file_count"), f"{scope}.file_count"
        )
        _require_nonnegative_integer(
            measurement.get("total_bytes"), f"{scope}.total_bytes"
        )
    for field in ("file_count", "total_bytes"):
        categorized = sum(
            measurements[scope][field]
            for scope in ("metallib", "metalar", "jit", "other")
        )
        if measurements["all_cache_files"][field] != categorized:
            raise ArtifactContractError(
                f"cache snapshot {field} category totals are inconsistent: {path}"
            )
        archive = measurements["metallib"][field] + measurements["metalar"][field]
        if measurements["metal_binary_archive"][field] != archive:
            raise ArtifactContractError(
                f"cache snapshot archive {field} total is inconsistent: {path}"
            )
    return payload


def _only_match(pattern: str, text: str, description: str) -> re.Match[str]:
    matches = list(re.finditer(pattern, text, re.MULTILINE))
    if len(matches) != 1:
        raise ArtifactContractError(
            f"expected exactly one {description}, found {len(matches)}"
        )
    return matches[0]


def _parse_count_line(text: str, prefix: str) -> dict[str, int]:
    if prefix == "pre-fork":
        pattern = r"^pre-fork archive cache: metallib=(\d+) metalar=(\d+) jit=(\d+) orphan=(\d+)$"
    else:
        pattern = (
            r"^post-fork archive cache: metallib=(\d+) metalar=(\d+) jit=(\d+) orphan=(\d+) "
            r"\(delta_metallib=(-?\d+) delta_metalar=(-?\d+)\)$"
        )
    match = _only_match(pattern, text, f"{prefix} cache count line")
    counts = {
        "metallib": int(match.group(1)),
        "metalar": int(match.group(2)),
        "jit": int(match.group(3)),
        "orphan": int(match.group(4)),
    }
    if prefix != "pre-fork":
        counts["delta_metallib"] = int(match.group(5))
        counts["delta_metalar"] = int(match.group(6))
    return counts


def _parse_single_integer(text: str, key: str) -> int:
    match = _only_match(rf"^{re.escape(key)}=(\d+)(?:\s.*)?$", text, key)
    return int(match.group(1))


def _read_bound_archive_log(path: Path) -> tuple[str, dict[str, Any]]:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise ArtifactContractError(
            f"cannot read archive stress log {path}: {error}"
        ) from error
    lines = raw.splitlines(keepends=True)
    if len(lines) < 3:
        raise ArtifactContractError(
            "archive stress raw log is missing its binding envelope"
        )
    try:
        start = lines[0].decode("utf-8").rstrip("\r\n")
        footer = lines[-1].decode("utf-8").rstrip("\r\n")
        body = b"".join(lines[1:-1])
        text = body.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ArtifactContractError(
            f"archive stress raw log is not UTF-8: {error}"
        ) from error
    if start != "PGACCEL_TEST_START name=gpu-stress-archive":
        raise ArtifactContractError("archive stress raw log has the wrong start marker")
    footer_match = re.fullmatch(
        r"PGACCEL_TEST_RESULT name=gpu-stress-archive exit_code=(\d+) result=(PASS|FAIL) "
        r"raw_lines=(\d+) body_sha256=([0-9a-f]{64}) binding_sha256=([0-9a-f]{64})",
        footer,
    )
    if footer_match is None:
        raise ArtifactContractError(
            "archive stress raw log has a malformed result marker"
        )
    exit_code = int(footer_match.group(1))
    result = footer_match.group(2)
    raw_lines = int(footer_match.group(3))
    body_sha256 = footer_match.group(4)
    binding_sha256 = footer_match.group(5)
    if exit_code != 0 or result != "PASS":
        raise ArtifactContractError("archive stress raw log binding reports failure")
    if len(body.splitlines()) != raw_lines:
        raise ArtifactContractError(
            "archive stress raw line count contradicts its binding"
        )
    if hashlib.sha256(body).hexdigest() != body_sha256:
        raise ArtifactContractError(
            "archive stress raw body hash does not match its binding"
        )
    binding = hashlib.sha256()
    binding.update(b"pgaccel-ctest-body-v1\0")
    binding.update(b"gpu-stress-archive\0")
    binding.update(str(exit_code).encode("ascii"))
    binding.update(b"\0")
    binding.update(result.encode("ascii"))
    binding.update(b"\0")
    binding.update(str(raw_lines).encode("ascii"))
    binding.update(b"\0")
    binding.update(bytes.fromhex(body_sha256))
    if binding.hexdigest() != binding_sha256:
        raise ArtifactContractError("archive stress raw binding hash is invalid")
    return text, {
        "binding_sha256": binding_sha256,
        "body_sha256": body_sha256,
        "raw_lines": raw_lines,
    }


def parse_archive_log(path: Path) -> dict[str, Any]:
    text, log_binding = _read_bound_archive_log(path)

    dimensions = _only_match(
        r"^workers=(\d+) iterations_per_worker=(\d+) total_dispatches=(\d+)$",
        text,
        "archive workload dimensions",
    )
    workers = int(dimensions.group(1))
    iterations = int(dimensions.group(2))
    total_dispatches = int(dimensions.group(3))
    if workers < 1 or iterations < 2:
        raise ArtifactContractError(
            "archive stress requires at least one worker and two iterations"
        )
    if total_dispatches != workers * iterations * len(KERNEL_CLASSES):
        raise ArtifactContractError(
            "archive stress total_dispatches contradicts workload dimensions"
        )

    cache_dir_match = _only_match(r"^jit_cache_dir=(.+)$", text, "JIT cache directory")
    raw_cache_dir = cache_dir_match.group(1)
    if raw_cache_dir == "<unresolved>":
        raise ArtifactContractError(
            "archive stress could not resolve its JIT cache directory"
        )
    cache_dir = str(Path(raw_cache_dir).expanduser().resolve())

    records: list[dict[str, int]] = []
    for line in text.splitlines():
        if not line.startswith("latency_record_us "):
            continue
        fields: dict[str, int] = {}
        for token in line.removeprefix("latency_record_us ").split():
            if "=" not in token:
                raise ArtifactContractError(f"malformed latency token: {token!r}")
            key, raw_value = token.split("=", 1)
            if key in fields:
                raise ArtifactContractError(f"duplicate latency field: {key}")
            if not raw_value.isdecimal():
                raise ArtifactContractError(
                    f"non-integer latency value for {key}: {raw_value!r}"
                )
            fields[key] = int(raw_value)
        missing = LATENCY_KEYS.difference(fields)
        extra = fields.keys() - LATENCY_KEYS
        if missing or extra:
            raise ArtifactContractError(
                f"latency record fields differ from contract: missing={sorted(missing)} extra={sorted(extra)}"
            )
        records.append(fields)

    if len(records) != workers:
        raise ArtifactContractError(
            f"expected {workers} latency records, found {len(records)}"
        )
    records.sort(key=lambda record: record["worker"])
    if [record["worker"] for record in records] != list(range(workers)):
        raise ArtifactContractError("latency worker indexes are missing or duplicated")

    for record in records:
        worker = record["worker"]
        if record["warm_iterations"] != iterations - 1:
            raise ArtifactContractError(
                f"worker {worker} warm iteration count does not match the stress dimensions"
            )
        for key in (
            "init_us",
            "cold_iteration_us",
            "cold_reduce_us",
            "cold_h3_us",
            "cold_pip_us",
        ):
            if record[key] <= 0:
                raise ArtifactContractError(
                    f"worker {worker} has missing/zero measurement {key}"
                )
        for _class_name, short_name in KERNEL_CLASSES:
            total = record[f"warm_{short_name}_total_us"]
            maximum = record[f"warm_{short_name}_max_us"]
            if total <= 0 or maximum <= 0 or maximum > total:
                raise ArtifactContractError(
                    f"worker {worker} has invalid warm {short_name} measurements"
                )
        if (
            record["warm_iteration_total_us"] <= 0
            or record["warm_iteration_max_us"] <= 0
            or record["warm_iteration_max_us"] > record["warm_iteration_total_us"]
        ):
            raise ArtifactContractError(
                f"worker {worker} has invalid warm iteration measurements"
            )
        if record["wall_us"] < record["cold_iteration_us"]:
            raise ArtifactContractError(
                f"worker {worker} wall time is below its cold iteration time"
            )

    succeeded = _only_match(
        r"^workers_succeeded=(\d+) / (\d+)$", text, "worker success total"
    )
    if int(succeeded.group(1)) != workers or int(succeeded.group(2)) != workers:
        raise ArtifactContractError("not every archive stress worker succeeded")
    for key in (
        "workers_crashed",
        "reports_missing",
        "xpc_compiler_service_hits",
        "pipeline_state_failures",
        "archive_load_failures",
        "archive_build_failures",
        "posix_spawn_failures",
        "cache_hash_instability_failures",
    ):
        if _parse_single_integer(text, key) != 0:
            raise ArtifactContractError(f"archive stress reported nonzero {key}")
    result = _only_match(
        r"^RESULT: (PASS|FAIL)(?:\s.*)?$", text, "archive stress result"
    )
    if result.group(1) != "PASS":
        raise ArtifactContractError("archive stress result is not PASS")

    pre_cache_counts = _parse_count_line(text, "pre-fork")
    post_cache_counts = _parse_count_line(text, "post-fork")
    if pre_cache_counts["orphan"] != 0 or post_cache_counts["orphan"] != 0:
        raise ArtifactContractError("archive stress reported orphan metallib files")
    if (
        post_cache_counts["delta_metallib"]
        != post_cache_counts["metallib"] - pre_cache_counts["metallib"]
        or post_cache_counts["delta_metalar"]
        != post_cache_counts["metalar"] - pre_cache_counts["metalar"]
    ):
        raise ArtifactContractError(
            "archive stress cache deltas contradict its before/after counts"
        )

    return {
        "cache_dir": cache_dir,
        "iterations_per_worker": iterations,
        "latency_records": records,
        "log_binding": log_binding,
        "post_cache_counts": post_cache_counts,
        "pre_cache_counts": pre_cache_counts,
        "total_dispatches": total_dispatches,
        "workers": workers,
    }


def _measurement(snapshot: dict[str, Any], scope: str) -> dict[str, int]:
    return snapshot["measurements"][scope]


def _validate_cache_contract(
    before: dict[str, Any], after: dict[str, Any], parsed: dict[str, Any]
) -> None:
    if before.get("cache_dir") != after.get("cache_dir"):
        raise ArtifactContractError(
            "before/after snapshots resolved different cache directories"
        )
    if before.get("cache_dir") != parsed.get("cache_dir"):
        raise ArtifactContractError(
            "cache snapshots and archive stress log resolved different directories"
        )
    if after.get("directory_exists") is not True:
        raise ArtifactContractError("post-stress cache directory was not observed")
    before_all = _measurement(before, "all_cache_files")
    if before_all["file_count"] != 0 or before_all["total_bytes"] != 0:
        raise ArtifactContractError(
            "pre-stress AdaptiveCpp cache is not empty; cold evidence is invalid"
        )
    for scope in ("metallib", "metalar", "jit"):
        if (
            _measurement(before, scope)["file_count"]
            != parsed["pre_cache_counts"][scope]
        ):
            raise ArtifactContractError(
                f"pre-stress {scope} count contradicts the archive log"
            )
        if (
            _measurement(after, scope)["file_count"]
            != parsed["post_cache_counts"][scope]
        ):
            raise ArtifactContractError(
                f"post-stress {scope} count contradicts the archive log"
            )
    for scope in ("metallib", "metalar", "jit"):
        measurement = _measurement(after, scope)
        if measurement["file_count"] == 0 or measurement["total_bytes"] == 0:
            raise ArtifactContractError(
                f"post-stress {scope} cache measurement is empty"
            )
    if (
        _measurement(after, "metallib")["file_count"]
        != _measurement(after, "metalar")["file_count"]
    ):
        raise ArtifactContractError(
            "post-stress metallib/metalar file counts are not paired"
        )


def _average(total: int, samples: int) -> float:
    if samples <= 0:
        raise ArtifactContractError("cannot compute an average without samples")
    return round(total / samples, 3)


def build_metrics(
    before: dict[str, Any], after: dict[str, Any], parsed: dict[str, Any]
) -> dict[str, Any]:
    _validate_cache_contract(before, after, parsed)
    workers: list[dict[str, Any]] = []
    for record in parsed["latency_records"]:
        classes: list[dict[str, Any]] = []
        for class_name, short_name in KERNEL_CLASSES:
            warm_samples = record["warm_iterations"]
            warm_total = record[f"warm_{short_name}_total_us"]
            classes.append(
                {
                    "cold_first_dispatch_us": record[f"cold_{short_name}_us"],
                    "kernel_class": class_name,
                    "warm_cache": {
                        "average_us": _average(warm_total, warm_samples),
                        "max_us": record[f"warm_{short_name}_max_us"],
                        "sample_count": warm_samples,
                        "total_us": warm_total,
                    },
                }
            )
        workers.append(
            {
                "cold_iteration_us": record["cold_iteration_us"],
                "init_us": record["init_us"],
                "kernel_classes": classes,
                "wall_us": record["wall_us"],
                "warm_iteration": {
                    "average_us": _average(
                        record["warm_iteration_total_us"], record["warm_iterations"]
                    ),
                    "max_us": record["warm_iteration_max_us"],
                    "sample_count": record["warm_iterations"],
                    "total_us": record["warm_iteration_total_us"],
                },
                "worker_index": record["worker"],
            }
        )

    summaries: list[dict[str, Any]] = []
    for class_name, short_name in KERNEL_CLASSES:
        cold_values = [
            record[f"cold_{short_name}_us"] for record in parsed["latency_records"]
        ]
        warm_samples = sum(
            record["warm_iterations"] for record in parsed["latency_records"]
        )
        warm_total = sum(
            record[f"warm_{short_name}_total_us"]
            for record in parsed["latency_records"]
        )
        summaries.append(
            {
                "cold_first_dispatch": {
                    "average_us": _average(sum(cold_values), len(cold_values)),
                    "max_us": max(cold_values),
                    "min_us": min(cold_values),
                    "sample_count": len(cold_values),
                    "total_us": sum(cold_values),
                },
                "kernel_class": class_name,
                "warm_cache": {
                    "average_us": _average(warm_total, warm_samples),
                    "max_us": max(
                        record[f"warm_{short_name}_max_us"]
                        for record in parsed["latency_records"]
                    ),
                    "sample_count": warm_samples,
                    "total_us": warm_total,
                },
            }
        )

    return {
        "artifact_type": "metal_stress_metrics",
        "cache_snapshots": [before, after],
        "latency": {
            "kernel_class_summaries": summaries,
            "performance_policy": "visibility_only_no_new_threshold",
            "workers": workers,
        },
        "raw_log_binding": parsed["log_binding"],
        "schema_version": SCHEMA_VERSION,
        "workload": {
            "iterations_per_worker": parsed["iterations_per_worker"],
            "total_dispatches": parsed["total_dispatches"],
            "workers": parsed["workers"],
        },
    }


def write_cache_tsv(path: Path, before: dict[str, Any], after: dict[str, Any]) -> None:
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.writer(handle, delimiter="\t", lineterminator="\n")
        writer.writerow(("point", "scope", "file_count", "total_bytes"))
        for snapshot in (before, after):
            for scope in CACHE_SCOPES:
                value = _measurement(snapshot, scope)
                writer.writerow(
                    (
                        snapshot["point"],
                        scope,
                        value["file_count"],
                        value["total_bytes"],
                    )
                )


def write_latency_tsv(path: Path, metrics: dict[str, Any]) -> None:
    with path.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.writer(handle, delimiter="\t", lineterminator="\n")
        writer.writerow(
            (
                "worker_index",
                "kernel_class",
                "cold_first_dispatch_us",
                "warm_sample_count",
                "warm_total_us",
                "warm_average_us",
                "warm_max_us",
            )
        )
        for worker in metrics["latency"]["workers"]:
            for kernel_class in worker["kernel_classes"]:
                warm = kernel_class["warm_cache"]
                writer.writerow(
                    (
                        worker["worker_index"],
                        kernel_class["kernel_class"],
                        kernel_class["cold_first_dispatch_us"],
                        warm["sample_count"],
                        warm["total_us"],
                        f"{warm['average_us']:.3f}",
                        warm["max_us"],
                    )
                )


def write_summary(path: Path, metrics: dict[str, Any]) -> None:
    before, after = metrics["cache_snapshots"]
    lines = [
        "metal-stress artifact contract: PASS",
        f"cache_dir={before['cache_dir']}",
    ]
    for snapshot in (before, after):
        all_cache = _measurement(snapshot, "all_cache_files")
        archive = _measurement(snapshot, "metal_binary_archive")
        lines.append(
            f"cache point={snapshot['point']} files={all_cache['file_count']} "
            f"bytes={all_cache['total_bytes']} archive_files={archive['file_count']} "
            f"archive_bytes={archive['total_bytes']}"
        )
    for summary in metrics["latency"]["kernel_class_summaries"]:
        cold = summary["cold_first_dispatch"]
        warm = summary["warm_cache"]
        lines.append(
            f"latency class={summary['kernel_class']} cold_first_min_us={cold['min_us']} "
            f"cold_first_max_us={cold['max_us']} cold_first_average_us={cold['average_us']:.3f} "
            f"warm_samples={warm['sample_count']} warm_average_us={warm['average_us']:.3f} "
            f"warm_max_us={warm['max_us']}"
        )
    lines.append("performance_policy=visibility_only_no_new_threshold")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def finalize_artifacts(
    before_path: Path, after_path: Path, log_path: Path, output_dir: Path
) -> None:
    before = load_snapshot(before_path, "before_cold_archive_stress")
    after = load_snapshot(after_path, "after_cold_archive_stress")
    parsed = parse_archive_log(log_path)
    metrics = build_metrics(before, after, parsed)
    output_dir.mkdir(parents=True, exist_ok=True)
    _write_json(output_dir / "metal-stress-metrics.json", metrics)
    write_cache_tsv(output_dir / "metal-stress-cache.tsv", before, after)
    write_latency_tsv(output_dir / "metal-stress-latency.tsv", metrics)
    write_summary(output_dir / "metal-stress-metrics-summary.txt", metrics)


def write_artifact_index(artifact_dir: Path) -> None:
    artifacts: list[dict[str, Any]] = []
    for relative, role in CORE_ARTIFACTS:
        path = artifact_dir / relative
        if not path.is_file() or path.stat().st_size == 0:
            raise ArtifactContractError(
                f"required Metal stress artifact is missing or empty: {path}"
            )
        artifacts.append({"path": relative, "required": True, "role": role})
    for benchmark_dir in BENCHMARK_DIRS:
        relative = f"{benchmark_dir}/artifact_index.json"
        path = artifact_dir / relative
        if not path.is_file() or path.stat().st_size == 0:
            raise ArtifactContractError(
                f"benchmark artifact index is missing or empty: {path}"
            )
        artifacts.append(
            {"path": relative, "required": True, "role": "benchmark_artifact_index"}
        )
    payload = {
        "artifact_type": "metal_stress_artifact_index",
        "artifacts": artifacts,
        "schema_version": SCHEMA_VERSION,
    }
    _write_json(artifact_dir / "artifact_index.json", payload)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    snapshot = subparsers.add_parser(
        "snapshot", help="measure the AdaptiveCpp Metal cache"
    )
    snapshot.add_argument("--point", required=True)
    snapshot.add_argument("--output", required=True, type=Path)
    snapshot.add_argument("--cache-dir")

    finalize = subparsers.add_parser(
        "finalize", help="validate and render archive metrics"
    )
    finalize.add_argument("--before", required=True, type=Path)
    finalize.add_argument("--after", required=True, type=Path)
    finalize.add_argument("--archive-log", required=True, type=Path)
    finalize.add_argument("--output-dir", required=True, type=Path)

    index = subparsers.add_parser(
        "index", help="validate and index the complete stress gate"
    )
    index.add_argument("--artifact-dir", required=True, type=Path)
    return parser


def main(argv: list[str] | None = None, stderr: TextIO = sys.stderr) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "snapshot":
            cache_dir = resolve_cache_dir(args.cache_dir)
            _write_json(args.output, measure_cache(cache_dir, args.point))
        elif args.command == "finalize":
            finalize_artifacts(
                args.before, args.after, args.archive_log, args.output_dir
            )
        else:
            write_artifact_index(args.artifact_dir)
    except ArtifactContractError as error:
        print(f"error: {error}", file=stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
