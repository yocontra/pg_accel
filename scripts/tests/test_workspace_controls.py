"""Real-Clang regressions for the separate explicit-control prerequisite."""

from __future__ import annotations

import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from test_workspace_calls import CallVerifier, fixture, view  # noqa: E402
from workspace_calls import ControlCallsResult  # noqa: E402


CALL = "auto* output = address<int>(workspace, offset);"


def qualify(body, prelude="", transform=lambda source: source):
    compiler = view(transform(fixture(body, prelude)))
    verifier = CallVerifier(compiler)
    return verifier.prove_controls(
        "caller", "address", base_parameter=0, metadata_parameters=(1,)
    )


class WorkspaceControlsTests(unittest.TestCase):
    def reject(self, body, prelude="", transform=lambda source: source):
        result = qualify(body, prelude, transform)
        self.assertIsInstance(result, ControlCallsResult)
        self.assertFalse(result.ok, result)
        self.assertEqual(result.calls, ())
        self.assertEqual(result.offset_inputs, ())
        self.assertEqual(result.controls, ())
        self.assertTrue(result.source_sha256)
        return result

    def accept(self, body, prelude=""):
        result = qualify(body, prelude)
        self.assertTrue(result.ok, result.detail)
        self.assertTrue(result.calls)
        self.assertIn("caller not approved", result.detail)
        return result

    def test_old_witness_remains_accepted_only_by_old_api(self):
        body = "unsigned long offset=0; if (*layout.rows) offset=1; " + CALL
        verifier = CallVerifier(view(fixture(body)))
        old = verifier.prove(
            "caller", "address", base_parameter=0, metadata_parameters=(1,)
        )
        self.assertTrue(old.ok, old.detail)
        self.assertFalse(
            verifier.prove_controls(
                "caller", "address", base_parameter=0, metadata_parameters=(1,)
            ).ok
        )

    def test_metadata_dependency_ids_come_from_the_same_compiler_view(self):
        for body, names in (
            (
                "if(layout.offset) { auto* output=address<int>(workspace,0); }",
                {"layout"},
            ),
            (
                "auto* output=address<int>(workspace,layout.offsets[0]);",
                {"layout"},
            ),
            (
                "unsigned long selected=layout.offset; unsigned long transitive=selected; "
                "if(transitive) { auto* output=address<int>(workspace,layout.offsets[0]); }",
                {"layout", "selected", "transitive"},
            ),
        ):
            with self.subTest(body=body):
                verifier = CallVerifier(view(fixture(body)))
                function = next(
                    node
                    for node in verifier.definitions.values()
                    if node.get("name") == "caller"
                )

                def declarations(node):
                    if node.get("kind") in {"ParmVarDecl", "VarDecl"}:
                        yield node
                    for child in node.get("inner", ()):
                        yield from declarations(child)

                expected = {
                    node["id"]
                    for node in declarations(function)
                    if node.get("name") in names
                }
                self.assertEqual(len(expected), len(names))
                result = verifier.prove_controls(
                    "caller", "address", base_parameter=0, metadata_parameters=(1,)
                )
                self.assertTrue(result.ok, result.detail)
                self.assertEqual(set(result.offset_inputs), expected)
                old = verifier.prove(
                    "caller", "address", base_parameter=0, metadata_parameters=(1,)
                )
                self.assertTrue(old.ok, old.detail)
                layout_id = next(
                    node["id"]
                    for node in declarations(function)
                    if node.get("name") == "layout"
                )
                self.assertNotIn(layout_id, old.offset_inputs)

    def test_collected_metadata_dependencies_are_discarded_on_failure(self):
        for body in (
            "if(layout.offset) { auto* output=address<int>(workspace,layout.offsets[0]); } "
            "if(*layout.rows) return;",
            "auto* first=address<int>(workspace,layout.offset); "
            "auto* second=address<int>(workspace,*layout.rows);",
        ):
            with self.subTest(body=body):
                self.reject(body)

    def test_metadata_builtin_controls_and_loops(self):
        for body in (
            "unsigned long offset=layout.offset; if(index) { " + CALL + " }",
            "unsigned long offset=0; if(index) offset=1; else offset=2; " + CALL,
            "unsigned long offset=0; if(index) return; " + CALL,
            "for(unsigned long offset=0; offset<index; ++offset) { " + CALL + " }",
            "unsigned long offset=0; for(;offset<index;) { " + CALL + " ++offset; }",
            "unsigned long offset=0; for(;;) { " + CALL + " break; }",
            "unsigned long offset=0; while(offset<index) { " + CALL + " ++offset; }",
            "unsigned long offset=0; do { "
            + CALL
            + " ++offset; } while(offset<index);",
            "unsigned long offset=0; for(;offset<index;offset+=1) { if(offset==2) continue; "
            + CALL
            + " if(offset==3) break; }",
            "unsigned long offset=0; if(index && layout.offset || !index) offset=1; "
            + CALL,
            "unsigned long offset = index ? layout.offset : layout.offsets[0]; " + CALL,
            "unsigned long a=0,b=1; unsigned long offset=index ? a : b; " + CALL,
            "unsigned long offset=0,a=0,b=a; a=b; b+=a; if(b) offset=a; " + CALL,
            "unsigned long offset=0; { unsigned long index=1; if(index) offset=1; } "
            + CALL,
            "for(unsigned long i=0;i<2;++i) for(unsigned long j=0;j<2;++j) { unsigned long offset=layout.offsets[i+j]; "
            + CALL
            + " }",
        ):
            with self.subTest(body=body):
                self.accept(body)

    def test_row_controls_and_transitive_dependencies_fail(self):
        for body in (
            "unsigned long offset=0; if(*layout.rows) { " + CALL + " }",
            "unsigned long offset=0; bool gate=*layout.rows; if(gate) offset=1; "
            + CALL,
            "unsigned long offset=0; bool gate=false; gate=*layout.rows; if(gate) offset=1; "
            + CALL,
            "unsigned long offset=0; if(*layout.rows) return; " + CALL,
            "unsigned long offset=0; while(*layout.rows) { " + CALL + " }",
            "unsigned long offset=0; do { " + CALL + " } while(*layout.rows);",
            "for(unsigned long offset=0;offset<index;offset+=*layout.rows) { "
            + CALL
            + " }",
            "for(unsigned long offset=*layout.rows;offset<index;++offset) { "
            + CALL
            + " }",
            "unsigned long offset=0; for(;offset<index;++offset) { if(*layout.rows) break; "
            + CALL
            + " }",
            "unsigned long offset=0; for(;offset<index;++offset) { "
            + CALL
            + " if(*layout.rows) continue; }",
            "unsigned long offset=0; for(;offset<index;++offset) { if(*layout.rows) continue; "
            + CALL
            + " }",
            "unsigned long offset=0; for(;offset<index;++offset) { "
            + CALL
            + " if(*layout.rows) offset=1; }",
            "unsigned long offset=0; if(index && *layout.rows) offset=1; " + CALL,
            "unsigned long offset=0; if(index || *layout.rows) offset=1; " + CALL,
            "unsigned long offset=*layout.rows ? 0 : 1; " + CALL,
            "unsigned long offset=0; " + CALL + " if(*layout.rows) offset=1;",
        ):
            with self.subTest(body=body):
                self.reject(body)

    def test_atomic_call_sets_both_orders_and_control_spans(self):
        good = "auto* first=address<int>(workspace,0);"
        bad = "if(*layout.rows) { auto* second=address<int>(workspace,0); }"
        for body in (good + bad, bad + good):
            self.reject(body)
        result = self.accept("if(index) { auto* output=address<int>(workspace,0); }")
        self.assertTrue(result.controls)
        self.assertTrue(all(control.start < control.end for control in result.controls))

    def test_conditional_alias_stays_conditional_and_all_branches_checked(self):
        result = self.accept(
            "auto* output=index ? address<int>(workspace,0) : nullptr;"
        )
        self.assertTrue(result.calls[0].conditional)
        self.reject("auto* output=*layout.rows ? address<int>(workspace,0) : nullptr;")
        self.reject(
            "if(false && *layout.rows) {} auto* output=address<int>(workspace,0);"
        )

    def test_unknown_calls_overloads_and_hidden_storage(self):
        for condition, prelude in (
            ("hidden()", "bool hidden();"),
            ("index && hidden()", "bool hidden();"),
            ("predicate()", "bool (*predicate)();"),
            ("Flag{}", "struct Flag { operator bool() const; };"),
            ("Flag{} == Flag{}", "struct Flag {}; bool operator==(Flag,Flag);"),
            ("state", "thread_local unsigned long state;"),
            ("state", "volatile unsigned long state;"),
        ):
            with self.subTest(condition=condition):
                self.reject(
                    "unsigned long offset=0; if(" + condition + ") offset=1; " + CALL,
                    prelude,
                )
        self.reject(
            "unsigned long offset=0; static unsigned long state=hidden(); if(state) offset=1; "
            + CALL,
            "unsigned long hidden();",
        )
        self.reject(
            "unsigned long offset=0; volatile unsigned long state=1; if(state) offset=1; "
            + CALL
        )

    def test_metadata_mutation_and_escapes_anywhere_fail(self):
        mutations = (
            "layout.offset=0;",
            "layout.offsets[0]=0;",
            "auto& alias=layout.offset; alias=0;",
            "auto* alias=&layout.offset; *alias=0;",
            "auto& alias=layout; alias.offset=0;",
            "auto* alias=&layout; alias->offset=0;",
            "alter(layout);",
            "alter_scalar(layout.offset);",
            "auto& alias=layout.offset;",
        )
        for statement in mutations:
            for after in (False, True):
                call = "auto* output=address<int>(workspace,layout.offset);"
                body = call + statement if after else statement + call
                with self.subTest(statement=statement, after=after):
                    self.reject(
                        body,
                        "void alter(Layout&); void alter_scalar(unsigned long&);",
                        lambda source: source.replace(
                            "const Layout& layout", "Layout& layout"
                        ),
                    )
        self.accept("auto* output=address<int>(workspace,layout.offsets[index]);")

    def test_reference_fields_and_transitive_control_updates(self):
        self.reject(
            "if(layout.offset) { auto* output=address<int>(workspace,0); }",
            transform=lambda source: source.replace(
                "unsigned long offset;", "unsigned long& offset;", 1
            ),
        )
        self.reject(
            "unsigned long gate=0,other=*layout.rows; gate=other; if(gate) { auto* output=address<int>(workspace,0); }"
        )
        self.reject(
            "unsigned long gate=0; if(gate) { auto* output=address<int>(workspace,0); } gate=*layout.rows;"
        )

    def test_unsupported_structural_controls_and_if_initializers(self):
        for prefix in (
            "if(unsigned long gate=index; gate) {}",
            "if(unsigned long gate=index) {}",
            "while(unsigned long gate=index) {}",
            "for(;unsigned long gate=index;) {}",
            "goto label; label:;",
            "switch(index) {case 0: break; default: break;}",
            "try {} catch(...) {}",
            "if(index) throw 1;",
            'asm("");',
        ):
            with self.subTest(prefix=prefix):
                self.reject(prefix + " auto* output=address<int>(workspace,0);")
        self.reject(
            "unsigned long offset=0; for(;offset<index;hidden()) { " + CALL + " }",
            "void hidden();",
        )

    def test_returns_evaluate_effects(self):
        for expression in ("*layout.rows", "hidden()", "(hidden(),0)"):
            self.reject(
                "auto* output=address<int>(workspace,0); return " + expression + ";",
                "unsigned long hidden();",
                lambda source: source.replace("void caller(", "unsigned long caller("),
            )
        result = qualify(
            "auto* output=address<int>(workspace,0); return index;",
            transform=lambda source: source.replace(
                "void caller(", "unsigned long caller("
            ),
        )
        self.assertTrue(result.ok, result.detail)
        result = qualify(
            "auto* output=address<int>(workspace,0); return SUCCESS;",
            "enum Status { SUCCESS, FAILURE };",
            lambda source: source.replace("void caller(", "Status caller("),
        )
        self.assertTrue(result.ok, result.detail)

    def test_nested_closure_calls_and_capture_evaluation_fail_atomically(self):
        for body in (
            "auto* first=address<int>(workspace,0); auto fn=[&]{auto* second=address<int>(workspace,0);};",
            "auto fn=[&]{auto* output=address<int>(workspace,0);};",
            "unsigned long offset=0; auto fn=[&offset]{return offset;}; " + CALL,
            "auto fn=[gate=hidden()]{}; auto* output=address<int>(workspace,0);",
            "auto fn=[gate=*layout.rows]{}; auto* output=address<int>(workspace,0);",
        ):
            with self.subTest(body=body):
                self.reject(body, "unsigned long hidden();")
        self.accept("auto fn=[] {return 1;}; auto* output=address<int>(workspace,0);")

    def test_device_named_dispatch_does_not_approve_nested_helper(self):
        self.reject(
            "auto* first=address<int>(workspace,0); Queue queue; "
            "queue.single_task([=]{auto* second=address<int>(workspace,0);});",
            "struct Queue { template<class F> void single_task(F) {} };",
        )

    def test_plain_unknown_host_effect_is_an_obligation_not_approval(self):
        result = self.accept(
            "hidden(); auto* output=address<int>(workspace,0);", "void hidden();"
        )
        self.assertIn("external aliases", result.detail)

    def test_narrow_result_does_not_approve_host_output_writes(self):
        result = self.accept("auto* output=address<int>(workspace,0); *output=42;")
        self.assertIn("ordinary host effects", result.detail)
        self.assertIn("device execution unproved", result.detail)


if __name__ == "__main__":
    unittest.main()
