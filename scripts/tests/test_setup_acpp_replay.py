"""Run the real setup patch dispatcher on isolated, locally pinned source.

ACPP_SETUP_TEST_SOURCE may name a local AdaptiveCpp repository containing the
committed .acpp-version object. No network, compiler or installation is used.
Missing local objects are a failed prerequisite, never a skipped success.
"""

from __future__ import annotations

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
TARGETS = (
    "src/compiler/sscp/TargetSeparationPass.cpp",
    "src/compiler/sscp/HostKernelNameExtractionPass.cpp",
)
ENV = {
    "PATH": os.defpath,
    "LC_ALL": "C",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_SYSTEM": os.devnull,
    "GIT_TERMINAL_PROMPT": "0",
}


def run(*args: str, cwd: Path | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(args, cwd=cwd, env=ENV, capture_output=True, check=True)


def dispatcher(script: Path) -> str:
    source = script.read_text()
    start = "apply_metal_cpp_compat_patch() {\n"
    end = "\napply_adaptivecpp_patches\n"
    if source.count(start) != 1 or source.count(end) != 1:
        raise ValueError("setup patch dispatcher extraction boundaries are not unique")
    return source[source.index(start):source.index(end) + len(end)]


class SetupReplayTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.pin = (ROOT / ".acpp-version").read_text().strip()
        cls.local_source = Path(os.environ.get(
            "ACPP_SETUP_TEST_SOURCE", ROOT / ".pgaccel/src/AdaptiveCpp"
        )).resolve(strict=True)
        cls.script = Path(os.environ.get("ACPP_SETUP_TEST_SCRIPT", ROOT / "scripts/setup_acpp.sh"))
        cls.body = dispatcher(cls.script)
        cls.patches = ROOT / "patches/adaptivecpp"
        paths = set(TARGETS)
        for patch in cls.patches.glob("*.patch"):
            paths.update(re.findall(r"^\+\+\+ b/(.+)$", patch.read_text(), re.MULTILINE))
        # Read only explicitly pinned blobs, never mutable installed sources.
        cls.baseline = {
            path: run("git", "-C", str(cls.local_source), "show", f"{cls.pin}:{path}").stdout
            for path in sorted(paths)
        }
        with tempfile.TemporaryDirectory(prefix="acpp-replay-oracle-") as temporary:
            oracle = Path(temporary)
            run("git", "init", "-q", str(oracle))
            for path in TARGETS:
                destination = oracle / path
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(cls.baseline[path])
            run("git", "apply", "--unidiff-zero", f"--include={TARGETS[0]}",
                str(cls.patches / "sscp-host-coverage.patch"), cwd=oracle)
            cls.coverage = {path: (oracle / path).read_bytes() for path in TARGETS}
            run("git", "apply", str(cls.patches / "sscp-replay-controls.patch"), cwd=oracle)
            cls.combined = {path: (oracle / path).read_bytes() for path in TARGETS}

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="acpp-setup-replay-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.repo = self.root / "project"
        self.repo.mkdir()
        shutil.copytree(self.patches, self.repo / "patches/adaptivecpp")
        # A local shared clone exposes immutable objects without copying the
        # installed working tree or invoking a network remote.
        run("git", "clone", "--shared", "--no-checkout", str(self.local_source), str(self.source))
        run("git", "-C", str(self.source), "read-tree", self.pin)
        run("git", "-C", str(self.source), "checkout-index", "--", *self.baseline)

    def invoke(self, *, success=True):
        env = dict(ENV, PG_ACCEL_REPO_ROOT=str(self.repo),
                   PG_ACCEL_TOOL_ROOT=str(self.root / "unused-tools"),
                   ACPP_SRC=str(self.source), ACPP_REQUIRED_SHA=self.pin,
                   backend="generic", TMPDIR=str(self.root))
        result = subprocess.run(
            ["bash", "-c", "set -euo pipefail\n" + self.body + "\necho DISPATCH_COMPLETE\n"],
            cwd=self.root, env=env, capture_output=True, text=True, timeout=30,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("DISPATCH_COMPLETE", result.stdout)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertNotIn("DISPATCH_COMPLETE", result.stdout)
        self.assertFalse((self.root / "unused-tools").exists())
        self.assertEqual(list(self.root.glob("pgaccel-acpp-patches.*")), [])
        return result

    def target_bytes(self):
        return {path: (self.source / path).read_bytes() for path in TARGETS
                if (self.source / path).exists()}

    def write_targets(self, contents):
        for path, data in contents.items():
            (self.source / path).write_bytes(data)

    def assert_rejected_unchanged(self):
        before = self.target_bytes()
        self.invoke(success=False)
        self.assertEqual(self.target_bytes(), before)

    def test_fresh_and_repeated_full_dispatcher(self):
        self.invoke()
        self.assertEqual(self.target_bytes(), self.combined)
        all_bytes = {path: (self.source / path).read_bytes() for path in self.baseline}
        self.invoke()
        self.assertEqual({path: (self.source / path).read_bytes() for path in self.baseline}, all_bytes)

    def test_coverage_only_upgrade(self):
        self.invoke()
        self.write_targets(self.coverage)
        self.invoke()
        self.assertEqual(self.target_bytes(), self.combined)

    def test_partial_replay_target_states_reject_without_repair(self):
        self.invoke()
        for partial in TARGETS:
            with self.subTest(target=partial):
                self.write_targets(self.coverage)
                (self.source / partial).write_bytes(self.combined[partial])
                self.assert_rejected_unchanged()

    def test_individual_replay_hunks_reject_without_repair(self):
        self.invoke()
        patch = (self.patches / "sscp-replay-controls.patch").read_text()
        for section in re.split(r"(?=^--- a/)", patch, flags=re.MULTILINE):
            if not section:
                continue
            header, *hunks = re.split(r"(?=^@@ )", section, flags=re.MULTILINE)
            for index, hunk in enumerate(hunks):
                with self.subTest(header=header, hunk=index):
                    self.write_targets(self.coverage)
                    partial = self.root / "partial.patch"
                    partial.write_text(header + hunk)
                    run("git", "apply", str(partial), cwd=self.source)
                    self.assert_rejected_unchanged()

    def test_in_hunk_and_out_of_hunk_drift_reject(self):
        self.invoke()
        for target in TARGETS:
            baseline = {path: self.baseline[path] for path in TARGETS}
            for state in (baseline, self.coverage, self.combined):
                for drift in (b"// unrelated trailing drift\n", None):
                    with self.subTest(target=target, combined=state is self.combined, drift=drift):
                        self.write_targets(state)
                        data = state[target]
                        if drift is None:
                            needle = (b"std::size_t HcfObjectId =" if target == TARGETS[0]
                                      else b"SSCPKernelNameExtractionFunctions")
                            self.assertIn(needle, data)
                            data = data.replace(needle, needle + b"Drift", 1)
                        else:
                            data += drift
                        (self.source / target).write_bytes(data)
                        self.assert_rejected_unchanged()

    def test_missing_patch_rejects_before_target_mutation(self):
        for name in ("sscp-host-coverage.patch", "sscp-replay-controls.patch"):
            with self.subTest(patch=name):
                patch = self.repo / "patches/adaptivecpp" / name
                original = patch.read_bytes()
                patch.unlink()
                self.assert_rejected_unchanged()
                patch.write_bytes(original)

    def test_missing_target_rejects_before_other_target_mutation(self):
        for target in TARGETS:
            with self.subTest(target=target):
                path = self.source / target
                original = path.read_bytes()
                path.unlink()
                self.assert_rejected_unchanged()
                path.write_bytes(original)

    def test_missing_pinned_blob_rejects_before_target_mutation(self):
        self.pin = "0" * 40
        self.assert_rejected_unchanged()

    def test_other_coverage_target_drift_still_rejects(self):
        self.invoke()
        target = self.source / "src/runtime/metal/metal_queue.cpp"
        original = target.read_bytes()
        self.assertIn(b"#include <limits>", original)
        target.write_bytes(original.replace(b"#include <limits>", b"#include <not_limits>", 1))
        self.assert_rejected_unchanged()


if __name__ == "__main__":
    unittest.main()
