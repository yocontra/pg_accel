#!/usr/bin/env python3
"""Focused tests for the Metal stress artifact contract."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import subprocess
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


def candidate_provenance_fixture() -> dict[str, object]:
    digest = hashlib.sha256(b"fixture\n").hexdigest()
    return {
        "artifact_type": "metal_stress_candidate_provenance",
        "clean_worktree": True,
        "commit": "1" * 40,
        "git_status_sha256": hashlib.sha256(b"").hexdigest(),
        "head_ref": "main",
        "repository_root": "/fixture/pg_accel",
        "schema_version": artifacts.SCHEMA_VERSION,
        "source_inputs": [
            {"path": path, "sha256": digest, "size_bytes": 8}
            for path in artifacts.CANDIDATE_SOURCE_INPUTS
        ],
        "status": "pass",
        "tree": "2" * 40,
    }


def write_core_artifacts(root: Path) -> None:
    for relative, _role in artifacts.CORE_ARTIFACTS:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        if relative == "candidate-provenance.json":
            write_json(path, candidate_provenance_fixture())
        else:
            path.write_text("fixture\n", encoding="utf-8")


def write_nested_benchmark_index(root: Path) -> None:
    children = {
        "manifest.json": b"{}\n",
        "crashes.json": b"[]\n",
        "report.json": b"{}\n",
        "report.md": b"report\n",
        "report.csv": b"report\n",
        "correctness_diffs/result.json": b"{}\n",
    }
    entries = []
    for relative, content in children.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        entries.append(
            {"path": relative, "size_bytes": len(content), "modified_unix_seconds": 1}
        )
    write_json(
        root / "artifact_index.json",
        {
            "schema_version": 1,
            "entry_count": len(entries),
            "total_size_bytes": sum(entry["size_bytes"] for entry in entries),
            "entries": entries,
        },
    )
    (root / "artifact_checklist.md").write_text("# Checklist\n", encoding="utf-8")


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
            write_core_artifacts(root)
            for benchmark_dir in artifacts.BENCHMARK_DIRS:
                write_nested_benchmark_index(root / benchmark_dir)

            artifacts.write_artifact_index(root)
            first = (root / "artifact_index.json").read_text()
            artifacts.write_artifact_index(root)
            second = (root / "artifact_index.json").read_text()

            self.assertEqual(first, second)
            payload = json.loads(first)
            paths = [entry["path"] for entry in payload["artifacts"]]
            self.assertIn("metal-stress-metrics.json", paths)
            self.assertIn("bench-h3_bulk-100000/artifact_index.json", paths)
            self.assertIn("bench-h3_bulk-100000/artifact_checklist.md", paths)
            self.assertIn("bench-h3_bulk-100000/report.json", paths)
            self.assertTrue(
                all(entry["size_bytes"] >= 0 for entry in payload["artifacts"])
            )
            self.assertTrue(
                all(len(entry["sha256"]) == 64 for entry in payload["artifacts"])
            )
            artifacts.verify_artifact_index(root)

    def test_root_index_rejects_same_size_nested_child_tamper(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_core_artifacts(root)
            for benchmark_dir in artifacts.BENCHMARK_DIRS:
                write_nested_benchmark_index(root / benchmark_dir)

            artifacts.write_artifact_index(root)
            report = root / artifacts.BENCHMARK_DIRS[0] / "report.json"
            original = report.read_bytes()
            report.write_bytes(b"X" * len(original))
            with self.assertRaisesRegex(
                artifacts.ArtifactContractError, "path, size, or sha256"
            ):
                artifacts.verify_artifact_index(root)

    def test_index_rejects_missing_required_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(
                artifacts.ArtifactContractError, "missing or empty"
            ):
                artifacts.write_artifact_index(Path(temporary))

    def test_index_rejects_schema_free_nested_benchmark_index(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_core_artifacts(root)
            for benchmark_dir in artifacts.BENCHMARK_DIRS:
                path = root / benchmark_dir / "artifact_index.json"
                path.parent.mkdir(parents=True)
                path.write_text("{}\n", encoding="utf-8")
            with self.assertRaisesRegex(artifacts.ArtifactContractError, "wrong schema"):
                artifacts.write_artifact_index(root)


class CandidateProvenanceTests(unittest.TestCase):
    def test_stress_gate_captures_clean_candidate_before_build(self) -> None:
        gate = (SCRIPT.parent / "metal_stress_gate.sh").read_text(encoding="utf-8")
        self.assertIn('run_logged "candidate-provenance"', gate)
        self.assertIn("capture-candidate", gate)
        self.assertIn('run_logged "acpp-provenance" capture_acpp_provenance', gate)
        self.assertIn('grep -Fx "acpp_head=${required_sha}"', gate)
        self.assertIn("-DCMAKE_CXX_FLAGS=-nostdinc++ -isystem", gate)
        self.assertLess(
            gate.index('run_logged "candidate-provenance"'),
            gate.index("just gpu-build"),
        )
        self.assertLess(
            gate.index('run_logged "acpp-provenance"'),
            gate.index("just gpu-build"),
        )

    def _repository(self, root: Path) -> None:
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        subprocess.run(
            ["git", "config", "user.email", "fixture@example.invalid"],
            cwd=root,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Fixture"], cwd=root, check=True
        )
        for relative in artifacts.CANDIDATE_SOURCE_INPUTS:
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"fixture {relative}\n", encoding="utf-8")
        subprocess.run(["git", "add", "."], cwd=root, check=True)
        subprocess.run(["git", "commit", "-qm", "fixture"], cwd=root, check=True)

    def test_clean_candidate_binds_commit_tree_and_review_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self._repository(root)
            payload = artifacts.capture_candidate_provenance(root)
            self.assertEqual(
                payload["commit"],
                subprocess.run(
                    ["git", "rev-parse", "HEAD"],
                    cwd=root,
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout.strip(),
            )
            self.assertEqual(
                [row["path"] for row in payload["source_inputs"]],
                list(artifacts.CANDIDATE_SOURCE_INPUTS),
            )
            artifacts.validate_candidate_provenance(payload)

    def test_dirty_or_untracked_candidate_fails_closed(self) -> None:
        for relative in ("Cargo.lock", "untracked.txt"):
            with (
                self.subTest(relative=relative),
                tempfile.TemporaryDirectory() as temporary,
            ):
                root = Path(temporary)
                self._repository(root)
                (root / relative).write_text("dirty\n", encoding="utf-8")
                with self.assertRaisesRegex(
                    artifacts.ArtifactContractError, "worktree is dirty"
                ):
                    artifacts.capture_candidate_provenance(root)


class CrashArtifactTests(unittest.TestCase):
    def test_crash_artifact_accepts_only_the_producer_list_schema(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "crashes.json"
            path.write_text("[]\n", encoding="utf-8")
            self.assertEqual(artifacts.load_crash_count(path), 0)
            for malformed in ({"crashes": []}, {"error": "writer failed"}, None, 0):
                path.write_text(json.dumps(malformed), encoding="utf-8")
                with self.assertRaisesRegex(
                    artifacts.ArtifactContractError, "unknown schema"
                ):
                    artifacts.load_crash_count(path)


class LogAuditTests(unittest.TestCase):
    def _snapshot(self, root: Path, postgres: Path, panic: Path) -> Path:
        path = root / "offsets.json"
        write_json(path, artifacts.snapshot_log_offsets(postgres, panic))
        return path

    def test_full_delta_detects_failure_before_more_than_400_benign_lines(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            postgres = root / "postgres.log"
            panic = root / "panic.log"
            postgres.write_text("before run\n", encoding="utf-8")
            panic.write_bytes(b"")
            snapshot = self._snapshot(root, postgres, panic)
            with postgres.open("a", encoding="utf-8") as handle:
                handle.write("PANIC: backend crashed\n")
                handle.writelines(f"benign {index}\n" for index in range(500))
            with self.assertRaisesRegex(artifacts.ArtifactContractError, "log audit failed"):
                artifacts.audit_log_deltas(
                    snapshot, root / "audit.json", root / "excerpt.txt"
                )
            payload = json.loads((root / "audit.json").read_text())
            self.assertEqual(payload["status"], "FAIL")
            self.assertEqual(payload["sources"][0]["match_count"], 1)
            self.assertGreater(payload["sources"][0]["delta_lines_scanned"], 400)

    def test_truncate_and_regrow_cannot_hide_changed_prefix(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            postgres = root / "postgres.log"
            panic = root / "panic.log"
            postgres.write_bytes(b"A" * 100)
            panic.write_bytes(b"")
            snapshot = self._snapshot(root, postgres, panic)
            postgres.write_bytes(b"PANIC hidden in replaced prefix\n" + b"B" * 100)
            with self.assertRaisesRegex(artifacts.ArtifactContractError, "prefix changed"):
                artifacts.audit_log_deltas(
                    snapshot, root / "audit.json", root / "excerpt.txt"
                )

    def test_replacement_identity_and_runtime_crash_signatures_fail(self) -> None:
        signatures = (
            b"kernel dispatch failed\n",
            b"server process was terminated by signal 6: Abort trap\n",
            b"all server processes terminated; reinitializing\n",
        )
        for signature in signatures:
            with self.subTest(signature=signature), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                postgres = root / "postgres.log"
                panic = root / "panic.log"
                postgres.write_bytes(b"")
                panic.write_bytes(b"")
                snapshot = self._snapshot(root, postgres, panic)
                postgres.write_bytes(signature)
                with self.assertRaisesRegex(artifacts.ArtifactContractError, "log audit failed"):
                    artifacts.audit_log_deltas(
                        snapshot, root / "audit.json", root / "excerpt.txt"
                    )

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            postgres = root / "postgres.log"
            panic = root / "panic.log"
            postgres.write_bytes(b"stable prefix\n")
            panic.write_bytes(b"")
            snapshot = self._snapshot(root, postgres, panic)
            postgres.unlink()
            postgres.write_bytes(b"stable prefix\nbenign\n")
            with self.assertRaisesRegex(artifacts.ArtifactContractError, "identity changed"):
                artifacts.audit_log_deltas(
                    snapshot, root / "audit.json", root / "excerpt.txt"
                )


class ReleaseWorkflowTests(unittest.TestCase):
    def test_release_workflow_requires_live_gate_upload_and_dependency(self) -> None:
        workflow_path = SCRIPT.parents[1] / ".github/workflows/release.yml"
        workflow = workflow_path.read_text(encoding="utf-8")
        artifacts.validate_release_workflow_contract(workflow)

        mutants = (
            workflow.replace(
                "    runs-on: macos-26",
                "    runs-on: [self-hosted, macOS, ARM64, metal]",
                1,
            ),
            workflow.replace("          just metal-stress 18", "          # just metal-stress 18"),
            workflow.replace(
                "          just system-workload-gate 18 artifacts/system-workload-gate-pg18-qualified-metal",
                "          # just system-workload-gate 18 artifacts/system-workload-gate-pg18-qualified-metal",
            ),
            workflow.replace(
                '          just native-parity-p0 "$NATIVE_PARITY_ARTIFACT_DIR" "postgresql://localhost:28818/postgres" 18',
                '          # just native-parity-p0 "$NATIVE_PARITY_ARTIFACT_DIR" "postgresql://localhost:28818/postgres" 18',
            ),
            workflow.replace(
                "          rm -rf target/coverage",
                "          rm -rf target",
            ),
            workflow.replace(
                "          path: artifacts/metal-stress-pg18-qualified-metal",
                "          # path: artifacts/metal-stress-pg18-qualified-metal",
            ),
            workflow.replace(
                "          path: artifacts/native-parity-p0-pg18-qualified-metal",
                "          # path: artifacts/native-parity-p0-pg18-qualified-metal",
            ),
            workflow.replace(
                "needs: [build, linux-package, metal-coverage]",
                "needs: [build, linux-package]",
            ),
            workflow.replace(
                "        shell: bash\n        env:\n          METAL_STRESS_ARTIFACT_DIR",
                "        continue-on-error: true\n        shell: bash\n        env:\n          METAL_STRESS_ARTIFACT_DIR",
                1,
            ),
            workflow.replace(
                "        shell: bash\n        env:\n          METAL_STRESS_ARTIFACT_DIR",
                "        if: false\n        shell: bash\n        env:\n          METAL_STRESS_ARTIFACT_DIR",
                1,
            ),
            workflow.replace(
                "          just metal-stress 18",
                "          exit 0\n          just metal-stress 18",
                1,
            ),
            workflow.replace(
                "      - name: Upload release Metal stress artifacts\n        if: always()",
                "      - name: Upload release Metal stress artifacts\n"
                "        continue-on-error: true\n        if: always()",
                1,
            ),
            workflow.replace(
                "  metal-coverage:\n    name:",
                "  metal-coverage:\n    continue-on-error: true\n    name:",
                1,
            ),
            workflow.replace(
                "  release:\n    name:",
                "  release:\n    if: always()\n    name:",
                1,
            ),
            workflow.replace(
                "  release:\n    name:",
                "  release:\n    continue-on-error: true\n    name:",
                1,
            ),
        )
        for index, mutant in enumerate(mutants):
            with self.subTest(mutant=index):
                self.assertNotEqual(
                    mutant,
                    workflow,
                    "workflow adversarial mutation must change the baseline",
                )
                with self.assertRaises(artifacts.ArtifactContractError):
                    artifacts.validate_release_workflow_contract(mutant)

    def test_ci_workflow_requires_hosted_gates_and_toolchain_order(self) -> None:
        workflow_path = SCRIPT.parents[1] / ".github/workflows/ci.yml"
        workflow = workflow_path.read_text(encoding="utf-8")
        artifacts.validate_ci_workflow_contract(workflow)

        build_name = "      - name: Build pinned AdaptiveCpp generic toolchain"
        audit_name = "      - name: Run CPU-cheat analyzer and ABI integrity gate"
        swapped_linux_steps = (
            workflow.replace(build_name, "      - name: TEMP toolchain step", 1)
            .replace(audit_name, build_name, 1)
            .replace("      - name: TEMP toolchain step", audit_name, 1)
        )
        qualified_header = (
            "  metal-release-gates:\n"
            "    name: Qualified Metal release gates (PG 18)\n"
            "    runs-on: macos-26"
        )
        mutants = (
            workflow.replace("    runs-on: macos-26", "    runs-on: macos-14", 1),
            workflow.replace(
                qualified_header,
                qualified_header.replace(
                    "runs-on: macos-26",
                    "runs-on: [self-hosted, macOS, ARM64, metal]",
                ),
                1,
            ),
            workflow.replace(
                "          just metal-stress 18",
                "          # just metal-stress 18",
                1,
            ),
            workflow.replace(
                '          just native-parity-p0 "$NATIVE_PARITY_ARTIFACT_DIR" "postgresql://localhost:28818/postgres" 18',
                '          # just native-parity-p0 "$NATIVE_PARITY_ARTIFACT_DIR" "postgresql://localhost:28818/postgres" 18',
                1,
            ),
            workflow.replace(
                "          path: artifacts/native-parity-p0-pg18-qualified-metal",
                "          # path: artifacts/native-parity-p0-pg18-qualified-metal",
                1,
            ),
            workflow.replace(
                "          rm -rf target/coverage",
                "          rm -rf target",
                1,
            ),
            workflow.replace("            libclang-dev \\\n", "", 1),
            swapped_linux_steps,
        )
        for index, mutant in enumerate(mutants):
            with self.subTest(mutant=index):
                self.assertNotEqual(
                    mutant,
                    workflow,
                    "workflow adversarial mutation must change the baseline",
                )
                with self.assertRaises(artifacts.ArtifactContractError):
                    artifacts.validate_ci_workflow_contract(mutant)


if __name__ == "__main__":
    unittest.main()
