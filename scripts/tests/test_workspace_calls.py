from __future__ import annotations

import pathlib
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

from workspace_calls import CallVerifier  # noqa: E402
from workspace_effects import compile_view  # noqa: E402

HEADER = """
namespace witness {
struct Layout { unsigned long offset; unsigned long offsets[4]; unsigned long* rows; };
template<class T> T* address(void* base, unsigned long offset) {
  return reinterpret_cast<T*>(static_cast<unsigned char*>(base) + offset);
}
"""


def fixture(body, prelude=""):
    return (
        HEADER
        + prelude
        + "void caller(void* workspace, const Layout& layout, unsigned long index) {"
        + body
        + "}\n}"
    )


def view(source, names=frozenset({"address", "caller"})):
    return compile_view(
        source, ROOT, (), name_filter="witness::", retain_functions=names
    )


def prove(body, prelude="", metadata=(1,)):
    return CallVerifier(view(fixture(body, prelude))).prove(
        "caller",
        "address",
        base_parameter=0,
        metadata_parameters=metadata,
    )


class WorkspaceCallTests(unittest.TestCase):
    def test_literals_scalar_fields_fixed_arrays_and_reference_paths(self):
        for body in (
            "auto* output = address<int>(workspace, 8);",
            "auto* output = address<int>(workspace, index);",
            "auto* output = address<int>(workspace, layout.offset);",
            "auto* output = address<int>(workspace, layout.offsets[index]);",
            "const auto& alias = layout; "
            "auto* output = address<int>(workspace, alias.offset);",
            "unsigned long local = index; "
            "auto* output = address<int>(workspace, layout.offsets[local]);",
        ):
            with self.subTest(body=body):
                result = prove(body)
                self.assertTrue(result.ok, result.detail)
                self.assertEqual(len(result.calls), 1)
                call = result.calls[0]
                self.assertEqual(call.base_parameter, 0)
                self.assertEqual(call.destination_name, "output")
                self.assertIsNotNone(call.destination_id)
                self.assertLess(call.start, call.end)

    def test_records_require_explicit_metadata_contracts(self):
        result = prove(
            "auto* output = address<int>(workspace, layout.offset);", metadata=()
        )
        self.assertFalse(result.ok)
        self.assertEqual(result.calls, ())

    def test_row_pointer_reads_writes_and_callbacks_fail(self):
        prelude = "unsigned long hidden();"
        for offset in (
            "layout.rows[index]",
            "*layout.rows",
            "hidden()",
            "++index",
            "(index = 0)",
            "(hidden(), 0UL)",
        ):
            with self.subTest(offset=offset):
                result = prove(
                    f"auto* output = address<int>(workspace, {offset});", prelude
                )
                self.assertFalse(result.ok, result)
                self.assertEqual(result.calls, ())

    def test_implicit_class_conversions_fail_at_either_argument(self):
        prelude = """
struct Offset { operator unsigned long() const; };
struct Base { operator void*() const; };
"""
        for arguments in ("workspace, Offset{}", "Base{}, 0"):
            with self.subTest(arguments=arguments):
                result = prove(f"auto* output = address<int>({arguments});", prelude)
                self.assertFalse(result.ok, result)

    def test_unsafe_specialization_and_unseen_definition_fail(self):
        prelude = """
template<> int* address<int>(void* base, unsigned long offset) {
  *static_cast<int*>(base) = 42;
  return static_cast<int*>(base);
}
"""
        self.assertFalse(
            prove("auto* output = address<int>(workspace, 0);", prelude).ok
        )
        source = fixture("auto* output = address<int>(workspace, 0);")
        result = CallVerifier(view(source, frozenset({"caller"}))).prove(
            "caller",
            "address",
            base_parameter=0,
        )
        self.assertFalse(result.ok)
        self.assertIn("absent", result.detail)

    def test_compiler_selected_overload_not_shared_name_controls_the_result(self):
        overload = """
int* address(void* base, int offset) {
  *static_cast<int*>(base) = 42;
  return static_cast<int*>(base);
}
"""
        self.assertFalse(prove("auto* output = address(workspace, 0);", overload).ok)
        result = prove("auto* output = address<int>(workspace, 0);", overload)
        self.assertTrue(result.ok, result.detail)

    def test_indirect_helper_escape_invalidates_even_a_mixed_call_set(self):
        result = prove("""
auto* output = address<int>(workspace, 0);
auto fn = address<int>;
auto* second = fn(workspace, 4);
""")
        self.assertFalse(result.ok)
        self.assertEqual(result.calls, ())

    def test_partial_facts_are_discarded_on_a_later_failure(self):
        result = prove("""
auto* first = address<int>(workspace, 0);
auto* second = address<int>(workspace, layout.rows[0]);
""")
        self.assertFalse(result.ok)
        self.assertEqual(result.calls, ())

    def test_alias_facts_do_not_approve_later_host_writes(self):
        result = prove("auto* output = address<int>(workspace, 0); *output = 42;")
        self.assertTrue(result.ok, result.detail)
        self.assertEqual(result.calls[0].destination_name, "output")
        self.assertIn("caller not approved", result.detail)

    def test_const_assignment_member_and_conditional_aliases_are_preserved(self):
        cases = (
            ("const auto* output = address<int>(workspace, 0);", (), False),
            ("auto* const output = address<int>(workspace, 0);", (), False),
            ("int* output; output = address<int>(workspace, 0);", (), False),
            ("int* output = index ? address<int>(workspace, 0) : nullptr;", (), True),
            (
                "struct Holder { int* value; } output; "
                "output.value = address<int>(workspace, 0);",
                ("value",),
                False,
            ),
            (
                "struct Holder { int* values[4]; } output; "
                "output.values[index] = address<int>(workspace, 0);",
                ("values", "[*]"),
                False,
            ),
        )
        for body, path, conditional in cases:
            with self.subTest(body=body):
                result = prove(body)
                self.assertTrue(result.ok, result.detail)
                self.assertEqual(result.calls[0].destination_name, "output")
                self.assertEqual(result.calls[0].destination_path, path)
                self.assertEqual(result.calls[0].conditional, conditional)

    def test_metadata_pointer_field_cannot_be_laundered_via_a_reference(self):
        result = prove(
            "const auto& row = layout.rows[index]; "
            "auto* output = address<int>(workspace, row);"
        )
        self.assertFalse(result.ok, result)

    def test_reference_fields_are_not_plain_metadata_values(self):
        # Clang's MemberExpr type has already erased the field's reference.
        # A source-bound declared-type check must catch the implicit row read.
        for field in ("unsigned long& offset", "unsigned long (&offsets)[4]"):
            source = fixture(
                "auto* output = address<int>(workspace, "
                + ("layout.offset" if "offsets" not in field else "layout.offsets[0]")
                + ");"
            )
            source = source.replace(
                "unsigned long offset"
                if "offsets" not in field
                else "unsigned long offsets[4]",
                field,
                1,
            )
            with self.subTest(field=field):
                result = CallVerifier(view(source)).prove(
                    "caller",
                    "address",
                    base_parameter=0,
                    metadata_parameters=(1,),
                )
                self.assertFalse(result.ok, result)
                self.assertEqual(result.calls, ())

    def test_local_record_reads_require_a_trivial_metadata_copy(self):
        result = prove(
            "Layout local = layout; "
            "auto* output = address<int>(workspace, local.offset);"
        )
        self.assertTrue(result.ok, result.detail)
        self.assertEqual(len(result.calls[0].local_reads), 1)
        # An unrelated callback cannot be hidden in a local record initializer.
        result = prove(
            "Layout local = hidden(); "
            "auto* output = address<int>(workspace, local.offset);",
            "Layout hidden();",
        )
        self.assertFalse(result.ok, result)
        self.assertEqual(result.calls, ())

    def test_contract_indices_fail_closed(self):
        evidence = view(fixture("auto* output = address<int>(workspace, 0);"))
        for base, metadata in ((-1, ()), (8, ()), (0, (0,)), (0, (2,))):
            with self.subTest(base=base, metadata=metadata):
                result = CallVerifier(evidence).prove(
                    "caller",
                    "address",
                    base_parameter=base,
                    metadata_parameters=metadata,
                )
                self.assertFalse(result.ok)

    def test_workspace_parameter_reassignment_and_reference_escapes_fail(self):
        prelude = "void replace(void*&); void indirect(void**);"
        for statement in (
            "workspace = nullptr;",
            "workspace = layout.rows;",
            "workspace = static_cast<unsigned char*>(workspace) + 1;",
            "void*& alias = workspace; alias = nullptr;",
            "void** alias = &workspace; *alias = nullptr;",
            "replace(workspace);",
            "indirect(&workspace);",
            "reinterpret_cast<unsigned char*&>(workspace)++;",
            "auto change = [&workspace] { workspace = nullptr; }; change();",
            "auto escape = [&workspace] { return &workspace; }; *escape() = nullptr;",
        ):
            with self.subTest(statement=statement):
                result = prove(
                    statement + " auto* output = address<int>(workspace, 0);",
                    prelude,
                )
                self.assertFalse(result.ok, result)
                self.assertIn("workspace parameter", result.detail)
                self.assertEqual(result.calls, ())

    def test_workspace_assignment_after_call_or_on_backedge_is_not_ignored(self):
        for body in (
            "auto* output = address<int>(workspace, 0); workspace = nullptr;",
            "for (unsigned i=0; i<2; ++i) { "
            "auto* output = address<int>(workspace, 0); workspace = layout.rows; }",
            "if (index) workspace = nullptr; "
            "auto* output = address<int>(workspace, 0);",
            "if (!index) { auto* output = address<int>(workspace, 0); } "
            "else workspace = nullptr;",
        ):
            with self.subTest(body=body):
                result = prove(body)
                self.assertFalse(result.ok, result)
                self.assertEqual(result.calls, ())

    def test_pointer_value_copies_and_size_queries_preserve_base_identity(self):
        for body in (
            "void* copy = workspace; copy = nullptr; "
            "auto* output = address<int>(workspace, 0);",
            "read_value(workspace); auto* output = address<int>(workspace, 0);",
            "auto size = sizeof(workspace); "
            "auto* output = address<int>((workspace), 0);",
            "{ void* workspace = nullptr; workspace = layout.rows; } "
            "auto* output = address<int>(workspace, 0);",
        ):
            with self.subTest(body=body):
                result = prove(body, "void read_value(void*);")
                self.assertTrue(result.ok, result.detail)

    def test_stable_pointer_is_not_a_proof_of_pointee_effects(self):
        result = prove(
            "*static_cast<int*>(workspace) = 42; "
            "auto* output = address<int>(workspace, 0);"
        )
        self.assertTrue(result.ok, result.detail)
        self.assertIn("caller not approved", result.detail)

    def test_local_offset_initializers_cannot_hide_rows_or_callbacks(self):
        for initializer in (
            "layout.rows[index]",
            "*layout.rows",
            "hidden()",
            "(hidden(), 0UL)",
            "++index",
            "offset",
        ):
            with self.subTest(initializer=initializer):
                result = prove(
                    f"unsigned long offset = {initializer}; "
                    "auto* output = address<int>(workspace, offset);",
                    "unsigned long hidden();",
                )
                self.assertFalse(result.ok, result)
                self.assertEqual(result.calls, ())
        result = prove(
            "unsigned long offset; auto* output = address<int>(workspace, offset);"
        )
        self.assertFalse(result.ok, result)

    def test_local_offset_assignments_and_escapes_are_checked_after_calls_too(self):
        for statement in (
            "offset = layout.rows[0];",
            "offset += hidden();",
            "offset = other;",
            "replace(offset);",
            "indirect(&offset);",
            "const auto& alias = offset;",
            "auto escape = [&offset] { return &offset; };",
        ):
            for after in (False, True):
                call = "auto* output = address<int>(workspace, offset);"
                body = "unsigned long offset = 0; unsigned long other = hidden(); "
                body += call + statement if after else statement + call
                with self.subTest(statement=statement, after=after):
                    result = prove(
                        body,
                        "unsigned long hidden(); void replace(unsigned long&); void indirect(unsigned long*);",
                    )
                    self.assertFalse(result.ok, result)
                    self.assertEqual(result.calls, ())

    def test_scalar_parameter_updates_cannot_launder_row_values(self):
        result = prove(
            "index = layout.rows[0]; auto* output = address<int>(workspace, index);"
        )
        self.assertFalse(result.ok, result)
        self.assertEqual(result.offset_inputs, ())

    def test_metadata_induction_and_transitive_scalar_updates_pass(self):
        for body in (
            "unsigned long offset = layout.offset; offset += index; "
            "auto* output = address<int>(workspace, offset);",
            "for (unsigned long i=0; i<4; ++i) { "
            "auto* output = address<int>(workspace, layout.offsets[i]); }",
            "for (unsigned long i=index; i!=0; --i) { "
            "auto* output = address<int>(workspace, i); }",
            "unsigned long a=0,b=a; a=b; b+=a; "
            "auto* output = address<int>(workspace, b);",
        ):
            with self.subTest(body=body):
                result = prove(body)
                self.assertTrue(result.ok, result.detail)
                self.assertTrue(result.offset_inputs)

    def test_record_mutation_reference_escape_and_lifecycle_fail(self):
        for statement in (
            "local.offset = layout.rows[0];",
            "local.offset = 0;",  # record updates are deliberately not modeled
            "alter(local);",
            "auto* alias = &local;",
            "auto& alias = local;",
        ):
            with self.subTest(statement=statement):
                result = prove(
                    "Layout local = layout; "
                    + statement
                    + "auto* output = address<int>(workspace, local.offset);",
                    "void alter(const Layout&);",
                )
                self.assertFalse(result.ok, result)
        for member in (
            "Layout(const Layout&);",
            "~Layout();",
        ):
            source = fixture(
                "Layout local = layout; auto* output = address<int>(workspace, local.offset);"
            )
            source = source.replace("struct Layout {", "struct Layout { " + member)
            result = CallVerifier(view(source)).prove(
                "caller", "address", base_parameter=0, metadata_parameters=(1,)
            )
            self.assertFalse(result.ok, result)
            self.assertEqual(result.calls, ())

    def test_trivial_record_copies_compose_through_metadata_arrays(self):
        source = fixture("""
for (unsigned long i=0; i<4; ++i) {
  const auto level = layout.levels[i];
  const auto copy = level;
  auto* output = address<int>(workspace, copy.offset);
}
""").replace(
            "struct Layout {",
            "struct Level { unsigned long offset; }; struct Layout { Level levels[4];",
        )
        result = CallVerifier(view(source)).prove(
            "caller", "address", base_parameter=0, metadata_parameters=(1,)
        )
        self.assertTrue(result.ok, result.detail)
        self.assertEqual(len(result.offset_inputs), 3)

    def test_initializer_proof_does_not_approve_control_predicate_row_reads(self):
        result = prove(
            "unsigned long offset=0; if (*layout.rows) offset=1; auto* output=address<int>(workspace,offset);"
        )
        self.assertTrue(result.ok, result.detail)
        self.assertIn("caller not approved", result.detail)

    def test_record_value_capture_is_distinct_from_reference_escape(self):
        body = (
            "const Layout local = layout; "
            "auto read = [CAPTURE] { return local.offset; }; "
            "auto* output = address<int>(workspace, local.offset);"
        )
        value = prove(body.replace("CAPTURE", "local"))
        self.assertTrue(value.ok, value.detail)
        reference = prove(body.replace("CAPTURE", "&local"))
        self.assertFalse(reference.ok, reference)
        self.assertIn("LambdaExpr", reference.detail)
        self.assertEqual(reference.calls, ())


if __name__ == "__main__":
    unittest.main()
