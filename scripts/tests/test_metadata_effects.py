from __future__ import annotations

import pathlib
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

from metadata_effects import OutputRecord, Unproven, Verifier, compile_ast  # noqa: E402


PRELUDE = r"""
#include <cstdint>
enum pgaccel_status { PGACCEL_OK = 0, PGACCEL_INVALID_ARGUMENT = 4 };
struct Request {
  uint64_t rows;
  const uint64_t* values;
  uint64_t* writable;
  uint64_t counts[4];
  const Request* nested;
  int (*callback)(void*);
};
"""


def fixture(body: str, helpers: str = "") -> str:
    return (
        PRELUDE
        + helpers
        + r"""
extern "C" pgaccel_status pgaccel_metadata(const Request* req, uint64_t* out) {
  if (req == nullptr || out == nullptr) return PGACCEL_INVALID_ARGUMENT;
"""
        + body
        + "\n}"
    )


def prove(source: str):
    ast = compile_ast(source, ROOT, ROOT / "pgaccel-kernels/include")
    return Verifier(ast).prove("pgaccel_metadata", ("Request",))


class TypedMetadataEffectsTests(unittest.TestCase):
    def test_memcmp_explicit_and_macro_nonnull_preserve_bounded_contract(self):
        declaration = 'extern "C" int memcmp(const void*, const void*, __SIZE_TYPE__) '
        body = (
            "Request zero{}; if (memcmp(req, &zero, sizeof(zero))) "
            "return PGACCEL_INVALID_ARGUMENT; *out = req->rows; return PGACCEL_OK;"
        )
        for helpers in (
            declaration + "__attribute__((pure, nonnull(1,2)));",
            "#define LIBC_NONNULL __attribute__((nonnull(1,2)))\n"
            + declaration
            + "LIBC_NONNULL;",
            declaration + "__attribute__((nonnull));\n" + declaration + ";",
        ):
            with self.subTest(helpers=helpers):
                result = prove(fixture(body, helpers))
                self.assertTrue(result.ok, result.detail)

    def test_memcmp_nonnull_does_not_relax_general_helpers(self):
        helpers = (
            "uint64_t helper(const Request* r) __attribute__((nonnull));\n"
            "uint64_t helper(const Request* r) { return r->rows; }"
        )
        result = prove(fixture("*out = helper(req); return PGACCEL_OK;", helpers))
        self.assertFalse(result.ok, result)
        self.assertIn("unproven callable attribute", result.detail)

    def test_memcmp_function_try_bodies_are_not_external_declarations(self):
        declaration = 'extern "C" int memcmp(const void*, const void*, __SIZE_TYPE__);'
        implementation = (
            'extern "C" int memcmp(const void* a, const void*, __SIZE_TYPE__) '
            "try { return static_cast<const Request*>(a)->values[0]; } "
            "catch (...) { return 0; }"
        )
        body = (
            "Request zero{}; if (memcmp(req, &zero, sizeof(zero))) "
            "return PGACCEL_INVALID_ARGUMENT; *out = req->rows; return PGACCEL_OK;"
        )
        for later in (False, True):
            source = (
                fixture(body, declaration) + implementation
                if later
                else fixture(body, declaration + implementation)
            )
            with self.subTest(later=later):
                # Compiler rejection raises instead of satisfying this check.
                result = prove(source)
                self.assertFalse(result.ok, result)

    def test_memcmp_rejects_earlier_and_later_attributes_and_definitions(self):
        declaration = 'extern "C" int memcmp(const void*, const void*, __SIZE_TYPE__)'
        body = (
            "Request zero{}; if (memcmp(req, &zero, sizeof(zero))) "
            "return PGACCEL_INVALID_ARGUMENT; *out = req->rows; return PGACCEL_OK;"
        )
        for attribute in ('annotate("unknown effect")', "weak", 'visibility("hidden")'):
            for later in (False, True):
                attributed = declaration + f" __attribute__(({attribute}));\n"
                source = (
                    fixture(body, declaration + ";\n") + attributed
                    if later
                    else fixture(body, attributed + declaration + ";\n")
                )
                with self.subTest(attribute=attribute, later=later):
                    # prove() must return a rejection; compiler exceptions fail
                    # this test instead of masquerading as verifier coverage.
                    result = prove(source)
                    self.assertFalse(result.ok, result)
                    self.assertIn("memcmp", result.detail)
        for implementation in (
            'extern "C" int memcmp(const void*, const void*, __SIZE_TYPE__) { return 0; }',
            'extern "C" int memcmp(const void* a, const void*, __SIZE_TYPE__) '
            "{ return static_cast<const Request*>(a)->values[0]; }",
        ):
            for later in (False, True):
                source = (
                    fixture(body, declaration + ";\n") + implementation
                    if later
                    else fixture(body, implementation)
                )
                with self.subTest(implementation=implementation, later=later):
                    result = prove(source)
                    self.assertFalse(result.ok, result)
                    self.assertIn("external builtin ABI", result.detail)

    def test_memcmp_rejects_lookalikes_wrong_abi_and_renaming(self):
        for declaration, call in (
            ("int memcmp(const void*, const void*, __SIZE_TYPE__);", "memcmp"),
            (
                "namespace other { int memcmp(const void*, const void*, __SIZE_TYPE__); }",
                "other::memcmp",
            ),
            (
                'extern "C" long memcmp(const void*, const void*, __SIZE_TYPE__);',
                "memcmp",
            ),
            ('extern "C" int memcmp(void*, void*, __SIZE_TYPE__);', "memcmp"),
            (
                'extern "C" int memcmp(const void*, const void*, unsigned int);',
                "memcmp",
            ),
            (
                'extern "C" int memcmp(const void*, const void*, __SIZE_TYPE__, ...);',
                "memcmp",
            ),
            (
                'extern "C" int memcmp(const void*, const void*, __SIZE_TYPE__) asm("untrusted_compare");',
                "memcmp",
            ),
        ):
            with self.subTest(declaration=declaration):
                result = prove(
                    fixture(
                        f"Request zero{{}}; if ({call}(&zero, &zero, sizeof(zero))) "
                        "return PGACCEL_INVALID_ARGUMENT; *out = req->rows; return PGACCEL_OK;",
                        declaration,
                    )
                )
                self.assertFalse(result.ok, result)

    def test_memcmp_nonnull_keeps_exact_object_and_extent_requirements(self):
        helpers = (
            'extern "C" int memcmp(const void*, const void*, __SIZE_TYPE__) '
            "__attribute__((nonnull(1,2)));"
        )
        for operands in (
            "nullptr, &zero, sizeof(zero)",
            "req, nullptr, sizeof(zero)",
            "req->values, &zero, sizeof(zero)",
            "req->nested, &zero, sizeof(zero)",
            "out, &zero, sizeof(zero)",
            "req, &zero, sizeof(zero) + 1",
            "req, &zero, req->rows",
            "req, &zero, 0",
            "req, &zero, sizeof(uint64_t)",
        ):
            with self.subTest(operands=operands):
                result = prove(
                    fixture(
                        f"Request zero{{}}; if (memcmp({operands})) "
                        "return PGACCEL_INVALID_ARGUMENT; *out = req->rows; return PGACCEL_OK;",
                        helpers,
                    )
                )
                self.assertFalse(result.ok, result)

    def test_source_pragmas_cannot_suppress_hidden_vla_bound_errors(self):
        for directive in (
            '#pragma clang diagnostic ignored "-Wvla"',
            '#pragma GCC diagnostic warning "-Wvla"',
            '#pragma clang diagnostic ignored "-Weverything"',
            '_Pragma("clang diagnostic ignored \\"-Wvla\\"")',
        ):
            with self.subTest(directive=directive), self.assertRaises(Unproven):
                prove(
                    fixture(
                        "*out = h(nullptr) + req->rows; return PGACCEL_OK;",
                        directive + "\nextern int hidden();\n"
                        "int h(const int data[hidden()]) { return 1; }\n",
                    )
                )

    def test_scalar_metadata_and_fixed_descriptor_arrays(self):
        for body in (
            "*out = req->rows * 8; return PGACCEL_OK;",
            "out[0] = req->rows * 8; return PGACCEL_OK;",
            "auto& result = *out; result = req->rows; return PGACCEL_OK;",
            "uint64_t total = 0; for (uint64_t k = 0; k < 4; ++k) "
            "total += req->counts[k]; *out = total; return PGACCEL_OK;",
            "*out = req->values == nullptr ? 0 : req->rows * 8; return PGACCEL_OK;",
        ):
            with self.subTest(body=body):
                result = prove(fixture(body))
                self.assertTrue(result.ok, result.detail)

    def test_resolved_helper_parameters_keep_metadata_permissions(self):
        result = prove(
            fixture(
                "*out = size(*req); return PGACCEL_OK;",
                "uint64_t size(const Request& r) { return r.rows * 8; }",
            )
        )
        self.assertTrue(result.ok, result.detail)
        self.assertIn("size", result.functions)

    def test_row_reads_writes_and_same_typed_pointer_fields_are_rejected(self):
        for expression in (
            "req->values[0]",
            "*req->values",
            "req->nested->rows",
            "req->writable[0] = req->rows",
            "*(req->values + 1)",
            "reinterpret_cast<const Request*>(req->values)->rows",
            "static_cast<const uint64_t*>(static_cast<const void*>(req->values))[0]",
        ):
            with self.subTest(expression=expression):
                result = prove(fixture(f"*out = ({expression}); return PGACCEL_OK;"))
                self.assertFalse(result.ok, result)

    def test_transitive_row_reader_is_not_a_metadata_helper(self):
        result = prove(
            fixture(
                "*out = size(*req); return PGACCEL_OK;",
                "uint64_t read(const uint64_t* data) { return data[0]; }\n"
                "uint64_t size(const Request& r) { return read(r.values); }",
            )
        )
        self.assertFalse(result.ok, result)
        self.assertIn("opaque row storage", result.detail)

    def test_pointer_escape_and_reconstruction_are_rejected(self):
        for body in (
            "uint64_t* p = out; *p = req->rows; return PGACCEL_OK;",
            "auto address = reinterpret_cast<uintptr_t>(out); "
            "*reinterpret_cast<uint64_t*>(address) = req->rows; return PGACCEL_OK;",
            "++out; *out = req->rows; return PGACCEL_OK;",
            "out[1] = req->rows; return PGACCEL_OK;",
            "*out = *out + req->rows; return PGACCEL_OK;",
        ):
            with self.subTest(body=body):
                self.assertFalse(prove(fixture(body)).ok)

    def test_memcmp_requires_exact_metadata_objects_and_trusted_linkage(self):
        helpers = "#include <cstring>\n"
        good = (
            "Request zero{}; if (std::memcmp(req, &zero, sizeof(zero))) "
            "return PGACCEL_INVALID_ARGUMENT; *out = req->rows; return PGACCEL_OK;"
        )
        result = prove(fixture(good, helpers))
        self.assertTrue(result.ok, result.detail)
        for call in (
            "std::memcmp(req->values, &zero, sizeof(zero))",
            "std::memcmp(req, &zero, sizeof(zero) + 1)",
            "std::memcmp(out, &zero, sizeof(zero))",
        ):
            with self.subTest(call=call):
                self.assertFalse(
                    prove(
                        fixture(
                            f"Request zero{{}}; *out = {call}; return PGACCEL_OK;",
                            helpers,
                        )
                    ).ok
                )
        renamed = (
            'extern "C" int memcmp(const void*, const void*, __SIZE_TYPE__) '
            'asm("untrusted_compare");'
        )
        result = prove(fixture(good.replace("std::memcmp", "memcmp"), renamed))
        self.assertFalse(result.ok, result)

    def test_unknown_calls_callbacks_and_recursion_are_rejected(self):
        for body, helpers in (
            (
                "*out = external(req->values); return PGACCEL_OK;",
                "uint64_t external(const uint64_t*);",
            ),
            ("*out = req->callback(nullptr); return PGACCEL_OK;", ""),
            (
                "*out = recurse(*req); return PGACCEL_OK;",
                "uint64_t recurse(const Request& r) { return recurse(r); }",
            ),
        ):
            with self.subTest(body=body):
                self.assertFalse(prove(fixture(body, helpers)).ok)

    def test_loops_need_fixed_bounds_and_immutable_induction(self):
        for body in (
            "for (uint64_t i = 0; i < req->rows; ++i) {}",
            "for (uint64_t i = 0; i < 65; ++i) {}",
            "for (uint64_t i = 1; i < 4; ++i) {}",
            "for (uint64_t i = 0; i < 4; ++i) { --i; }",
            "for (uint64_t i = 0; i < 4; ++i) { auto& alias = i; alias = 0; }",
            "for (uint64_t i = 0; i < 4; ++i) { mutate(&i); }",
            "while (req->rows) {}",
        ):
            with self.subTest(body=body):
                result = prove(
                    fixture(
                        body + " *out = req->rows; return PGACCEL_OK;",
                        "uint64_t mutate(uint64_t* p) { *p = 0; return 0; }",
                    )
                )
                self.assertFalse(result.ok, result)

    def test_array_bounds_are_not_inferred_from_unknown_scalar_values(self):
        for index in ("4", "req->rows", "k", "-1"):
            with self.subTest(index=index):
                self.assertFalse(
                    prove(
                        fixture(
                            f"uint64_t k = 0; *out = req->counts[{index}]; "
                            "return PGACCEL_OK;"
                        )
                    ).ok
                )

    def test_publication_must_cover_success_and_failure_guards_cannot_go_stale(self):
        for body in (
            "if (req->rows) *out = req->rows; return PGACCEL_OK;",
            "for (uint64_t i=0; i<4; ++i) *out = req->counts[i]; return PGACCEL_OK;",
            "auto status = PGACCEL_INVALID_ARGUMENT; if (status != PGACCEL_OK) "
            "{ status = PGACCEL_OK; return status; } "
            "*out = req->rows; return PGACCEL_OK;",
            "return PGACCEL_OK;",
        ):
            with self.subTest(body=body):
                self.assertFalse(prove(fixture(body)).ok)
        result = prove(
            fixture(
                "auto status = check(*req); if (status != PGACCEL_OK) return status; "
                "*out = req->rows; return PGACCEL_OK;",
                "pgaccel_status check(const Request& r) { "
                "return r.rows ? PGACCEL_OK : PGACCEL_INVALID_ARGUMENT; }",
            )
        )
        self.assertTrue(result.ok, result.detail)

    def test_global_and_static_side_effects_are_rejected(self):
        for body in (
            "global = req->rows; *out = req->rows; return PGACCEL_OK;",
            "static uint64_t saved; saved = req->rows; "
            "*out = saved; return PGACCEL_OK;",
        ):
            with self.subTest(body=body):
                self.assertFalse(prove(fixture(body, "uint64_t global;")).ok)

    def test_object_lifecycle_cannot_hide_row_reads(self):
        for helpers in (
            "struct Local { uint64_t n = external(); }; uint64_t external();",
            "struct Local { Local() {} uint64_t n; };",
            "struct Local { ~Local() {} uint64_t n; };",
        ):
            with self.subTest(helpers=helpers):
                result = prove(
                    fixture(
                        "Local local; *out = req->rows; return PGACCEL_OK;",
                        "uint64_t external();" + helpers,
                    )
                )
                self.assertFalse(result.ok, result)

    def test_sizeof_variable_length_array_cannot_hide_evaluated_calls(self):
        # Parameter array bounds are not retained in Clang's JSON AST after
        # decay. The compiler profile must reject VLAs before producing proof
        # evidence, including side effects hidden at helper entry.
        with self.assertRaises(Unproven):
            prove(
                fixture(
                    "*out = sizeof(uint64_t[req->rows * external(req->values)]); "
                    "return PGACCEL_OK;",
                    "uint64_t external(const uint64_t*);",
                )
            )
        with self.assertRaises(Unproven):
            prove(
                fixture(
                    "*out = size(req, req->values); return PGACCEL_OK;",
                    "uint64_t external(const uint64_t*); "
                    "uint64_t size(const Request* r, "
                    "const uint64_t data[external(r->values)]) { return r->rows; }",
                )
            )

    def test_nested_metadata_work_and_storage_have_total_bounds(self):
        body = (
            "for (uint64_t a=0;a<64;++a) for(uint64_t b=0;b<64;++b) "
            "for(uint64_t c=0;c<64;++c) { uint64_t v = req->rows; } "
            "*out=req->rows; return PGACCEL_OK;"
        )
        result = prove(fixture(body))
        self.assertFalse(result.ok, result)
        self.assertIn("budget", result.detail)
        for helpers in (
            "struct Local { uint64_t data[64][64]; };",
            "struct alignas(4096) Local { uint64_t n; };",
        ):
            with self.subTest(helpers=helpers):
                self.assertFalse(
                    prove(
                        fixture(
                            "Local local; *out=req->rows; return PGACCEL_OK;", helpers
                        )
                    ).ok
                )

    def test_one_verifier_cannot_reuse_publication_or_record_state(self):
        source = fixture("*out = req->rows; return PGACCEL_OK;")
        verifier = Verifier(compile_ast(source, ROOT, ROOT / "pgaccel-kernels/include"))
        self.assertTrue(verifier.prove("pgaccel_metadata", ("Request",)).ok)
        self.assertFalse(verifier.prove("pgaccel_metadata", ("Request",)).ok)

    def test_invalid_cpp_is_not_proof_evidence(self):
        with self.assertRaises(Unproven):
            Verifier({"kind": "TranslationUnitDecl"})
        source = fixture("*out = req->rows; return PGACCEL_OK;")
        self.assertFalse(prove(source.replace('extern "C" ', "")).ok)
        with self.assertRaises(Unproven):
            prove(fixture("*out = missing_name; return PGACCEL_OK;"))
        with self.assertRaises(Unproven):
            prove(fixture("*out = req->rows;"))

    def test_effect_proof_is_specific_to_compiler_preprocessing(self):
        # This is why the staged verifier must not become a release exemption
        # until its translation-unit/dependency evidence is build-bound.
        helper = """
uint64_t size(const Request& req) {
#ifdef METADATA_TEST_ROW_READ
  return req.values[0];
#else
  return req.rows * 8;
#endif
}
"""
        source = fixture("*out = size(*req); return PGACCEL_OK;", helper)
        self.assertTrue(prove(source).ok)
        self.assertFalse(prove("#define METADATA_TEST_ROW_READ\n" + source).ok)


class ProductionMetadataEffectsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = ROOT / "pgaccel-kernels/src"
        cls.source = (cls.directory / "join_rows_layout.cpp").read_text()
        cls.header = (cls.directory / "join_rows_layout.hpp").read_text()
        cls.ast = compile_ast(
            cls.source, cls.directory, ROOT / "pgaccel-kernels/include"
        )

    def test_both_real_queries_have_bounded_typed_metadata_effects(self):
        for name, inputs in (
            ("pgaccel_join_rows_workspace_bytes", ("pgaccel_join_rows_request",)),
            (
                "pgaccel_join_rows_controlled_workspace_bytes",
                ("pgaccel_join_rows_request", "pgaccel_join_control"),
            ),
        ):
            with self.subTest(name=name):
                result = Verifier(self.ast).prove(name, inputs)
                self.assertTrue(result.ok, result.detail)
                self.assertEqual(result.loop_bounds, (4, 32, 4, 4))
                self.assertIn("valid_key", result.functions)
                self.assertIn("span", result.functions)

    def test_real_transitive_helper_mutations_fail_closed(self):
        for old, new in (
            (
                "key.nulls == nullptr || key.null_bytes >= rows",
                "key.nulls[0] == 0 || key.null_bytes >= rows",
            ),
            (
                "return pointer == nullptr;",
                "return *static_cast<const uint64_t*>(pointer) == 0;",
            ),
            ("step < 32", "step < request.inner_rows"),
            ("depth < kMaxLevels", "depth < request.outer_rows"),
            (
                "const auto options = control_contract(control);",
                "const auto options = control_contract(control); "
                "control.cancel_check(control.context);",
            ),
        ):
            with self.subTest(mutation=new):
                self.assertEqual(self.header.count(old), 1)
                source = self.source.replace(
                    '#include "join_rows_layout.hpp"', self.header.replace(old, new)
                )
                ast = compile_ast(
                    source, self.directory, ROOT / "pgaccel-kernels/include"
                )
                result = Verifier(ast).prove(
                    "pgaccel_join_rows_controlled_workspace_bytes",
                    ("pgaccel_join_rows_request", "pgaccel_join_control"),
                )
                self.assertFalse(result.ok, result)


class OutputRecordCapabilityTests(unittest.TestCase):
    contract = OutputRecord(
        "Workspace",
        (("version", "unsigned int"), ("bytes", "unsigned long long"),
         ("alignment", "unsigned long long")),
        frozenset({"version", "bytes", "alignment"}),
        frozenset({"bytes", "alignment"}),
    )
    declarations = """
    struct Workspace {
      unsigned int version;
      unsigned long long bytes;
      unsigned long long alignment;
    };
    """

    def check(self, body, helpers="", *, contract=None, declarations=None):
        source = PRELUDE + (declarations or self.declarations) + helpers + """
        extern "C" pgaccel_status pgaccel_metadata(const Request* req, Workspace* out) {
          if (!req || !out || out->version != 1) return PGACCEL_INVALID_ARGUMENT;
        """ + body + "}"
        ast = compile_ast(source, ROOT, ROOT / "pgaccel-kernels/include")
        return Verifier(ast).prove(
            "pgaccel_metadata", ("Request",), output_record=contract or self.contract
        )

    def test_complete_scalar_record_publication(self):
        result = self.check("out->bytes = req->rows; out->alignment = 64; return PGACCEL_OK;")
        self.assertTrue(result.ok, result.detail)

    def test_field_identity_survives_output_alias(self):
        result = self.check("Workspace& alias = *out; alias.bytes = req->rows; "
                            "alias.alignment = 64; return PGACCEL_OK;")
        self.assertTrue(result.ok, result.detail)

    def test_every_branch_must_publish_every_required_field(self):
        for branch in (
            "if (req->rows) out->alignment = 64;",
            "if (req->rows) out->alignment = 64; else out->bytes = 0;",
            "req->rows ? (out->alignment = 64) : (out->bytes = 0);",
            "req->rows && (out->alignment = 64);",
            "req->rows || (out->alignment = 64);",
        ):
            with self.subTest(branch=branch):
                result = self.check("out->bytes = req->rows; " + branch + "return PGACCEL_OK;")
                self.assertFalse(result.ok, result)

    def test_both_branches_publish_same_fields(self):
        result = self.check("out->bytes = req->rows; if (req->rows) "
                            "out->alignment = 64; else out->alignment = 32; return PGACCEL_OK;")
        self.assertTrue(result.ok, result.detail)

    def test_unlisted_write_whole_record_and_payload_reads_fail(self):
        for body in (
            "out->version = 2; out->bytes = req->rows; out->alignment = 64;",
            "Workspace replacement{1, req->rows, 64}; *out = replacement;",
            "out->bytes = req->values[0]; out->alignment = 64;",
            "out->bytes = req->rows; out->alignment = 64; req->writable[0] = 1;",
        ):
            with self.subTest(body=body):
                self.assertFalse(self.check(body + "return PGACCEL_OK;").ok)

    def test_helper_cannot_publish_output_field(self):
        result = self.check(
            "publish(&out->bytes, req->rows); out->alignment = 64; return PGACCEL_OK;",
            "uint64_t publish(unsigned long long* p, uint64_t v) { *p = v; return v; }",
        )
        self.assertFalse(result.ok, result)

    def test_layout_and_read_capability_are_exact(self):
        body = "out->bytes = req->rows; out->alignment = 64; return PGACCEL_OK;"
        self.assertFalse(self.check(body, declarations=self.declarations.replace(
            "unsigned int version", "unsigned long long version")).ok)
        restricted = OutputRecord(
            self.contract.type_name, self.contract.fields,
            frozenset(), self.contract.required,
        )
        self.assertFalse(self.check(body, contract=restricted).ok)

    def test_default_scalar_contract_is_unchanged(self):
        self.assertTrue(prove(fixture("*out = req->rows; return PGACCEL_OK;")).ok)
        result = prove(fixture("*out = *out + req->rows; return PGACCEL_OK;"))
        self.assertFalse(result.ok)
        self.assertIn("write-only", result.detail)

    def test_failure_guard_bounds_immutable_input_field_loop(self):
        loop = (
            "uint64_t total = 0; for (uint64_t i = 0; i < req->rows; ++i) "
            "total += req->counts[i]; out->bytes = total; out->alignment = 64; "
            "return PGACCEL_OK;"
        )
        for guard in (
            "if (req->rows > 4) return PGACCEL_INVALID_ARGUMENT;",
            "if (req->rows >= 5) return PGACCEL_INVALID_ARGUMENT;",
            "if (req->rows > 4 || out->version != 1) return PGACCEL_INVALID_ARGUMENT;",
        ):
            with self.subTest(guard=guard):
                result = self.check(guard + loop)
                self.assertTrue(result.ok, result.detail)
                self.assertEqual(result.loop_bounds, (4,))

    def test_missing_wrong_or_branch_local_bounds_cannot_escape(self):
        loop = (
            "uint64_t total = 0; for (uint64_t i = 0; i < req->rows; ++i) "
            "total += req->counts[i]; out->bytes = total; out->alignment = 64; "
            "return PGACCEL_OK;"
        )
        for guard in (
            "",
            "if (req->rows > 5) return PGACCEL_INVALID_ARGUMENT;",
            "if (req->counts[0] > 4) return PGACCEL_INVALID_ARGUMENT;",
            "if (req->rows > 4 && out->version != 1) return PGACCEL_INVALID_ARGUMENT;",
            "if (out->version == 1) { if (req->rows > 4) return PGACCEL_INVALID_ARGUMENT; }",
        ):
            with self.subTest(guard=guard):
                result = self.check(guard + loop)
                self.assertFalse(result.ok, result)

    def test_helper_guard_is_not_an_unchecked_postcondition(self):
        helper = (
            "uint64_t validate(const Request* req) { "
            "if (req->rows > 4) return 4; return 0; }"
        )
        result = self.check(
            "validate(req); uint64_t total = 0; "
            "for (uint64_t i = 0; i < req->rows; ++i) total += req->counts[i]; "
            "out->bytes = total; out->alignment = 64; return PGACCEL_OK;", helper,
        )
        self.assertFalse(result.ok, result)

    def test_output_alias_cannot_invalidate_input_bound(self):
        contract = OutputRecord(
            "R", (("rows", "unsigned long long"), ("value", "unsigned long long")),
            frozenset({"rows"}), frozenset({"rows"}),
        )
        for mutation in (
            "out->rows = 1000;",
            "out->rows += 1000;",
            "R& alias = *out; alias.rows = 1000;",
            "if (req->value) out->rows = 1000;",
            "for (unsigned i = 0; i < 1; ++i) out->rows = 1000;",
        ):
            with self.subTest(mutation=mutation):
                source = """
                enum pgaccel_status { OK = 0, ERROR = 4 };
                struct R { unsigned long long rows, value; };
                extern "C" pgaccel_status metadata(const R* req, R* out) {
                  if (req->rows > 4) return ERROR;
                """ + mutation + """
                  unsigned long long total = 0;
                  for (unsigned i = 0; i < req->rows; ++i) total += req->value;
                  out->rows = total;
                  return OK;
                }
                """
                ast = compile_ast(source, ROOT, ROOT / "pgaccel-kernels/include")
                result = Verifier(ast).prove("metadata", ("R",), output_record=contract)
                self.assertFalse(result.ok, result)

    def test_output_write_inside_guarded_loop_is_rejected(self):
        result = self.check(
            "if (req->rows > 4) return PGACCEL_INVALID_ARGUMENT; "
            "for (uint64_t i = 0; i < req->rows; ++i) out->bytes = 1000; "
            "out->bytes = req->rows; out->alignment = 64; return PGACCEL_OK;"
        )
        self.assertFalse(result.ok, result)
        self.assertIn("may alias", result.detail)

    def test_alias_cannot_prolong_loop_through_reference_compound_write(self):
        source = """
        enum pgaccel_status { OK = 0, ERROR = 4 };
        struct R { unsigned long long rows; };
        extern "C" pgaccel_status metadata(const R* req, R* out) {
          if (req->rows > 4) return ERROR;
          R& alias = *out;
          for (unsigned i = 0; i < req->rows; ++i) alias.rows += 1;
          out->rows = req->rows;
          return OK;
        }
        """
        contract = OutputRecord(
            "R", (("rows", "unsigned long long"),),
            frozenset({"rows"}), frozenset({"rows"}),
        )
        ast = compile_ast(source, ROOT, ROOT / "pgaccel-kernels/include")
        result = Verifier(ast).prove("metadata", ("R",), output_record=contract)
        self.assertFalse(result.ok, result)
        self.assertIn("may alias", result.detail)

    def test_guard_condition_output_write_cannot_recreate_stale_bounds(self):
        contract = OutputRecord(
            "R", (("rows", "unsigned long long"), ("value", "unsigned long long")),
            frozenset({"rows"}), frozenset({"rows"}),
        )
        for condition in (
            "req->rows > 4 || (out->rows = 1000) == 0",
            "req->rows > 4 || (out->rows += 1000) == 0",
            "(out->rows = 1000) == 0 || req->rows > 4",
        ):
            with self.subTest(condition=condition):
                source = """
                enum pgaccel_status { OK = 0, ERROR = 4 };
                struct R { unsigned long long rows, value; };
                extern "C" pgaccel_status metadata(const R* req, R* out) {
                """ + "if (" + condition + ") return ERROR;" + """
                  unsigned long long total = 0;
                  for (unsigned i = 0; i < req->rows; ++i) total += req->value;
                  out->rows = total;
                  return OK;
                }
                """
                ast = compile_ast(source, ROOT, ROOT / "pgaccel-kernels/include")
                result = Verifier(ast).prove("metadata", ("R",), output_record=contract)
                self.assertFalse(result.ok, result)

    def test_argument_order_cannot_lend_stale_bounds_to_helper(self):
        prefix = """
        enum pgaccel_status { OK = 0, ERROR = 4 };
        struct R { unsigned long long rows, value; };
        unsigned long long scan(const R* req) {
          unsigned long long n = 0;
          for (unsigned i = 0; i < req->rows; ++i) n += req->value;
          return n;
        }
        unsigned long long combine(unsigned long long a, unsigned long long b) {
          return a + b;
        }
        extern "C" pgaccel_status metadata(const R* req, R* out) {
          if (req->rows > 4) return ERROR;
          out->rows =
        """
        contract = OutputRecord(
            "R", (("rows", "unsigned long long"), ("value", "unsigned long long")),
            frozenset({"rows"}), frozenset({"rows"}),
        )
        for expression, expected in (
            ("combine(scan(req), req->value)", True),
            ("combine(req->value, scan(req))", True),
            ("combine(scan(req), (out->rows = 1000))", False),
            ("combine((out->rows = 1000), scan(req))", False),
            ("combine(scan(req), ++out->rows)", False),
        ):
            with self.subTest(expression=expression):
                ast = compile_ast(
                    prefix + expression + "; return OK; }",
                    ROOT, ROOT / "pgaccel-kernels/include",
                )
                result = Verifier(ast).prove("metadata", ("R",), output_record=contract)
                self.assertEqual(result.ok, expected, result)


if __name__ == "__main__":
    unittest.main()
