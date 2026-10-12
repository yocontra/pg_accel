from __future__ import annotations

import dataclasses
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import cpu_cheat_audit as audit  # noqa: E402
from metadata_build import ROOTS, build, digest  # noqa: E402
from metadata_effects import Unproven  # noqa: E402
from metadata_link import capture, map_exports, verify_binding  # noqa: E402

SOURCE = """
typedef unsigned long long uint64_t;
typedef enum { PGACCEL_OK = 0, PGACCEL_ERROR = 1 } pgaccel_status;
struct pgaccel_join_rows_request { uint64_t rows; const uint64_t* values; };
struct pgaccel_join_control { uint64_t limit; };
extern "C" pgaccel_status pgaccel_join_rows_workspace_bytes(
    const pgaccel_join_rows_request* r, uint64_t* out) {
  *out = r->rows * 8; return PGACCEL_OK;
}
extern "C" pgaccel_status pgaccel_join_rows_controlled_workspace_bytes(
    const pgaccel_join_rows_request* r, const pgaccel_join_control* c, uint64_t* out) {
  *out = r->rows * 8 + c->limit; return PGACCEL_OK;
}
"""


class MetadataLinkTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="metadata-link-test-")
        self.addCleanup(temporary.cleanup)
        self.directory = pathlib.Path(temporary.name)
        self.source = self.directory / "query.cpp"
        self.source.write_text(SOURCE)
        self.object = self.directory / "query.o"
        self.library = self.directory / (
            "query.dylib" if sys.platform == "darwin" else "query.so"
        )
        self.map = self.directory / "query.map"
        self.binding = self.directory / "binding.json"
        self.marker = self.directory / "rebuild-marker"
        self.marker.write_text("before link")
        self.compiler = pathlib.Path(shutil.which("clang++") or "missing-clang++")

    def link(self, flags=("-fPIC",)):
        build(self.compiler, self.source, self.object, flags)
        linker_flags = (
            ("-dynamiclib", f"-Wl,-map,{self.map}")
            if sys.platform == "darwin"
            else ("-shared", f"-Wl,-Map,{self.map}")
        )
        subprocess.run(
            [
                str(self.compiler),
                *linker_flags,
                str(self.object),
                "-o",
                str(self.library),
            ],
            check=True,
            capture_output=True,
            timeout=30,
        )
        document = capture(self.library, self.map, self.object)
        self.binding.write_text(json.dumps(document))
        return document

    def verify(self):
        return verify_binding(self.binding, self.library, self.source, self.marker)

    def test_real_link_map_rebuild_and_symbol_ownership(self):
        for flags in (("-fPIC", "-O3"), ("-fPIC", "-O0", "-g")):
            with self.subTest(flags=flags):
                self.link(flags)
                evidence = self.verify()
                self.assertEqual(evidence["status"], "independently_verified")
                self.assertEqual(set(evidence["exports"]), set(ROOTS))
                self.assertEqual(
                    {proof["name"] for proof in evidence["proofs"]}, set(ROOTS)
                )

    def test_receipt_proof_booleans_are_not_authority(self):
        self.link()
        receipt = self.object.with_suffix(".o.metadata.json")
        document = json.loads(receipt.read_text())
        document["proofs"] = [{"ok": True, "name": "fake"}]
        receipt.write_text(json.dumps(document))
        self.binding.write_text(
            json.dumps(capture(self.library, self.map, self.object))
        )
        self.assertEqual(
            {proof["name"] for proof in self.verify()["proofs"]}, set(ROOTS)
        )

    def test_receipt_cannot_choose_an_executable_or_release_build_root(self):
        self.link()
        with self.assertRaisesRegex(Unproven, "outside the expected release build"):
            verify_binding(
                self.binding,
                self.library,
                self.source,
                self.marker,
                build_root=self.directory / "wrong-build",
            )
        receipt = self.object.with_suffix(".o.metadata.json")
        document = json.loads(receipt.read_text())
        document["compiler"] = str(self.source)
        receipt.write_text(json.dumps(document))
        with self.assertRaisesRegex(Unproven, "not the audit compiler"):
            self.verify()

    def test_forged_object_receipt_cannot_replace_rebuilt_object(self):
        self.link()
        self.object.write_bytes(self.object.read_bytes() + b"different object")
        receipt = self.object.with_suffix(".o.metadata.json")
        document = json.loads(receipt.read_text())
        document["object_sha256"] = digest(self.object.read_bytes())
        receipt.write_text(json.dumps(document))
        self.binding.write_text(
            json.dumps(capture(self.library, self.map, self.object))
        )
        with self.assertRaisesRegex(Unproven, "differs from linked input"):
            self.verify()

    def test_changed_library_or_source_invalidates_binding(self):
        for target in ("library", "source"):
            with self.subTest(target=target):
                self.link()
                if target == "library":
                    self.library.write_bytes(
                        self.library.read_bytes() + b"modified library"
                    )
                else:
                    self.source.write_text(
                        SOURCE.replace("r->rows * 8", "r->values[0]")
                    )
                with self.assertRaises(Unproven):
                    self.verify()

    def test_wrong_source_and_stale_link_are_rejected(self):
        self.link()
        with self.assertRaises(Unproven):
            verify_binding(
                self.binding, self.library, self.directory / "other.cpp", self.marker
            )
        self.marker.write_text("after link")
        with self.assertRaisesRegex(Unproven, "predates"):
            self.verify()

    def test_changed_map_owner_is_not_just_a_name_match(self):
        self.link()
        text = self.map.read_text()
        self.assertIn(str(self.object), text)
        self.map.write_text(
            text.replace(str(self.object), str(self.directory / "different.o"))
        )
        with self.assertRaisesRegex(Unproven, "not supplied"):
            capture(self.library, self.map, self.object)


class MetadataMapAndIsolationTests(unittest.TestCase):
    def test_real_source_view_tag_is_not_a_preprocessor_failure(self):
        source = ROOT / "pgaccel-kernels/src/join_rows_layout.cpp"
        before = audit.audit_source(source, source.read_text())
        self.assertTrue(
            all(
                "macro_expanded_body" in entry.classifications
                for entry in before.entrypoint_audits
            )
        )
        after = audit._metadata_overlay(before, frozenset(ROOTS))
        self.assertTrue(all(entry.ok for entry in after.entrypoint_audits))
        self.assertFalse(after.findings)
        self.assertFalse(audit._external_device_proofs([source], [after]))

    def test_supported_map_dialects_and_rejected_ownership(self):
        names = list(ROOTS)
        samples = (
            f"# Object files:\n[ 1] query.o\n# Sections:\n# Symbols:\n"
            f"0x100 0x10 [ 1] _{names[0]}\n0x110 0x10 [ 1] _{names[1]}\n"
            f"# Dead Stripped Symbols:\n0x100 0x10 [ 2] _{names[0]}\n",
            "Linker script and memory map\n .text          0x100 0x20 query.o\n"
            f"                0x100 {names[0]}\n                0x110 {names[1]}\n",
            "Linker script and memory map\n .text.long_section_name\n"
            "                0x100 0x20 query.o\n"
            f"                0x100 {names[0]}\n                0x110 {names[1]}\n",
            "VMA LMA Size Align Out In Symbol\n100 100 20 4 query.o:(.text)\n"
            f"100 100 10 1 {names[0]}\n110 110 10 1 {names[1]}\n",
        )
        for sample in samples:
            with self.subTest(sample=sample):
                result = map_exports(sample, ROOT)
                self.assertEqual(result[names[0]], (0x100, ROOT / "query.o"))
                self.assertEqual(result[names[1]], (0x110, ROOT / "query.o"))
                with self.assertRaises(Unproven):
                    map_exports(sample.replace(names[1], "unknown"), ROOT)
                with self.assertRaises(Unproven):
                    map_exports(sample.replace(names[1], names[0]), ROOT)
        with self.assertRaises(Unproven):
            map_exports("these names appear: " + " ".join(ROOTS), ROOT)

    def test_report_overlay_never_supplies_borrowable_device_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = pathlib.Path(temporary) / "metadata.cpp"
            source.write_text(SOURCE)
            before = audit.audit_source(source, SOURCE)
            after = audit._metadata_overlay(before, frozenset(ROOTS))
            self.assertTrue(all(entry.ok for entry in after.entrypoint_audits))
            self.assertFalse(after.findings)
            for entry in after.entrypoint_audits:
                self.assertFalse(entry.guaranteed_output_parameter_positions)
                self.assertFalse(entry.output_parameter_position_variants)
            self.assertFalse(audit._external_device_proofs([source], [after]))
            # Even accidental future device tagging must not turn metadata into
            # an externally borrowable GPU proof.
            mixed = dataclasses.replace(
                after,
                entrypoint_audits=tuple(
                    dataclasses.replace(
                        entry,
                        classifications=(*entry.classifications, "device_dispatch"),
                    )
                    for entry in after.entrypoint_audits
                ),
            )
            self.assertFalse(audit._external_device_proofs([source], [mixed]))
            wrapper = (
                SOURCE.split('extern "C"', 1)[0]
                + """
extern "C" pgaccel_status pgaccel_join_rows_workspace_bytes(
    const pgaccel_join_rows_request*, uint64_t*);
extern "C" pgaccel_status pgaccel_not_a_kernel(
    const pgaccel_join_rows_request* r, uint64_t* out) {
  return pgaccel_join_rows_workspace_bytes(r, out);
}
"""
            )
            result = audit.audit_source(
                source.with_name("wrapper.cpp"),
                wrapper,
                external_proofs=audit._external_device_proofs([source], [after]),
            )
            self.assertEqual(len(result.entrypoint_audits), 1)
            self.assertEqual(
                result.entrypoint_audits[0].entrypoint, "pgaccel_not_a_kernel"
            )
            self.assertFalse(result.entrypoint_audits[0].ok)

    def test_overlay_preserves_hard_findings_and_inventory_errors(self):
        source = ROOT / "fixture.cpp"
        before = audit.audit_source(source, SOURCE)
        for classification in (
            "preprocessor_expansion_error",
            "parse_error",
            "ambiguous_control_flow",
        ):
            broken = dataclasses.replace(
                before,
                findings=(
                    *before.findings,
                    audit.Finding(
                        source, 1, next(iter(ROOTS)), "unproven", (classification,)
                    ),
                ),
            )
            with self.assertRaises(audit.ParseError):
                audit._metadata_overlay(broken, frozenset(ROOTS))
        with self.assertRaises(audit.ParseError):
            audit._metadata_overlay(before, frozenset())
        unchanged, abi = audit._integrate_metadata_binding(
            [before], None, ROOT / "missing.json", (), None
        )
        self.assertIsNone(abi)
        self.assertEqual(unchanged[0], before)
        self.assertEqual(
            unchanged[-1].findings[0].classifications[0], "metadata_binding_error"
        )


if __name__ == "__main__":
    unittest.main()
