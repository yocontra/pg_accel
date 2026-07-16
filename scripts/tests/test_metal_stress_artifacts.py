#!/usr/bin/env python3
"""Focused tests for the Metal stress artifact contract."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "metal_stress_artifacts.py"
SPEC = importlib.util.spec_from_file_location("metal_stress_artifacts", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
artifacts = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = artifacts
SPEC.loader.exec_module(artifacts)


def write_json(path: Path, payload: dict[str, object]) -> None:
    path.write_text(json.dumps(payload), encoding="utf-8")


def latency_record(worker: int, warm_iterations: int = 2) -> str:
    offset = worker * 10
    values = {
        "worker": worker,
        "init_us": 50 + offset,
        "cold_iteration_us": 600 + offset,
        "cold_reduce_us": 100 + offset,
        "cold_h3_us": 200 + offset,
        "cold_pip_us": 300 + offset,
        "warm_iterations": warm_iterations,
        "warm_iteration_total_us": 300 + offset,
        "warm_iteration_max_us": 160 + offset,
        "warm_reduce_total_us": 40 + offset,
        "warm_reduce_max_us": 25 + offset,
        "warm_h3_total_us": 100 + offset,
        "warm_h3_max_us": 60 + offset,
        "warm_pip_total_us": 140 + offset,
        "warm_pip_max_us": 80 + offset,
        "wall_us": 1000 + offset,
    }
    return "latency_record_us " + " ".join(
        f"{key}={value}" for key, value in values.items()
    )


def valid_archive_log(
    records: list[str] | None = None,
    cache_dir: Path | str = "/tmp/metal-stress-fixture",
) -> str:
    latency_records = (
        records if records is not None else [latency_record(0), latency_record(1)]
    )
    lines = [
        "=== Metal MTLBinaryArchive fork stress test ===",
        "workers=2 iterations_per_worker=3 total_dispatches=18",
        f"jit_cache_dir={cache_dir}",
        "pre-fork archive cache: metallib=0 metalar=0 jit=0 orphan=0",
        "post-fork archive cache: metallib=1 metalar=1 jit=1 orphan=0 "
        "(delta_metallib=1 delta_metalar=1)",
        *latency_records,
        "workers_succeeded=2 / 2",
        "workers_crashed=0",
        "reports_missing=0",
        "xpc_compiler_service_hits=0",
        "pipeline_state_failures=0",
        "archive_load_failures=0",
        "archive_build_failures=0",
        "posix_spawn_failures=0",
        "cache_hash_instability_failures=0",
        "RESULT: PASS - synthetic fixture",
    ]
    body = ("\n".join(lines) + "\n").encode("utf-8")
    body_sha256 = hashlib.sha256(body).hexdigest()
    raw_lines = len(body.splitlines())
    binding = hashlib.sha256()
    binding.update(b"pgaccel-ctest-body-v1\0")
    binding.update(b"gpu-stress-archive\0")
    binding.update(b"0\0PASS\0")
    binding.update(str(raw_lines).encode("ascii"))
    binding.update(b"\0")
    binding.update(bytes.fromhex(body_sha256))
    return (
        "PGACCEL_TEST_START name=gpu-stress-archive\n"
        + body.decode("utf-8")
        + "PGACCEL_TEST_RESULT name=gpu-stress-archive exit_code=0 result=PASS "
        + f"raw_lines={raw_lines} body_sha256={body_sha256} "
        + f"binding_sha256={binding.hexdigest()}\n"
    )


class CacheMeasurementTests(unittest.TestCase):
    def test_snapshot_records_archive_jit_other_counts_and_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary) / "jit-cache"
            cache.mkdir()
            (cache / "a.metallib").write_bytes(b"lib")
            (cache / "a.metalar").write_bytes(b"ar12")
            (cache / "a.jit").write_bytes(b"jit!!")
            (cache / "kernel-index").write_bytes(b"index!")

            snapshot = artifacts.measure_cache(cache, "fixture")

            measurements = snapshot["measurements"]
            self.assertEqual(
                measurements["all_cache_files"], {"file_count": 4, "total_bytes": 18}
            )
            self.assertEqual(
                measurements["metal_binary_archive"],
                {"file_count": 2, "total_bytes": 7},
            )
            self.assertEqual(measurements["jit"], {"file_count": 1, "total_bytes": 5})
            self.assertEqual(measurements["other"], {"file_count": 1, "total_bytes": 6})

    def test_missing_cache_is_a_measured_empty_state(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary) / "missing"

            snapshot = artifacts.measure_cache(cache, "before")

            self.assertFalse(snapshot["directory_exists"])
            self.assertEqual(
                snapshot["measurements"]["all_cache_files"],
                {"file_count": 0, "total_bytes": 0},
            )

    def test_non_regular_cache_entry_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary) / "jit-cache"
            cache.mkdir()
            (cache / "unexpected-directory").mkdir()

            with self.assertRaisesRegex(artifacts.ArtifactContractError, "non-regular"):
                artifacts.measure_cache(cache, "fixture")

    def test_inconsistent_snapshot_totals_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            snapshot = artifacts.measure_cache(
                root / "missing", "before_cold_archive_stress"
            )
            snapshot["measurements"]["all_cache_files"]["file_count"] = 1
            path = root / "snapshot.json"
            write_json(path, snapshot)

            with self.assertRaisesRegex(
                artifacts.ArtifactContractError, "totals are inconsistent"
            ):
                artifacts.load_snapshot(path, "before_cold_archive_stress")


class ArchiveParserTests(unittest.TestCase):
    def test_valid_contract_emits_json_tsv_and_summary(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cache = root / "jit-cache"
            cache.mkdir()
            before = artifacts.measure_cache(cache, "before_cold_archive_stress")
            (cache / "a.metallib").write_bytes(b"metallib")
            (cache / "a.metalar").write_bytes(b"metalar")
            (cache / "a.jit").write_bytes(b"jit")
            after = artifacts.measure_cache(cache, "after_cold_archive_stress")
            before_path = root / "before.json"
            after_path = root / "after.json"
            log_path = root / "archive.log"
            write_json(before_path, before)
            write_json(after_path, after)
            log_path.write_text(valid_archive_log(cache_dir=cache), encoding="utf-8")

            artifacts.finalize_artifacts(
                before_path, after_path, log_path, root / "output"
            )
            rendered_names = (
                "metal-stress-metrics.json",
                "metal-stress-cache.tsv",
                "metal-stress-latency.tsv",
                "metal-stress-metrics-summary.txt",
            )
            first_render = {
                name: (root / "output" / name).read_bytes() for name in rendered_names
            }
            artifacts.finalize_artifacts(
                before_path, after_path, log_path, root / "output"
            )
            second_render = {
                name: (root / "output" / name).read_bytes() for name in rendered_names
            }
            self.assertEqual(first_render, second_render)

            metrics = json.loads(
                (root / "output/metal-stress-metrics.json").read_text()
            )
            summaries = metrics["latency"]["kernel_class_summaries"]
            self.assertEqual(
                [row["kernel_class"] for row in summaries],
                [row[0] for row in artifacts.KERNEL_CLASSES],
            )
            self.assertEqual(summaries[0]["cold_first_dispatch"]["max_us"], 110)
            self.assertEqual(summaries[0]["warm_cache"]["sample_count"], 4)
            self.assertEqual(
                metrics["latency"]["performance_policy"],
                "visibility_only_no_new_threshold",
            )
            latency_tsv = (root / "output/metal-stress-latency.tsv").read_text()
            self.assertIn("0\treduce_f32\t100\t2\t40\t20.000\t25\n", latency_tsv)
            cache_tsv = (root / "output/metal-stress-cache.tsv").read_text()
            self.assertIn("after_cold_archive_stress\tmetallib\t1\t8\n", cache_tsv)
            summary = (root / "output/metal-stress-metrics-summary.txt").read_text()
            self.assertIn("metal-stress artifact contract: PASS", summary)
            self.assertIn(
                "performance_policy=visibility_only_no_new_threshold", summary
            )

    def test_missing_worker_latency_record_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "archive.log"
            log.write_text(valid_archive_log([latency_record(0)]), encoding="utf-8")

            with self.assertRaisesRegex(
                artifacts.ArtifactContractError, "latency records"
            ):
                artifacts.parse_archive_log(log)

    def test_zero_warm_measurement_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "archive.log"
            broken = latency_record(0).replace(
                "warm_h3_total_us=100", "warm_h3_total_us=0"
            )
            log.write_text(
                valid_archive_log([broken, latency_record(1)]), encoding="utf-8"
            )

            with self.assertRaisesRegex(
                artifacts.ArtifactContractError, "invalid warm h3"
            ):
                artifacts.parse_archive_log(log)

    def test_duplicate_latency_field_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "archive.log"
            duplicate = latency_record(0) + " worker=0"
            log.write_text(
                valid_archive_log([duplicate, latency_record(1)]), encoding="utf-8"
            )

            with self.assertRaisesRegex(
                artifacts.ArtifactContractError, "duplicate latency field"
            ):
                artifacts.parse_archive_log(log)

    def test_tampered_raw_log_binding_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            log = Path(temporary) / "archive.log"
            tampered = valid_archive_log().replace(
                "cold_reduce_us=100", "cold_reduce_us=101", 1
            )
            log.write_text(tampered, encoding="utf-8")

            with self.assertRaisesRegex(
                artifacts.ArtifactContractError, "body hash does not match"
            ):
                artifacts.parse_archive_log(log)

    def test_snapshot_log_count_contradiction_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cache = root / "cache"
            cache.mkdir()
            before = artifacts.measure_cache(cache, "before_cold_archive_stress")
            (cache / "a.metallib").write_bytes(b"lib")
            (cache / "a.metalar").write_bytes(b"ar")
            (cache / "a.jit").write_bytes(b"jit")
            after = artifacts.measure_cache(cache, "after_cold_archive_stress")
            after = copy.deepcopy(after)
            after["measurements"]["metallib"]["file_count"] = 2
            log = root / "archive.log"
            log.write_text(valid_archive_log(cache_dir=cache), encoding="utf-8")
            parsed = artifacts.parse_archive_log(log)

            with self.assertRaisesRegex(artifacts.ArtifactContractError, "contradicts"):
                artifacts.build_metrics(before, after, parsed)


class ArtifactIndexTests(unittest.TestCase):
    def test_index_is_complete_and_deterministic(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for relative, _role in artifacts.CORE_ARTIFACTS:
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture\n", encoding="utf-8")
            for benchmark_dir in artifacts.BENCHMARK_DIRS:
                path = root / benchmark_dir / "artifact_index.json"
                path.parent.mkdir(parents=True)
                path.write_text("{}\n", encoding="utf-8")

            artifacts.write_artifact_index(root)
            first = (root / "artifact_index.json").read_text()
            artifacts.write_artifact_index(root)
            second = (root / "artifact_index.json").read_text()

            self.assertEqual(first, second)
            payload = json.loads(first)
            paths = [entry["path"] for entry in payload["artifacts"]]
            self.assertIn("metal-stress-metrics.json", paths)
            self.assertIn("bench-h3_bulk-100000/artifact_index.json", paths)

    def test_index_rejects_missing_required_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(
                artifacts.ArtifactContractError, "missing or empty"
            ):
                artifacts.write_artifact_index(Path(temporary))


if __name__ == "__main__":
    unittest.main()
