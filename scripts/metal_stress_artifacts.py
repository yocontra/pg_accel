#!/usr/bin/env python3
"""Build and validate the machine-readable Metal stress evidence contract."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any, TextIO


SCHEMA_VERSION = 1
LOG_EXCERPT_BYTES = 64 * 1024
LOG_FAILURE_PATTERN = re.compile(
    rb"PGACCEL PANIC|PANIC|segmentation fault|MTLCompilerService|resource leak|leaked resource|"
    rb"kernel[^\r\n]{0,96}(?:failed|failure)|server process[^\r\n]{0,160}terminated by signal|"
    rb"crash recovery|reinitializ(?:e|ing|ation)",
    re.IGNORECASE,
)
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
    ("candidate-provenance.json", "exact_candidate_provenance"),
    ("candidate-provenance.log", "exact_candidate_provenance_log"),
    ("acpp-provenance.txt", "adaptivecpp_toolchain_provenance"),
    ("acpp-provenance.log", "adaptivecpp_toolchain_provenance_log"),
    ("metadata.txt", "environment_metadata"),
    ("summary.txt", "gate_summary"),
    ("gpu-build.log", "build_log"),
    ("install.log", "gate_log"),
    ("extension-smoke.log", "gate_log"),
    ("sql-tests.log", "gate_log"),
    ("clean-logs.log", "gate_log"),
    ("log-start-offsets.log", "log_binding_log"),
    ("metal-log-start-offsets.json", "log_start_offsets"),
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
    ("bench-gpu_reduce_sum-100000.log", "benchmark_log"),
    ("bench-gpu_nlj_between-50000.log", "benchmark_log"),
    ("bench-gpu_sort_topk_wide-100000.log", "benchmark_log"),
    ("bench-h3_bulk-100000.log", "benchmark_log"),
    ("bench-spatial_filter-100000.log", "benchmark_log"),
    ("bench-raster_reclass-100.log", "benchmark_log"),
    ("cancellation.log", "gate_log"),
    ("postgres-log-audit.log", "artifact_validation_log"),
    ("postgres-log-audit.json", "postgres_log_audit"),
    ("postgres-log-tail.txt", "postgres_log_audit_excerpt"),
)
CANDIDATE_SOURCE_INPUTS = (
    ".acpp-version",
    ".tool-versions",
    "Cargo.lock",
    "patches/adaptivecpp/default-targets-json.patch",
    "patches/adaptivecpp/sleef-helper-address-space-specialization.patch",
    "patches/adaptivecpp/soft-fp-2-package-integration.patch",
    "patches/adaptivecpp/sscp-host-coverage.patch",
    "patches/soft-fp/metal-constexpr-bitcast.patch",
    "scripts/metal_stress_artifacts.py",
    "scripts/metal_stress_gate.sh",
)
GIT_OBJECT_RE = re.compile(r"^[0-9a-f]{40}(?:[0-9a-f]{24})?$")
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


def _git(repo_root: Path, *arguments: str, allow_failure: bool = False) -> str:
    try:
        completed = subprocess.run(
            ["git", *arguments],
            cwd=repo_root,
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ArtifactContractError(
            f"cannot inspect exact candidate Git state: {error}"
        ) from error
    if completed.returncode != 0:
        if allow_failure:
            return ""
        detail = (
            completed.stderr.strip()
            or completed.stdout.strip()
            or "unknown Git error"
        )
        raise ArtifactContractError(f"cannot inspect exact candidate Git state: {detail}")
    return completed.stdout.strip()


def _candidate_source_input(repo_root: Path, relative: str) -> dict[str, Any]:
    path = repo_root / relative
    if path.is_symlink() or not path.is_file():
        raise ArtifactContractError(
            f"exact candidate source input is missing: {relative}"
        )
    if not _git(repo_root, "ls-files", "--error-unmatch", "--", relative):
        raise ArtifactContractError(
            f"exact candidate source input is not tracked: {relative}"
        )
    raw = path.read_bytes()
    return {
        "path": relative,
        "sha256": hashlib.sha256(raw).hexdigest(),
        "size_bytes": len(raw),
    }


def validate_candidate_provenance(payload: Any) -> dict[str, Any]:
    if not isinstance(payload, dict):
        raise ArtifactContractError("exact candidate provenance is not a JSON object")
    if (
        payload.get("artifact_type") != "metal_stress_candidate_provenance"
        or payload.get("schema_version") != SCHEMA_VERSION
        or payload.get("status") != "pass"
        or payload.get("clean_worktree") is not True
    ):
        raise ArtifactContractError(
            "exact candidate provenance has the wrong schema or status"
        )
    for field in ("commit", "tree"):
        value = payload.get(field)
        if not isinstance(value, str) or GIT_OBJECT_RE.fullmatch(value) is None:
            raise ArtifactContractError(
                f"exact candidate provenance has an invalid {field}"
            )
    if payload.get("git_status_sha256") != hashlib.sha256(b"").hexdigest():
        raise ArtifactContractError("exact candidate provenance does not bind an empty Git status")
    if (
        not isinstance(payload.get("repository_root"), str)
        or not payload["repository_root"]
    ):
        raise ArtifactContractError("exact candidate provenance is missing its repository root")
    if payload.get("head_ref") is not None and not isinstance(
        payload.get("head_ref"), str
    ):
        raise ArtifactContractError("exact candidate provenance has an invalid head ref")
    inputs = payload.get("source_inputs")
    if not isinstance(inputs, list) or [
        row.get("path") for row in inputs if isinstance(row, dict)
    ] != list(CANDIDATE_SOURCE_INPUTS):
        raise ArtifactContractError(
            "exact candidate provenance source inputs are incomplete or reordered"
        )
    for row in inputs:
        if (
            not isinstance(row, dict)
            or set(row) != {"path", "sha256", "size_bytes"}
            or not isinstance(row["size_bytes"], int)
            or row["size_bytes"] < 0
            or not isinstance(row["sha256"], str)
            or re.fullmatch(r"[0-9a-f]{64}", row["sha256"]) is None
        ):
            raise ArtifactContractError("exact candidate provenance has a malformed source input")
    return payload


def capture_candidate_provenance(repo_root: Path) -> dict[str, Any]:
    repo_root = repo_root.expanduser().resolve()
    discovered_root = Path(_git(repo_root, "rev-parse", "--show-toplevel")).resolve()
    if discovered_root != repo_root:
        raise ArtifactContractError(
            f"exact candidate repo root mismatch: requested={repo_root} actual={discovered_root}"
        )
    status = _git(
        repo_root,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--ignore-submodules=none",
    )
    if status:
        preview = " | ".join(status.splitlines()[:8])
        raise ArtifactContractError(f"exact candidate worktree is dirty: {preview}")
    commit = _git(repo_root, "rev-parse", "--verify", "HEAD^{commit}")
    tree = _git(repo_root, "rev-parse", "--verify", "HEAD^{tree}")
    head_ref = _git(
        repo_root,
        "symbolic-ref",
        "--quiet",
        "--short",
        "HEAD",
        allow_failure=True,
    )
    payload = {
        "artifact_type": "metal_stress_candidate_provenance",
        "clean_worktree": True,
        "commit": commit,
        "git_status_sha256": hashlib.sha256(status.encode("utf-8")).hexdigest(),
        "head_ref": head_ref or None,
        "repository_root": str(repo_root),
        "schema_version": SCHEMA_VERSION,
        "source_inputs": [
            _candidate_source_input(repo_root, relative)
            for relative in CANDIDATE_SOURCE_INPUTS
        ],
        "status": "pass",
        "tree": tree,
    }
    return validate_candidate_provenance(payload)


def load_candidate_provenance(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArtifactContractError(f"cannot read exact candidate provenance: {error}") from error
    return validate_candidate_provenance(payload)


def load_crash_count(path: Path) -> int:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArtifactContractError(
            f"cannot read benchmark crash artifact {path}: {error}"
        ) from error
    if isinstance(payload, list):
        return len(payload)
    raise ArtifactContractError(
        f"benchmark crash artifact has an unknown schema: {path}"
    )


def _log_source(role: str, path: Path) -> dict[str, Any]:
    path = path.expanduser().resolve()
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise ArtifactContractError(f"Metal stress {role} path is not a file: {path}")
    try:
        stat = path.stat() if path.exists() else None
    except OSError as error:
        raise ArtifactContractError(f"cannot stat Metal stress {role} {path}: {error}") from error
    digest = hashlib.sha256()
    if stat is not None:
        try:
            with path.open("rb") as handle:
                while chunk := handle.read(64 * 1024):
                    digest.update(chunk)
        except OSError as error:
            raise ArtifactContractError(f"cannot hash Metal stress {role} {path}: {error}") from error
    return {
        "device": stat.st_dev if stat is not None else None,
        "existed": stat is not None,
        "inode": stat.st_ino if stat is not None else None,
        "length_bytes": stat.st_size if stat is not None else 0,
        "path": str(path),
        "prefix_sha256": digest.hexdigest(),
        "role": role,
    }


def snapshot_log_offsets(postgres_log: Path, panic_log: Path) -> dict[str, Any]:
    sources = [
        _log_source("postgres_log", postgres_log),
        _log_source("panic_log", panic_log),
    ]
    panic = sources[1]
    if panic["length_bytes"] != 0:
        raise ArtifactContractError(
            f"panic log is non-empty at Metal stress audit start: {panic['path']}"
        )
    return {
        "artifact_type": "metal_stress_log_start_offsets",
        "schema_version": SCHEMA_VERSION,
        "sources": sources,
    }


def load_log_offsets(path: Path) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArtifactContractError(f"cannot read log-offset artifact {path}: {error}") from error
    if not isinstance(payload, dict):
        raise ArtifactContractError(f"log-offset artifact is not a JSON object: {path}")
    if payload.get("schema_version") != SCHEMA_VERSION or payload.get(
        "artifact_type"
    ) != "metal_stress_log_start_offsets":
        raise ArtifactContractError(f"log-offset artifact has the wrong schema: {path}")
    sources = payload.get("sources")
    if not isinstance(sources, list) or len(sources) != 2:
        raise ArtifactContractError(f"log-offset artifact must contain two sources: {path}")
    roles: list[str] = []
    for source in sources:
        if not isinstance(source, dict):
            raise ArtifactContractError(f"malformed log source in {path}")
        role = source.get("role")
        roles.append(role if isinstance(role, str) else "")
        if not isinstance(source.get("path"), str) or not source["path"]:
            raise ArtifactContractError(f"log source path is missing in {path}")
        if not isinstance(source.get("existed"), bool):
            raise ArtifactContractError(f"log source existed flag is invalid in {path}")
        _require_nonnegative_integer(source.get("length_bytes"), "log length_bytes")
        if source["existed"]:
            _require_nonnegative_integer(source.get("device"), "log device")
            _require_nonnegative_integer(source.get("inode"), "log inode")
        elif source.get("device") is not None or source.get("inode") is not None:
            raise ArtifactContractError(f"absent log source has file identity in {path}")
        if not isinstance(source.get("prefix_sha256"), str) or re.fullmatch(
            r"[0-9a-f]{64}", source["prefix_sha256"]
        ) is None:
            raise ArtifactContractError(f"log source prefix hash is invalid in {path}")
    if roles != ["postgres_log", "panic_log"]:
        raise ArtifactContractError(f"log-offset roles are missing or reordered: {path}")
    return payload


def audit_log_deltas(snapshot_path: Path, output_path: Path, excerpt_path: Path) -> None:
    snapshot = load_log_offsets(snapshot_path)
    failures: list[str] = []
    audited_sources: list[dict[str, Any]] = []
    excerpts: list[bytes] = []

    for source in snapshot["sources"]:
        role = source["role"]
        path = Path(source["path"])
        start = source["length_bytes"]
        digest = hashlib.sha256()
        delta_bytes = 0
        line_count = 0
        match_count = 0
        match_samples: list[dict[str, Any]] = []
        tail = b""

        if path.is_symlink() or (path.exists() and not path.is_file()):
            failures.append(f"{role} path is no longer a regular file: {path}")
            current = 0
        elif not path.exists():
            current = 0
            if source["existed"]:
                failures.append(f"{role} disappeared after the audit started: {path}")
        else:
            try:
                handle = path.open("rb")
            except OSError as error:
                raise ArtifactContractError(f"cannot open audited log {path}: {error}") from error
            with handle:
                opened = os.fstat(handle.fileno())
                current = opened.st_size
                if source["existed"] and (
                    opened.st_dev != source["device"] or opened.st_ino != source["inode"]
                ):
                    failures.append(f"{role} file identity changed after audit start: {path}")
                if current < start:
                    failures.append(
                        f"{role} shrank after the audit started ({current} < {start} bytes): {path}"
                    )
                prefix_bytes = min(start, current)
                prefix_digest = hashlib.sha256()
                remaining = prefix_bytes
                while remaining:
                    chunk = handle.read(min(64 * 1024, remaining))
                    if not chunk:
                        break
                    prefix_digest.update(chunk)
                    remaining -= len(chunk)
                if remaining or prefix_bytes != start or prefix_digest.hexdigest() != source["prefix_sha256"]:
                    failures.append(f"{role} prefix changed after audit start: {path}")

                expected_delta = current - start if current >= start else current
                handle.seek(start if current >= start else 0)
                remaining = expected_delta
                pending = b""
                while remaining:
                    chunk = handle.read(min(64 * 1024, remaining))
                    if not chunk:
                        failures.append(f"{role} ended before its captured end offset: {path}")
                        break
                    remaining -= len(chunk)
                    delta_bytes += len(chunk)
                    digest.update(chunk)
                    tail = (tail + chunk)[-LOG_EXCERPT_BYTES:]
                    pending += chunk
                    lines = pending.splitlines(keepends=True)
                    if lines and not lines[-1].endswith((b"\n", b"\r")):
                        pending = lines.pop()
                    else:
                        pending = b""
                    for raw_line in lines:
                        line_count += 1
                        if LOG_FAILURE_PATTERN.search(raw_line):
                            match_count += 1
                            if len(match_samples) < 20:
                                match_samples.append({
                                    "line": line_count,
                                    "text": raw_line.decode("utf-8", errors="replace").rstrip("\r\n")[:512],
                                })
                if pending:
                    line_count += 1
                    if LOG_FAILURE_PATTERN.search(pending):
                        match_count += 1
                        if len(match_samples) < 20:
                            match_samples.append({
                                "line": line_count,
                                "text": pending.decode("utf-8", errors="replace")[:512],
                            })
                closed = os.fstat(handle.fileno())
                if closed.st_size != current or closed.st_dev != opened.st_dev or closed.st_ino != opened.st_ino:
                    failures.append(f"{role} changed while its bounded audit was running: {path}")
                if delta_bytes != expected_delta:
                    failures.append(
                        f"{role} audit read {delta_bytes} bytes, expected exactly {expected_delta}: {path}"
                    )

        if match_count:
            failures.append(f"{role} contains {match_count} crash/panic/resource pattern(s)")
        if role == "panic_log" and delta_bytes:
            failures.append(f"panic log received {delta_bytes} byte(s) during Metal stress")

        audited_sources.append(
            {
                "capture_end_offset_bytes": current,
                "delta_bytes_scanned": delta_bytes,
                "delta_lines_scanned": line_count,
                "delta_sha256": digest.hexdigest(),
                "match_count": match_count,
                "match_samples": match_samples,
                "path": str(path),
                "role": role,
                "run_start_offset_bytes": start,
            }
        )
        header = (
            f"source={path}\nrole={role}\nrun_start_offset_bytes={start}\n"
            f"capture_end_offset_bytes={current}\ndelta_bytes_scanned={delta_bytes}\n"
            f"delta_lines_scanned={line_count}\n--- bounded final delta excerpt ---\n"
        ).encode("utf-8")
        excerpts.extend([header, tail, b"\n"])

    payload = {
        "artifact_type": "metal_stress_log_audit",
        "failures": failures,
        "schema_version": SCHEMA_VERSION,
        "sources": audited_sources,
        "status": "PASS" if not failures else "FAIL",
    }
    _write_json(output_path, payload)
    excerpt_path.parent.mkdir(parents=True, exist_ok=True)
    excerpt_path.write_bytes(b"".join(excerpts))
    if failures:
        raise ArtifactContractError("Metal stress log audit failed: " + "; ".join(failures))


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


def _artifact_evidence(
    artifact_dir: Path,
    relative: str,
    role: str,
    *,
    require_nonempty: bool,
) -> dict[str, Any]:
    path = artifact_dir / relative
    if path.is_symlink() or not path.is_file():
        raise ArtifactContractError(
            f"required Metal stress artifact is missing or empty/non-regular: {path}"
        )
    before = path.stat()
    if require_nonempty and before.st_size == 0:
        raise ArtifactContractError(f"required Metal stress artifact is empty: {path}")
    raw = path.read_bytes()
    after = path.stat()
    if (
        before.st_dev != after.st_dev
        or before.st_ino != after.st_ino
        or before.st_size != after.st_size
        or before.st_mtime_ns != after.st_mtime_ns
        or len(raw) != after.st_size
    ):
        raise ArtifactContractError(f"Metal stress artifact changed while hashing: {path}")
    return {
        "path": relative,
        "required": True,
        "role": role,
        "sha256": hashlib.sha256(raw).hexdigest(),
        "size_bytes": len(raw),
    }


def _safe_nested_path(index_path: Path, child: Any, seen: set[str]) -> str:
    if not isinstance(child, str) or not child or "\\" in child:
        raise ArtifactContractError(f"unsafe benchmark artifact path in {index_path}")
    candidate = Path(child)
    if (
        candidate.is_absolute()
        or any(part in ("", ".", "..") for part in candidate.parts)
        or child in seen
        or child in ("artifact_index.json", "artifact_checklist.md")
    ):
        raise ArtifactContractError(
            f"unsafe/duplicate benchmark artifact path in {index_path}: {child}"
        )
    seen.add(child)
    return child


def _benchmark_artifact_evidence(
    artifact_dir: Path, benchmark_dir: str
) -> list[dict[str, Any]]:
    benchmark_root = artifact_dir / benchmark_dir
    relative_index = f"{benchmark_dir}/artifact_index.json"
    index_path = artifact_dir / relative_index
    if index_path.is_symlink() or not index_path.is_file():
        raise ArtifactContractError(
            f"benchmark artifact index is missing or not a regular file: {index_path}"
        )
    try:
        nested = json.loads(index_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArtifactContractError(
            f"invalid benchmark artifact index {index_path}: {error}"
        ) from error
    if not isinstance(nested, dict) or nested.get("schema_version") != 1:
        raise ArtifactContractError(f"benchmark artifact index has wrong schema: {index_path}")
    entries = nested.get("entries")
    if not isinstance(entries, list) or not entries:
        raise ArtifactContractError(f"benchmark artifact index has no entries: {index_path}")
    if nested.get("entry_count") != len(entries):
        raise ArtifactContractError(
            f"benchmark artifact index entry_count mismatch: {index_path}"
        )

    seen: set[str] = set()
    indexed_sizes: dict[str, int] = {}
    total = 0
    for entry in entries:
        if not isinstance(entry, dict):
            raise ArtifactContractError(f"malformed benchmark artifact entry: {index_path}")
        child = _safe_nested_path(index_path, entry.get("path"), seen)
        size = _require_nonnegative_integer(
            entry.get("size_bytes"), f"{relative_index}:{child}.size_bytes"
        )
        child_path = benchmark_root / child
        if child_path.is_symlink() or not child_path.is_file() or child_path.stat().st_size != size:
            raise ArtifactContractError(
                f"benchmark artifact size/path contradicts nested index: {child_path}"
            )
        indexed_sizes[child] = size
        total += size
    if nested.get("total_size_bytes") != total:
        raise ArtifactContractError(
            f"benchmark artifact index total size mismatch: {index_path}"
        )
    required_nested = {
        "manifest.json",
        "crashes.json",
        "report.json",
        "report.md",
        "report.csv",
    }
    if not required_nested.issubset(seen) or not any(
        child.startswith("correctness_diffs/") for child in seen
    ):
        raise ArtifactContractError(f"benchmark artifact index is incomplete: {index_path}")
    if load_crash_count(benchmark_root / "crashes.json") != 0:
        raise ArtifactContractError(f"benchmark artifact records crashes: {index_path}")

    actual: set[str] = set()
    for path in benchmark_root.rglob("*"):
        if path.is_symlink():
            raise ArtifactContractError(f"benchmark artifact tree contains a symlink: {path}")
        if path.is_file():
            actual.add(path.relative_to(benchmark_root).as_posix())
    self_generated = {"artifact_index.json"}
    if (benchmark_root / "artifact_checklist.md").is_file():
        self_generated.add("artifact_checklist.md")
    if actual != seen | self_generated:
        missing = sorted((seen | self_generated) - actual)
        unindexed = sorted(actual - (seen | self_generated))
        raise ArtifactContractError(
            f"benchmark artifact inventory mismatch in {index_path}: "
            f"missing={missing} unindexed={unindexed}"
        )

    artifacts = [
        _artifact_evidence(
            artifact_dir,
            relative_index,
            "benchmark_artifact_index",
            require_nonempty=True,
        )
    ]
    for child in sorted(seen):
        evidence = _artifact_evidence(
            artifact_dir,
            f"{benchmark_dir}/{child}",
            "benchmark_artifact",
            require_nonempty=False,
        )
        if evidence["size_bytes"] != indexed_sizes[child]:
            raise ArtifactContractError(
                f"benchmark artifact changed after nested index validation: "
                f"{benchmark_root / child}"
            )
        artifacts.append(evidence)
    if "artifact_checklist.md" in self_generated:
        artifacts.append(
            _artifact_evidence(
                artifact_dir,
                f"{benchmark_dir}/artifact_checklist.md",
                "benchmark_artifact_checklist",
                require_nonempty=True,
            )
        )
    return artifacts


def _collect_root_artifacts(artifact_dir: Path) -> list[dict[str, Any]]:
    artifacts: list[dict[str, Any]] = []
    for relative, role in CORE_ARTIFACTS:
        artifacts.append(
            _artifact_evidence(
                artifact_dir, relative, role, require_nonempty=True
            )
        )
    load_candidate_provenance(artifact_dir / "candidate-provenance.json")
    for benchmark_dir in BENCHMARK_DIRS:
        artifacts.extend(_benchmark_artifact_evidence(artifact_dir, benchmark_dir))
    return artifacts


def verify_artifact_index(artifact_dir: Path) -> None:
    index_path = artifact_dir / "artifact_index.json"
    try:
        payload = json.loads(index_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ArtifactContractError(f"cannot read Metal stress artifact index: {error}") from error
    if (
        not isinstance(payload, dict)
        or payload.get("artifact_type") != "metal_stress_artifact_index"
        or payload.get("schema_version") != SCHEMA_VERSION
        or not isinstance(payload.get("artifacts"), list)
    ):
        raise ArtifactContractError(f"Metal stress artifact index has the wrong schema: {index_path}")
    expected = _collect_root_artifacts(artifact_dir)
    if payload["artifacts"] != expected:
        raise ArtifactContractError(
            "Metal stress artifact index contradicts current path, size, or sha256 evidence"
        )


def write_artifact_index(artifact_dir: Path) -> None:
    artifacts = _collect_root_artifacts(artifact_dir)
    payload = {
        "artifact_type": "metal_stress_artifact_index",
        "artifacts": artifacts,
        "schema_version": SCHEMA_VERSION,
    }
    _write_json(artifact_dir / "artifact_index.json", payload)
    verify_artifact_index(artifact_dir)


def _workflow_job_block(workflow: str, job: str) -> str:
    match = re.search(
        rf"^  {re.escape(job)}:\s*$\n(?P<body>(?:^(?:    .*|\s*)$\n?)*)",
        workflow,
        re.MULTILINE,
    )
    if match is None:
        raise ArtifactContractError(f"workflow is missing the `{job}` job")
    return match.group(0)


def _workflow_step(job_block: str, name: str) -> str:
    lines = job_block.splitlines()
    marker = f"      - name: {name}"
    indexes = [index for index, line in enumerate(lines) if line == marker]
    if len(indexes) != 1:
        raise ArtifactContractError(f"workflow requires exactly one `{name}` step")
    start = indexes[0]
    end = next(
        (index for index in range(start + 1, len(lines)) if lines[index].startswith("      - ")),
        len(lines),
    )
    return "\n".join(lines[start:end])


def _workflow_step_lines(step: str) -> set[str]:
    return {
        line.strip()
        for line in step.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }


def _workflow_step_commands(step: str, label: str) -> list[str]:
    lines = step.splitlines()
    run_indexes = [
        index for index, line in enumerate(lines) if line.strip() == "run: |"
    ]
    if len(run_indexes) != 1:
        raise ArtifactContractError(
            f"{label} workflow step requires exactly one literal run block"
        )
    return [
        line.strip()
        for line in lines[run_indexes[0] + 1 :]
        if line.strip() and not line.lstrip().startswith("#")
    ]


def _validate_terminal_workflow_step(
    job: str,
    name: str,
    expected_commands: list[str],
    required_lines: tuple[str, ...] = (),
) -> None:
    step = _workflow_step(job, name)
    lines = _workflow_step_lines(step)
    for required in ("shell: bash", "run: |", *required_lines):
        if required not in lines:
            raise ArtifactContractError(
                f"{name} workflow step is missing `{required}`"
            )
    if any(line.startswith(("if:", "continue-on-error:")) for line in lines):
        raise ArtifactContractError(
            f"{name} workflow step cannot be conditional or continue on error"
        )
    commands = _workflow_step_commands(step, name)
    if commands != expected_commands:
        raise ArtifactContractError(
            f"{name} workflow run block must contain only its strict terminal commands"
        )


def _validate_workflow_upload(
    job: str, name: str, artifact_dir: str
) -> None:
    upload = _workflow_step(job, name)
    lines = _workflow_step_lines(upload)
    for required in (
        "if: always()",
        "uses: actions/upload-artifact@v4",
        "with:",
        f"path: {artifact_dir}",
        "if-no-files-found: error",
    ):
        if required not in lines:
            raise ArtifactContractError(
                f"{name} workflow upload is missing `{required}`"
            )
    if any(line.startswith("continue-on-error:") for line in lines):
        raise ArtifactContractError(
            f"{name} workflow upload cannot continue on error"
        )


def _validate_qualified_metal_job(
    workflow: str,
    job_name: str,
    coverage_step: str,
    upload_steps: tuple[str, str, str, str, str],
) -> str:
    metal = _workflow_job_block(workflow, job_name)
    runs_on = re.findall(r"^    runs-on:\s*(.+?)\s*$", metal, re.MULTILINE)
    if runs_on != ["macos-26"]:
        raise ArtifactContractError(
            f"qualified Metal job must run on the hosted Apple Silicon `macos-26` label, found {runs_on!r}"
        )
    if re.search(r"^    (?:if|continue-on-error)\s*:", metal, re.MULTILINE):
        raise ArtifactContractError(
            "qualified Metal workflow job cannot be conditional or continue on error"
        )
    if not re.search(r"^    timeout-minutes:\s*360\s*$", metal, re.MULTILINE):
        raise ArtifactContractError(
            "qualified Metal workflow job requires the hosted-runner 360-minute timeout"
        )

    coverage_dir = "artifacts/coverage-pg18-qualified-metal"
    benchmark_dir = "artifacts/benchmark-ship-gate-pg18-qualified-metal"
    system_dir = "artifacts/system-workload-gate-pg18-qualified-metal"
    stress_dir = "artifacts/metal-stress-pg18-qualified-metal"
    parity_dir = "artifacts/native-parity-p0-pg18-qualified-metal"
    gate_specs = (
        (
            coverage_step,
            [
                "set -euo pipefail",
                'CPP_COVERAGE_LLVM_PREFIX="$(brew --prefix llvm@20)" just coverage 18',
            ],
            (
                "env:",
                f"COVERAGE_ARTIFACT_DIR: {coverage_dir}",
                "COVERAGE_MIN_RUST_LINES: 90",
                "COVERAGE_MIN_CPP_LINES: 90",
                "COVERAGE_MIN_SQL_ASSERTIONS: 90",
            ),
        ),
        (
            "Remove coverage build intermediates",
            ["set -euo pipefail", "rm -rf target/coverage"],
            (),
        ),
        (
            "Run live Metal benchmark ship gate",
            [
                "set -euo pipefail",
                f"just metal-benchmark-ship-gate 18 {benchmark_dir}",
            ],
            (),
        ),
        (
            "Run broad system workload characterization",
            [
                "set -euo pipefail",
                f"just system-workload-gate 18 {system_dir}",
            ],
            (),
        ),
        (
            "Run live Metal stress gate",
            ["set -euo pipefail", "just metal-stress 18"],
            ("env:", f"METAL_STRESS_ARTIFACT_DIR: {stress_dir}"),
        ),
        (
            "Run native-decline parity gate",
            [
                "set -euo pipefail",
                'just native-parity-p0 "$NATIVE_PARITY_ARTIFACT_DIR" "postgresql://localhost:28818/postgres" 18',
            ],
            ("env:", f"NATIVE_PARITY_ARTIFACT_DIR: {parity_dir}"),
        ),
    )
    for name, commands, required_lines in gate_specs:
        _validate_terminal_workflow_step(
            metal, name, commands, required_lines
        )

    artifact_dirs = (
        coverage_dir,
        benchmark_dir,
        system_dir,
        stress_dir,
        parity_dir,
    )
    for upload_name, artifact_dir in zip(upload_steps, artifact_dirs, strict=True):
        _validate_workflow_upload(metal, upload_name, artifact_dir)

    gate_markers = [f"      - name: {spec[0]}" for spec in gate_specs]
    upload_markers = [f"      - name: {name}" for name in upload_steps]
    indexes = [metal.index(marker) for marker in (*gate_markers, *upload_markers)]
    if indexes != sorted(indexes):
        raise ArtifactContractError(
            "qualified Metal gates and durable uploads are not in the required order"
        )
    return metal


def validate_ci_workflow_contract(workflow: str) -> None:
    mac = _workflow_job_block(workflow, "mac-arm64")
    if not re.search(r"^    runs-on:\s*macos-26\s*$", mac, re.MULTILINE):
        raise ArtifactContractError(
            "macOS arm64 compatibility jobs must use the Apple Silicon `macos-26` label"
        )
    _validate_qualified_metal_job(
        workflow,
        "metal-release-gates",
        "Run three-layer release coverage gate",
        (
            "Upload three-layer coverage artifacts",
            "Upload Metal benchmark ship-gate artifacts",
            "Upload broad system workload artifacts",
            "Upload Metal stress artifacts",
            "Upload native-decline parity artifacts",
        ),
    )
    linux = _workflow_job_block(workflow, "linux-x86")
    build_marker = "      - name: Build pinned AdaptiveCpp generic toolchain"
    audit_marker = "      - name: Run CPU-cheat analyzer and ABI integrity gate"
    if build_marker not in linux or audit_marker not in linux:
        raise ArtifactContractError(
            "Linux workflow requires both AdaptiveCpp setup and the CPU-cheat analyzer"
        )
    if linux.index(audit_marker) < linux.index(build_marker):
        raise ArtifactContractError(
            "Linux CPU-cheat analysis must run after AdaptiveCpp headers are available"
        )
    if "libclang-dev" not in linux:
        raise ArtifactContractError(
            "Linux AdaptiveCpp setup requires the Clang development headers"
        )


def validate_release_workflow_contract(workflow: str) -> None:
    _validate_qualified_metal_job(
        workflow,
        "metal-coverage",
        "Run release coverage gate",
        (
            "Upload release coverage artifacts",
            "Upload release Metal benchmark ship-gate artifacts",
            "Upload release broad system workload artifacts",
            "Upload release Metal stress artifacts",
            "Upload release native-decline parity artifacts",
        ),
    )
    release = _workflow_job_block(workflow, "release")
    needs = re.search(r"^    needs:\s*\[([^]]+)\]\s*$", release, re.MULTILINE)
    if needs is None or "metal-coverage" not in {
        dependency.strip() for dependency in needs.group(1).split(",")
    }:
        raise ArtifactContractError(
            "release publication does not depend on the qualified Metal gate job"
        )
    if re.search(r"^    if\s*:", release, re.MULTILINE):
        raise ArtifactContractError(
            "release publication cannot override dependency success with a job condition"
        )
    if re.search(r"^    continue-on-error\s*:", release, re.MULTILINE):
        raise ArtifactContractError("release publication cannot continue on error")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    snapshot = subparsers.add_parser(
        "snapshot", help="measure the AdaptiveCpp Metal cache"
    )
    snapshot.add_argument("--point", required=True)
    snapshot.add_argument("--output", required=True, type=Path)
    snapshot.add_argument("--cache-dir")

    candidate = subparsers.add_parser(
        "capture-candidate",
        help="require and capture an exact clean candidate checkout",
    )
    candidate.add_argument("--repo-root", required=True, type=Path)
    candidate.add_argument("--output", required=True, type=Path)

    crash_count = subparsers.add_parser(
        "crash-count", help="validate and count a benchmark crash artifact"
    )
    crash_count.add_argument("--path", required=True, type=Path)

    log_snapshot = subparsers.add_parser(
        "log-snapshot", help="capture run-start offsets for the Metal stress logs"
    )
    log_snapshot.add_argument("--postgres-log", required=True, type=Path)
    log_snapshot.add_argument("--panic-log", required=True, type=Path)
    log_snapshot.add_argument("--output", required=True, type=Path)

    log_audit = subparsers.add_parser(
        "log-audit", help="scan the complete run-bound Metal stress log deltas"
    )
    log_audit.add_argument("--snapshot", required=True, type=Path)
    log_audit.add_argument("--output", required=True, type=Path)
    log_audit.add_argument("--excerpt", required=True, type=Path)

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

    verify_index = subparsers.add_parser(
        "verify-index", help="verify a sealed Metal stress artifact index"
    )
    verify_index.add_argument("--artifact-dir", required=True, type=Path)

    workflow = subparsers.add_parser(
        "workflow-audit", help="validate qualified Metal workflow wiring"
    )
    workflow.add_argument("--path", required=True, type=Path)
    workflow.add_argument(
        "--kind", choices=("release", "ci"), default="release"
    )
    return parser


def main(argv: list[str] | None = None, stderr: TextIO = sys.stderr) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "snapshot":
            cache_dir = resolve_cache_dir(args.cache_dir)
            _write_json(args.output, measure_cache(cache_dir, args.point))
        elif args.command == "capture-candidate":
            payload = capture_candidate_provenance(args.repo_root)
            _write_json(args.output, payload)
            print(f"commit={payload['commit']}")
            print(f"tree={payload['tree']}")
        elif args.command == "crash-count":
            print(load_crash_count(args.path))
        elif args.command == "log-snapshot":
            _write_json(
                args.output,
                snapshot_log_offsets(args.postgres_log, args.panic_log),
            )
        elif args.command == "log-audit":
            audit_log_deltas(args.snapshot, args.output, args.excerpt)
        elif args.command == "finalize":
            finalize_artifacts(
                args.before, args.after, args.archive_log, args.output_dir
            )
        elif args.command == "index":
            write_artifact_index(args.artifact_dir)
        elif args.command == "verify-index":
            verify_artifact_index(args.artifact_dir)
        else:
            workflow = args.path.read_text(encoding="utf-8")
            if args.kind == "ci":
                validate_ci_workflow_contract(workflow)
            else:
                validate_release_workflow_contract(workflow)
    except ArtifactContractError as error:
        print(f"error: {error}", file=stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
