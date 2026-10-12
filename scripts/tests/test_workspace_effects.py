from __future__ import annotations

import pathlib
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

from workspace_effects import Unproven, Verifier, compile_view  # noqa: E402


def prove(source):
    return Verifier(compile_view(source, ROOT, (), name_filter="address")).prove(
        "address"
    )


def helper(expression, prelude="", extra=""):
    return (
        prelude
        + "\ntemplate<class T> T* address(void* base, unsigned long offset) { return "
        + expression
        + "; }\n"
        + extra
        + "\nint* use(void* p) { return address<int>(p, 4); }\n"
    )


ADDRESS = "reinterpret_cast<T*>(static_cast<unsigned char*>(base) + offset)"


class WorkspaceEffectsTests(unittest.TestCase):
    def test_base_derived_pointer_and_nullable_sentinel(self):
        for expression, prelude, nullable in (
            (ADDRESS, "", False),
            (f"offset == ~0UL ? nullptr : {ADDRESS}", "", True),
            (
                f"offset == end ? nullptr : {ADDRESS}",
                "constexpr unsigned long end = ~0UL;",
                True,
            ),
            ("static_cast<T*>(base)", "", False),
        ):
            with self.subTest(expression=expression):
                result = prove(helper(expression, prelude))
                self.assertTrue(result.ok, result.detail)
                self.assertEqual(result.base_parameter, 0)
                self.assertEqual(result.nullable, nullable)
                self.assertEqual(result.definitions, 2)  # Template plus int instance.
                self.assertEqual(len(result.source_sha256), 64)

    def test_arbitrary_helper_name_is_not_an_approval(self):
        source = helper(ADDRESS).replace("address", "renamed")
        result = Verifier(compile_view(source, ROOT, (), name_filter="renamed")).prove(
            "renamed"
        )
        self.assertTrue(result.ok, result.detail)
        self.assertFalse(prove("void address() {} ").ok)

    def test_anonymous_record_pointers_remain_plain_pointers(self):
        source = (
            "namespace { struct Record { int value; };\n"
            + helper(ADDRESS)
            + "Record* record_use(void* p) { return address<Record>(p, 0); } }"
        )
        result = prove(source)
        self.assertTrue(result.ok, result.detail)
        self.assertEqual(result.definitions, 3)

    def test_body_proof_does_not_approve_call_site_conversions(self):
        source = (
            helper(ADDRESS)
            + """
struct Offset {
  int* out;
  operator unsigned long() const { *out = 42; return 0; }
};
int* bad_caller(int* out) { return address<int>(out, Offset{out}); }
"""
        )
        # Deliberately passes the helper BODY proof. A consumer must still
        # prove argument evaluation and propagate the returned pointer alias.
        # No result from this module currently changes the release audit.
        result = prove(source)
        self.assertTrue(result.ok, result.detail)

    def test_reads_writes_calls_and_reconstruction_fail_closed(self):
        prelude = "unsigned long hidden(); extern int* other;"
        for expression in (
            "reinterpret_cast<T*>(offset)",
            "reinterpret_cast<T*>(reinterpret_cast<unsigned long>(base) + offset)",
            "static_cast<T*>(other)",
            "(hidden(), static_cast<T*>(base))",
            "(*static_cast<int*>(base) = 42, static_cast<T*>(base))",
            "reinterpret_cast<T*>(static_cast<unsigned char*>(base) "
            "+ *static_cast<int*>(base))",
            "reinterpret_cast<T*>(static_cast<unsigned char*>(base) + hidden())",
        ):
            with self.subTest(expression=expression):
                # The foreign pointer case needs reinterpret_cast for unrelated T.
                expression = expression.replace(
                    "static_cast<T*>(other)", "reinterpret_cast<T*>(other)"
                )
                self.assertFalse(prove(helper(expression, prelude)).ok)

    def test_dynamic_const_tls_and_volatile_globals_are_not_constants(self):
        for declaration in (
            "unsigned long end;",
            "volatile unsigned long end;",
            "const unsigned long end = hidden();",
            "thread_local const unsigned long end = hidden();",
            "constexpr volatile unsigned long end = 0;",
        ):
            with self.subTest(declaration=declaration):
                result = prove(
                    helper(
                        f"offset == end ? nullptr : {ADDRESS}",
                        "unsigned long hidden(); " + declaration,
                    )
                )
                self.assertFalse(result.ok, result)

    def test_hidden_parameter_conversion_is_not_a_body_proof(self):
        # This helper's parameter is class-typed, even though source spelling
        # makes it look like an integer typedef. A conversion may run host work.
        source = """
struct Offset { operator unsigned long() const; };
using size_t = Offset;
int* address(void* base, size_t offset) {
  return reinterpret_cast<int*>(static_cast<unsigned char*>(base) + offset);
}
"""
        self.assertFalse(prove(source).ok)

    def test_overloaded_operators_and_side_effecting_specializations_fail(self):
        specialized = """
template<> int* address<int>(void* base, unsigned long offset) {
  *static_cast<int*>(base) = 42;
  return static_cast<int*>(base);
}
"""
        self.assertFalse(prove(helper(ADDRESS, extra=specialized)).ok)
        source = """
struct Evil {};
Evil operator+(Evil, unsigned long);
int* address(void* base, unsigned long offset) {
  return (Evil{} + offset, static_cast<int*>(base));
}
"""
        self.assertFalse(prove(source).ok)

    def test_local_raii_defaults_attributes_and_extra_statements_fail(self):
        for source in (
            "struct Guard { ~Guard(); }; int* address(void* p, unsigned long n) "
            "{ Guard guard; return static_cast<int*>(p); }",
            "unsigned long hidden(); int* address(void* p, unsigned long n = hidden()) "
            "{ return static_cast<int*>(p); }",
            "__attribute__((noinline)) int* address(void* p, unsigned long n) "
            "{ return static_cast<int*>(p); }",
            "int* address(void* p, unsigned long n) "
            "{ ++n; return static_cast<int*>(p); }",
            "int* address(void* p, unsigned long n, ...) "
            "{ return static_cast<int*>(p); }",
            "template<class T> T address(void* p, unsigned long n) { return p; }",
        ):
            with self.subTest(source=source):
                self.assertFalse(prove(source).ok)

    def test_absent_helpers_and_raw_json_do_not_prove_anything(self):
        self.assertFalse(prove("int unrelated; ").ok)
        with self.assertRaises(Unproven):
            Verifier({"kind": "FunctionDecl"})
        with self.assertRaises(Unproven):
            prove("int* address(void* p, unsigned long n) { syntax error }")


if __name__ == "__main__":
    unittest.main()
