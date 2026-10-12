from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

from metadata_build import (  # noqa: E402
    ROOTS,
    build,
    checked_flags,
    prove_unit,
    verify_receipt,
)
from metadata_effects import Unproven  # noqa: E402

PRELUDE = """
#include <stdint.h>
enum pgaccel_status { PGACCEL_OK = 0 };
struct Request { uint64_t rows; const uint64_t* values; };
"""
QUERY = """
extern "C" pgaccel_status pgaccel_metadata(const Request* req, uint64_t* out) {
  *out = size(*req); return PGACCEL_OK;
}
"""
HELPER = "static uint64_t size(const Request& r) { return r.rows * 8; }\n"
ROLES = {"pgaccel_metadata": ("Request",)}


class MetadataBuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="metadata-build-test-")
        self.addCleanup(self.temporary.cleanup)
        self.directory = pathlib.Path(self.temporary.name)
        self.compiler = pathlib.Path(shutil.which("clang++") or "missing-clang++")
        self.source = self.directory / "query.cpp"
        self.header = self.directory / "layout.h"
        self.output = self.directory / "query.o"
        self.source.write_text('#include "layout.h"\n' + QUERY)
        self.header.write_text(PRELUDE + HELPER)

    def build(self, flags=()):
        return build(self.compiler, self.source, self.output, flags, ROLES)

    def verify(self, receipt, flags=()):
        verify_receipt(receipt, self.compiler, self.source, self.output, flags, ROLES)

    def run_object(self, extra=""):
        harness = self.directory / "main.cpp"
        harness.write_text(
            PRELUDE
            + extra
            + 'extern "C" pgaccel_status pgaccel_metadata(const Request*, uint64_t*);\n'
            + "int main() { Request r{19, nullptr}; uint64_t out=0; "
            + "return pgaccel_metadata(&r, &out) != 0 || out != 152; }"
        )
        executable = self.directory / "query"
        subprocess.run(
            [str(self.compiler), str(harness), str(self.output), "-o", str(executable)],
            check=True,
            capture_output=True,
        )
        subprocess.run([str(executable)], check=True, timeout=10)

    def test_built_object_executes_proved_metadata_and_replays(self):
        receipt = self.build(("-O3", "-fPIC"))
        self.verify(receipt, ("-O3", "-fPIC"))
        document = json.loads(receipt.read_text())
        self.assertEqual(document["proofs"][0]["name"], "pgaccel_metadata")
        self.assertTrue(document["proofs"][0]["ok"])
        self.run_object()

    def test_codegen_cannot_reopen_header_changed_after_proof(self):
        def mutate_after_proof(ast, roots):
            result = prove_unit(ast, roots)
            self.header.write_text(
                PRELUDE + HELPER.replace("r.rows * 8", "r.values[0]")
            )
            return result

        with mock.patch("metadata_build.prove_unit", side_effect=mutate_after_proof):
            receipt = self.build()
        self.run_object()
        with self.assertRaises(Unproven):
            self.verify(receipt)

    def test_other_object_cannot_replace_internal_helper(self):
        self.build(("-O0",))
        self.run_object("uint64_t size(const Request&) { return 999; }\n")

    def test_dependency_file_and_header_bytes_are_bound(self):
        depfile = self.directory / "query.d"
        receipt = build(self.compiler, self.source, self.output, (), ROLES, depfile)
        self.assertIn(str(self.source), depfile.read_text())
        self.assertIn(str(self.header), depfile.read_text())
        self.verify(receipt)
        self.header.write_text(self.header.read_text() + "\n// header comment\n")
        with self.assertRaises(Unproven):
            self.verify(receipt)

    def test_uncaptured_has_include_inputs_fail_closed(self):
        (self.directory / "presence.h").write_text("// never included\n")
        self.header.write_text(
            PRELUDE
            + '#if __has_include("presence.h")\n'
            + HELPER
            + "#else\n"
            + HELPER.replace("* 8", "* 16")
            + "#endif\n"
        )
        with self.assertRaisesRegex(Unproven, "changed preprocessing semantics"):
            self.build()

    def test_header_source_flags_and_object_changes_invalidate_receipt(self):
        for mutation in ("header", "source", "flags", "object"):
            with self.subTest(mutation=mutation):
                receipt = self.build()
                flags = ()
                if mutation == "header":
                    self.header.write_text(
                        self.header.read_text().replace("* 8", "* 16")
                    )
                elif mutation == "source":
                    self.source.write_text(
                        self.source.read_text() + "\n// source edit\n"
                    )
                elif mutation == "flags":
                    flags = ("-O3",)
                else:
                    self.output.write_bytes(self.output.read_bytes() + b"changed")
                with self.assertRaises(Unproven):
                    self.verify(receipt, flags)

    def test_optimization_and_defines_cannot_hide_row_work(self):
        for condition, flags in (
            ("defined(__OPTIMIZE__)", ("-O3",)),
            ("defined(ROW_READ)", ("-DROW_READ=1",)),
        ):
            with self.subTest(condition=condition):
                self.header.write_text(
                    PRELUDE
                    + "static uint64_t size(const Request& r) {\n"
                    + f"#if {condition}\n return r.values[0];\n"
                    + "#else\n return r.rows;\n#endif\n}\n"
                )
                receipt = self.build()
                old_object = self.output.read_bytes()
                old_receipt = receipt.read_bytes()
                with self.assertRaisesRegex(Unproven, "opaque row storage"):
                    self.build(flags)
                self.assertEqual(self.output.read_bytes(), old_object)
                self.assertEqual(receipt.read_bytes(), old_receipt)
                with self.assertRaises(Unproven):
                    self.verify(receipt, flags)

    def test_transitive_include_changes_are_replayed(self):
        included = self.directory / "nested.h"
        included.write_text(HELPER)
        self.header.write_text(PRELUDE + '#include "nested.h"\n')
        receipt = self.build()
        included.write_text(HELPER.replace("r.rows * 8", "r.values[0]"))
        with self.assertRaises(Unproven):
            self.verify(receipt)
        with self.assertRaisesRegex(Unproven, "opaque row storage"):
            self.build()

    def test_weak_helpers_and_global_initializers_do_not_get_receipts(self):
        for extra in (
            HELPER.replace("static ", "inline "),
            HELPER + "extern int external(); int x = external();",
            HELPER + "__attribute__((constructor)) static void start() {}",
            HELPER + "__attribute__((destructor)) static void stop() {}",
            HELPER + 'asm(".byte 0");',
        ):
            with self.subTest(extra=extra):
                self.header.write_text(PRELUDE + extra)
                with self.assertRaises(Unproven):
                    self.build()
                self.assertFalse(self.output.exists())
                self.assertFalse(self.output.with_suffix(".o.metadata.json").exists())

    def test_unknown_and_action_overriding_flags_fail_closed(self):
        for flags in (
            ("-Xclang", "-load", "plugin.so"),
            ("@flags.rsp",),
            ("-include-pch", "hidden.pch"),
            ("-include", "hidden.h"),
            ("-Wno-error=vla",),
            ("-Wno-error=return-type",),
            ("-flto",),
            ("-c", "other.cpp"),
            ("-o", "other.o"),
            ("-x", "c++"),
            ("-I",),
            ("-D", "-Xclang"),
        ):
            with self.subTest(flags=flags), self.assertRaises(Unproven):
                checked_flags(flags)

    def test_profile_rejects_vla_bounds_even_when_ast_would_drop_them(self):
        self.header.write_text(
            PRELUDE
            + "extern int side_effect();\n"
            + "static int ignored(const int values[side_effect()]) { return 0; }\n"
            + HELPER
        )
        with self.assertRaises(Unproven):
            self.build()

    def test_transitive_header_cannot_weaken_mandatory_diagnostics(self):
        for directive in (
            '#pragma clang diagnostic ignored "-Wvla"',
            '#pragma clang diagnostic ignored "-Wreturn-type"',
            '#pragma GCC diagnostic warning "-Wvla"',
            "#pragma clang system_header",
            '_Pragma("clang diagnostic ignored \\"-Wvla\\"")',
        ):
            with self.subTest(directive=directive):
                self.header.write_text(
                    directive
                    + "\n"
                    + PRELUDE
                    + "extern int hidden();\n"
                    + "static int h(const int values[hidden()]) { return 1; }\n"
                    + HELPER.replace("r.rows * 8", "r.rows * 8 + h(nullptr)")
                )
                with self.assertRaises(Unproven):
                    self.build()

    def test_receipt_cannot_choose_its_expected_source_object_or_contract(self):
        receipt = self.build()
        for field, replacement in (
            ("source", "/unrelated.cpp"),
            ("object", "/unrelated.o"),
            ("roots", {}),
            ("profile", []),
            ("compiler_version", "different"),
            ("compiler_sha256", "0" * 64),
            ("driver_sha256", "0" * 64),
            ("verifier_sha256", "0" * 64),
        ):
            with self.subTest(field=field):
                original = receipt.read_bytes()
                document = json.loads(original)
                document[field] = replacement
                receipt.write_text(json.dumps(document))
                with self.assertRaises(Unproven):
                    self.verify(receipt)
                receipt.write_bytes(original)

    def test_real_queries_build_without_sycl(self):
        source = ROOT / "pgaccel-kernels/src/join_rows_layout.cpp"
        flags = ("-O3", "-fPIC", "-I", str(ROOT / "pgaccel-kernels/include"))
        receipt = build(self.compiler, source, self.output, flags)
        verify_receipt(receipt, self.compiler, source, self.output, flags)
        document = json.loads(receipt.read_text())
        self.assertEqual({proof["name"] for proof in document["proofs"]}, set(ROOTS))
        self.assertTrue(all(proof["ok"] for proof in document["proofs"]))

    def test_instrumented_object_preserves_source_and_header_coverage(self):
        def execute(arguments, **kwargs):
            return subprocess.run(
                arguments,
                check=True,
                capture_output=True,
                text=True,
                timeout=30,
                **kwargs,
            ).stdout

        llvm_cov = execute([str(self.compiler), "-print-prog-name=llvm-cov"]).strip()
        llvm_profdata = execute(
            [str(self.compiler), "-print-prog-name=llvm-profdata"]
        ).strip()
        source = ROOT / "pgaccel-kernels/src/join_rows_layout.cpp"
        instrumentation = ("-fprofile-instr-generate", "-fcoverage-mapping")
        flags = ("-O0", "-I", str(ROOT / "pgaccel-kernels/include"), *instrumentation)
        build(self.compiler, source, self.output, flags)
        executable = self.directory / "instrumented"
        execute(
            [
                str(self.compiler),
                "-std=c++17",
                *flags,
                str(ROOT / "pgaccel-kernels/test/test_join_rows_layout.cpp"),
                str(self.output),
                "-o",
                str(executable),
            ]
        )
        raw = self.directory / "coverage.profraw"
        output = execute(
            [str(executable)], env={**os.environ, "LLVM_PROFILE_FILE": str(raw)}
        )
        self.assertIn("40389 passed, 0 failed", output)
        profile = self.directory / "coverage.profdata"
        execute([llvm_profdata, "merge", "-sparse", str(raw), "-o", str(profile)])
        report = json.loads(
            execute([llvm_cov, "export", str(executable), f"-instr-profile={profile}"])
        )
        files = {
            pathlib.Path(entry["filename"]): entry
            for entry in report["data"][0]["files"]
        }
        self.assertFalse(any("<stdin>" in str(path) for path in files))
        for path in (source, source.with_suffix(".hpp")):
            lines = files[path]["summary"]["lines"]
            self.assertGreater(lines["count"], 0)
            self.assertGreaterEqual(lines["percent"], 90.0)


if __name__ == "__main__":
    unittest.main()
