import pathlib
import os
import hashlib
import sys
import tempfile
import time
import unittest
from unittest import mock
from types import SimpleNamespace
import scripts.metadata_clang as metadata_clang

from scripts.metadata_clang import (
    ExtractionError,
    Limits,
    decode_bounded,
    run_bounded,
    include_symlink_aliases,
    snapshot_copy,
    resolve_contained,
)


@unittest.skipUnless(os.environ.get("METADATA_AST_DUMP"), "set built extractor path")
class CompilerClosureTests(unittest.TestCase):
    def extract(self, source, *, tamper=False, header=None):
        compiler = os.environ.get(
            "METADATA_CLANG", "/opt/homebrew/opt/llvm@20/bin/clang++"
        )
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "fixture.cpp").write_text(source)
            if header is not None:
                (root / "fixture.h").write_text(header)
            compiled = run_bounded(
                [
                    compiler,
                    "-std=c++17",
                    "-Werror=vla",
                    "-Wsystem-headers",
                    "-Xclang",
                    "-fvalidate-ast-input-files-content",
                    "-Xclang",
                    "-fno-pch-timestamp",
                    "-fpch-instantiate-templates",
                    "-emit-ast",
                    "fixture.cpp",
                    "-o",
                    "fixture.ast",
                ],
                root,
                root / "compile.out",
                root / "compile.err",
                artifacts=(root / "fixture.ast",),
            )
            self.assertEqual(
                compiled["returncode"], 0, (root / "compile.err").read_text()
            )
            if tamper:
                target = root / ("fixture.cpp" if header is None else "fixture.h")
                original = source if header is None else header
                original_stat = target.stat()
                changed = original.replace("return 7", "return 8")
                self.assertNotEqual(original, changed)
                self.assertEqual(len(original), len(changed))
                target.write_text(changed)
                os.utime(
                    target,
                    ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns),
                )
            result = run_bounded(
                [os.environ["METADATA_AST_DUMP"], str(root / "fixture.ast"), "entry"],
                root,
                root / "closure.json",
                root / "closure.err",
            )
            if tamper:
                self.assertEqual(
                    result["returncode"],
                    2,
                    "same-size/timestamp source mutation must reject AST",
                )
                return {}
            self.assertIn(
                result["returncode"], (0, 1), (root / "closure.err").read_text()
            )
            closure = decode_bounded(root / "closure.json", Limits())
            self.assertEqual(len(closure["serialized_inputs"]), 1)
            self.assertEqual(
                closure["serialized_inputs"][0]["sha256"],
                hashlib.sha256((root / "fixture.ast").read_bytes()).hexdigest(),
            )
            source_inputs = [
                item
                for item in closure["loaded_source_inputs"]
                if pathlib.Path(item["path"]).name == "fixture.cpp"
            ]
            self.assertEqual(len(source_inputs), 1)
            self.assertEqual(
                source_inputs[0]["sha256"], hashlib.sha256(source.encode()).hexdigest()
            )
            self.assertFalse(closure["provenance_verified"])
            return closure

    def callable_names(self, closure):
        identities = set(closure["callable"])
        return {
            item["node"].get("name")
            for item in closure["declarations"]
            if item["canonical"] in identities
        }

    def test_unused_template_method_not_callable(self):
        closure = self.extract("""
template<class T> struct R {
 T value; int unused() { return value.missing(); }
 int used() { return value; }
};
extern "C" int entry() { R<int> r{4}; return r.used(); }
""")
        self.assertIn("used", self.callable_names(closure))
        self.assertNotIn("unused", self.callable_names(closure))
        self.assertFalse(any("indirect" in x for x in closure["unresolved"]))

    def test_hidden_default_initializer_and_destructor(self):
        closure = self.extract("""
int hidden() { return 3; } void cleanup() {}
struct R { int value=hidden(); ~R() { cleanup(); } };
extern "C" int entry() { R r; return r.value; }
""")
        self.assertTrue({"hidden", "cleanup"} <= self.callable_names(closure))

    def test_later_redeclaration_body_retained(self):
        closure = self.extract("""
int helper(); extern "C" int entry() { return helper(); }
int helper() { return 7; }
""")
        helpers = [
            x for x in closure["declarations"] if x["node"].get("name") == "helper"
        ]
        self.assertGreaterEqual(len(helpers), 2)
        self.assertEqual(len({x["canonical"] for x in helpers}), 1)
        self.assertTrue(
            any(
                any(n.get("kind") == "CompoundStmt" for n in x["node"].get("inner", []))
                for x in helpers
            )
        )

    def test_missing_callee_body_blocks(self):
        closure = self.extract("""
int hidden(); extern "C" int entry() { return hidden(); }
""")
        self.assertTrue(any("no body: hidden" in x for x in closure["unresolved"]))

    def test_missing_root_body_blocks(self):
        closure = self.extract('extern "C" int entry();')
        self.assertIn("root declaration has no body", closure["unresolved"])

    def test_virtual_call_is_not_a_direct_callee_proof(self):
        closure = self.extract("""
struct R { virtual int get() { return 1; } };
extern "C" int entry(R* r) { return r->get(); }
""")
        self.assertTrue(any("virtual dispatch" in x for x in closure["unresolved"]))

    def test_array_element_destructor_dependency_retained(self):
        closure = self.extract("""
void cleanup() {} struct R { ~R() { cleanup(); } };
extern "C" int entry() { R values[2]; return 1; }
""")
        self.assertIn("cleanup", self.callable_names(closure))

    def test_same_timestamp_source_tamper_rejected(self):
        self.extract('extern "C" int entry() { return 7; }', tamper=True)

    def test_same_timestamp_included_header_tamper_rejected(self):
        self.extract(
            '#include "fixture.h"\nextern "C" int entry() { return helper(); }',
            header="inline int helper() { return 7; }",
            tamper=True,
        )

    def test_same_named_records_remain_distinct(self):
        closure = self.extract("""
namespace A { struct R { int x; }; }
namespace B { struct R { long x; }; }
extern "C" long entry(A::R* a, B::R* b) { return a->x+b->x; }
""")
        records = [
            x
            for x in closure["declarations"]
            if x["node"].get("name") == "R" and x["node"].get("completeDefinition")
        ]
        self.assertEqual(len({x["canonical"] for x in records}), 2)
        self.assertEqual(len({x["semantic_context"] for x in records}), 2)


class BoundedDiagnosticsTests(unittest.TestCase):
    def test_darwin_zombie_permission_error_checked_before_reap(self):
        for state in ("Z", "S"):
            with self.subTest(state=state), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                events = []
                child = mock.Mock(pid=12345, returncode=0)
                child.wait.side_effect = lambda **kwargs: events.append("reap")

                def inventory(*args, **kwargs):
                    events.append("inventory")
                    return SimpleNamespace(stdout=f"12345 {state}\n")

                with (
                    mock.patch.object(
                        metadata_clang.subprocess, "Popen", return_value=child
                    ),
                    mock.patch.object(
                        metadata_clang.os,
                        "waitid",
                        return_value=SimpleNamespace(si_pid=12345),
                    ),
                    mock.patch.object(
                        metadata_clang.os, "killpg", side_effect=PermissionError
                    ),
                    mock.patch.object(
                        metadata_clang.subprocess, "run", side_effect=inventory
                    ),
                ):
                    if state == "Z":
                        run_bounded(["fixture"], root, root / "out", root / "err")
                    else:
                        with self.assertRaises(PermissionError):
                            run_bounded(["fixture"], root, root / "out", root / "err")
                self.assertEqual(events, ["inventory", "reap"])

    def test_cleanup_signals_before_reap_without_popen_poll(self):
        for nested in (False, True):
            with (
                self.subTest(nested=nested),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                events = []
                child = mock.Mock(pid=12345, returncode=0)
                child.wait.side_effect = lambda **kwargs: events.append(
                    ("reap", kwargs)
                )
                with (
                    mock.patch.object(
                        metadata_clang.subprocess, "Popen", return_value=child
                    ),
                    mock.patch.object(
                        metadata_clang.os,
                        "waitid",
                        return_value=SimpleNamespace(si_pid=12345),
                    ) as observe,
                    mock.patch.object(
                        metadata_clang.os,
                        "killpg",
                        side_effect=lambda *args: events.append(("group", args)),
                    ),
                    mock.patch.object(
                        metadata_clang.os,
                        "kill",
                        side_effect=lambda *args: events.append(("child", args)),
                    ),
                    mock.patch.object(metadata_clang, "_IN_WATCHED_WORKER", nested),
                ):
                    run_bounded(["fixture"], root, root / "out", root / "err")
                self.assertEqual(
                    [event[0] for event in events],
                    ["reap"] if nested else ["group", "reap"],
                )
                self.assertEqual(events[-1][1], {"timeout": 5})
                self.assertTrue(observe.call_args.args[2] & os.WNOWAIT)
                child.poll.assert_not_called()
                child.kill.assert_not_called()
                child.send_signal.assert_not_called()

    def test_timeout_and_monitor_error_cleanup_retain_leader(self):
        for timeout in (False, True):
            with (
                self.subTest(timeout=timeout),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = pathlib.Path(directory)
                events = []
                child = mock.Mock(pid=12345, returncode=-9)
                child.wait.side_effect = lambda **kwargs: events.append("reap")
                with (
                    mock.patch.object(
                        metadata_clang.subprocess, "Popen", return_value=child
                    ),
                    mock.patch.object(metadata_clang.os, "waitid", return_value=None),
                    mock.patch.object(
                        metadata_clang.os,
                        "killpg",
                        side_effect=lambda *args: events.append("signal"),
                    ),
                    mock.patch.object(
                        metadata_clang.time,
                        "monotonic",
                        side_effect=[0, 2 if timeout else 0],
                    ),
                    mock.patch.object(
                        metadata_clang.subprocess,
                        "run",
                        side_effect=RuntimeError("monitor failed"),
                    ),
                ):
                    with self.assertRaisesRegex(
                        Exception, "deadline" if timeout else "monitor failed"
                    ):
                        run_bounded(
                            ["fixture"],
                            root,
                            root / "out",
                            root / "err",
                            Limits(seconds=1),
                        )
                self.assertEqual(events, ["signal", "reap"])
                child.poll.assert_not_called()

    def test_unexpected_reaper_never_signals_numeric_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            child = mock.Mock(pid=12345)
            with (
                mock.patch.object(
                    metadata_clang.subprocess, "Popen", return_value=child
                ),
                mock.patch.object(
                    metadata_clang.os, "waitid", side_effect=ChildProcessError
                ),
                mock.patch.object(metadata_clang.os, "killpg") as group_signal,
                mock.patch.object(metadata_clang.os, "kill") as child_signal,
            ):
                with self.assertRaisesRegex(ExtractionError, "unexpectedly reaped"):
                    run_bounded(["fixture"], root, root / "out", root / "err")
            group_signal.assert_not_called()
            child_signal.assert_not_called()
            child.wait.assert_not_called()

    def test_outer_watchdog_owns_nested_compiler_group(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            module_root = pathlib.Path(__file__).resolve().parents[2]
            code = (
                f"import sys,pathlib; sys.path.insert(0,{str(module_root)!r}); "
                "import scripts.metadata_clang as m; m._IN_WATCHED_WORKER=True; "
                "p=pathlib.Path.cwd(); "
                "m.run_bounded([sys.executable,'-c',"
                "\"import os,pathlib,time; pathlib.Path('child-group').write_text(str(os.getpgrp())); time.sleep(30)\"],"
                "p,p/'nested.out',p/'nested.err')"
            )
            with self.assertRaisesRegex(ExtractionError, "deadline"):
                run_bounded(
                    [sys.executable, "-c", code],
                    root,
                    root / "out",
                    root / "err",
                    Limits(seconds=1),
                )
            group = int((root / "child-group").read_text())
            self.assertNotEqual(group, os.getpgrp())
            # No living member remains; a killed child may briefly be a zombie
            # awaiting the operating system's orphan reaper.
            import subprocess

            snapshot = subprocess.check_output(["ps", "-axo", "pgid=,stat="], text=True)
            self.assertFalse(
                any(
                    int(parts[0]) == group and not parts[1].startswith("Z")
                    for line in snapshot.splitlines()
                    if len(parts := line.split()) == 2
                )
            )

    def test_sdk_sibling_header_and_all_hop_containment(self):
        with tempfile.TemporaryDirectory() as directory:
            base = pathlib.Path(directory).resolve()
            sdk = base / "sdk"
            includes = sdk / "usr/include"
            framework = sdk / "System/Framework/Headers"
            includes.mkdir(parents=True)
            framework.mkdir(parents=True)
            header = framework / "x.h"
            header.write_text("struct X {};")
            alias = includes / "x.h"
            alias.symlink_to(header)
            self.assertEqual(resolve_contained(alias, sdk), header)
            self.assertEqual(
                include_symlink_aliases({alias}, [includes], sdk_anchor=sdk), {alias}
            )
            outside = base / "outside"
            outside.symlink_to(header)
            alias.unlink()
            alias.symlink_to(outside)
            with self.assertRaisesRegex(ExtractionError, "escapes"):
                resolve_contained(alias, sdk)
            alias.unlink()
            nested = framework / "nested"
            nested.symlink_to(outside)
            alias.symlink_to(nested)
            with self.assertRaisesRegex(ExtractionError, "escapes"):
                resolve_contained(alias, sdk)
            nested.unlink()
            nested.symlink_to(alias)
            with self.assertRaisesRegex(ExtractionError, "cycle or hop"):
                resolve_contained(alias, sdk)
            nested.unlink()
            nested.symlink_to(header)
            with self.assertRaisesRegex(ExtractionError, "hop limit"):
                resolve_contained(alias, sdk, max_hops=1)

    def test_snapshot_copy_enforces_bytes_and_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "input"
            source.write_bytes(b"12345")
            with self.assertRaisesRegex(ExtractionError, "byte limit"):
                snapshot_copy(source, root / "too-large", 4, time.monotonic() + 2)
            self.assertLessEqual((root / "too-large").stat().st_size, 4)
            with self.assertRaisesRegex(ExtractionError, "deadline"):
                snapshot_copy(source, root / "expired", 5, time.monotonic() - 1)
            self.assertEqual(
                snapshot_copy(source, root / "exact", 5, time.monotonic() + 2), 5
            )
            self.assertEqual((root / "exact").read_bytes(), b"12345")

    def test_symlink_escape_requires_explicit_observation(self):
        with tempfile.TemporaryDirectory() as directory:
            base = pathlib.Path(directory)
            root = base / "include"
            root.mkdir()
            outside = base / "outside.h"
            outside.write_text("struct R {};")
            alias = root / "escape.h"
            alias.symlink_to(outside)
            with self.assertRaisesRegex(ExtractionError, "escapes"):
                include_symlink_aliases({alias}, [root])
            self.assertEqual(
                include_symlink_aliases({outside}, [root], observed={outside}),
                {outside, alias},
            )
            with self.assertRaisesRegex(ExtractionError, "deadline"):
                include_symlink_aliases(
                    {outside}, [root], observed={outside}, deadline=time.monotonic() - 1
                )

    def test_include_symlink_aliases_preserve_only_observed_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "real.h"
            target.write_text("struct R {};\n")
            (root / "alias.h").symlink_to(target)
            other = root / "other.h"
            other.write_text("struct S {};\n")
            (root / "not_observed.h").symlink_to(other)
            self.assertEqual(
                include_symlink_aliases({target}, [root]), {target, root / "alias.h"}
            )

    def test_invalid_deadlines_rejected_before_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for seconds in (float("nan"), float("inf"), -1, 0, 601):
                with self.subTest(seconds=seconds), self.assertRaises(ExtractionError):
                    run_bounded(
                        ["nonexistent-command"],
                        root,
                        root / "out",
                        root / "err",
                        Limits(seconds=seconds),
                    )
            self.assertFalse((root / "out").exists())

    def test_fast_writer_cannot_exceed_hard_file_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            result = run_bounded(
                [sys.executable, "-c", "import os; os.write(1,b'x'*1000000)"],
                root,
                root / "out",
                root / "err",
                Limits(output_bytes=4096),
            )
            self.assertLessEqual((root / "out").stat().st_size, 4096)
            # A short write is possible: byte-bound success is not a valid
            # extraction result; its absent JSON trailer still fails closed.
            with self.assertRaises(ExtractionError):
                decode_bounded(root / "out", Limits(output_bytes=4096))
            self.assertFalse(result["provenance_verified"])

    def test_generated_artifact_has_same_hard_file_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            artifact = root / "artifact"
            run_bounded(
                [
                    sys.executable,
                    "-c",
                    "from pathlib import Path; Path('artifact').write_bytes(b'x'*1000000)",
                ],
                root,
                root / "out",
                root / "err",
                Limits(output_bytes=4096),
                artifacts=(artifact,),
            )
            self.assertLessEqual(artifact.stat().st_size, 4096)

    def test_decode_depth_before_json_parser(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "input.json"
            path.write_text('{"a":' + "[" * 10 + "0" + "]" * 10 + "}")
            with self.assertRaisesRegex(ExtractionError, "nesting"):
                decode_bounded(path, Limits(json_depth=5))

    def test_quoted_brackets_do_not_count(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "input.json"
            path.write_text('{"a":"[[[\\"[[["}')
            self.assertEqual(
                decode_bounded(path, Limits(json_depth=2)), {"a": '[[["[[['}
            )

    def test_byte_limit(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "input.json"
            path.write_text('{"a":12345}')
            with self.assertRaisesRegex(ExtractionError, "byte limit"):
                decode_bounded(path, Limits(output_bytes=2))

    def test_process_receipt_is_not_proof(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            result = run_bounded(
                [sys.executable, "-c", 'print("ok")'], root, root / "out", root / "err"
            )
            self.assertEqual(result["returncode"], 0)
            self.assertFalse(result["effects_proven"])
            self.assertFalse(result["provenance_verified"])

    def test_deadline(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with self.assertRaisesRegex(ExtractionError, "deadline"):
                run_bounded(
                    [sys.executable, "-c", "import time; time.sleep(10)"],
                    root,
                    root / "out",
                    root / "err",
                    Limits(seconds=0.1),
                )


if __name__ == "__main__":
    unittest.main()
