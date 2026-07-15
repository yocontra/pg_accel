from __future__ import annotations

import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


SCRIPTS_DIR = pathlib.Path(__file__).resolve().parents[1]
REPO_ROOT = SCRIPTS_DIR.parent
sys.path.insert(0, str(SCRIPTS_DIR))

import cpu_cheat_audit as audit  # noqa: E402


FIXTURE_PATH = pathlib.Path("fixture.cpp")


def audit_fixture(source: str) -> audit.FileAudit:
    return audit.audit_source(FIXTURE_PATH, textwrap.dedent(source))


def audit_compiling_fixture(source: str) -> audit.FileAudit:
    rendered = textwrap.dedent(source)
    compiler = shutil.which("clang++")
    if compiler is None:
        raise unittest.SkipTest("clang++ is required for compile-valid regressions")
    completed = subprocess.run(
        [compiler, "-std=c++17", "-fsyntax-only", "-x", "c++", "-"],
        input=rendered,
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise AssertionError(f"fixture is not valid C++:\n{completed.stderr}")
    return audit.audit_source(FIXTURE_PATH, rendered)


def finding_for(result: audit.FileAudit, name: str) -> audit.Finding:
    return next(finding for finding in result.findings if finding.entrypoint == name)


class EntrypointDiscoveryTests(unittest.TestCase):
    def test_multiline_direct_extern_c_definition_is_found(self) -> None:
        result = audit_fixture(
            r"""
            extern "C"
            pgaccel_status
            pgaccel_multiline(
                int* values,
                size_t count) {
              sycl::queue& queue = get_queue();
              queue.parallel_for<Kernel>(sycl::range<1>(count), [=](sycl::id<1> id) {
                values[id[0]] = 1;
              });
              return PGACCEL_OK;
            }
            """
        )
        self.assertEqual(result.entrypoints, 1)
        self.assertFalse(result.findings)

    def test_extern_c_linkage_block_definition_is_found(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" {
            pgaccel_status pgaccel_in_block(int* values, size_t count) {
              sycl::queue* q = get_queue();
              q->single_task<Kernel>([=]() { values[0] = count; });
              return PGACCEL_OK;
            }
            }
            """
        )
        self.assertEqual(result.entrypoints, 1)
        self.assertFalse(result.findings)

    def test_non_c_linkage_function_is_not_an_entrypoint(self) -> None:
        result = audit_fixture(
            """
            pgaccel_status pgaccel_cpp_only() { return PGACCEL_ERROR_NO_DEVICE; }
            """
        )
        self.assertEqual(result.entrypoints, 0)
        self.assertIn("no extern", result.findings[0].message)

    def test_duplicate_entrypoint_definition_is_ambiguous(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_duplicate(int) {
              return PGACCEL_ERROR_NO_DEVICE;
            }
            extern "C" pgaccel_status pgaccel_duplicate(float) {
              return PGACCEL_ERROR_NO_DEVICE;
            }
            """
        )
        finding = finding_for(result, "pgaccel_duplicate")
        self.assertIn("ambiguous_entrypoint", finding.classifications)

    def test_unbalanced_source_fails_closed(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_broken() {
              if (true) {
                return PGACCEL_ERROR;
            }
            """
        )
        self.assertEqual(result.findings[0].entrypoint, "<parser>")
        self.assertIn("parser_error", result.findings[0].classifications)

    def test_preprocessor_directive_contents_are_not_code(self) -> None:
        result = audit_fixture(
            r"""
            #define CHEAT_TEXT q.parallel_for( \
                something_with_an_unbalanced_brace({)
            extern "C" pgaccel_status pgaccel_preprocessed() {
              return PGACCEL_ERROR_NO_DEVICE;
            }
            """
        )
        self.assertFalse(result.findings)


class DeviceTerminalTests(unittest.TestCase):
    def test_typed_queue_parallel_for_passes(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_direct(int* out, size_t count) {
              sycl::queue& q = get_queue();
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
                out[id[0]] = 1;
              });
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn("device_dispatch", result.entrypoint_audits[0].classifications)

    def test_typed_handler_submit_passes(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_submit(int* out, size_t count) {
              sycl::queue* q = get_queue();
              q->submit([&](sycl::handler& h) {
                h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
                  out[id[0]] = 1;
                });
              });
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)

    def test_untyped_parallel_for_method_is_not_a_sycl_terminal(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_fake_method(FakeQueue& q) {
              q.parallel_for(123);
              return PGACCEL_OK;
            }
            """
        )
        finding = finding_for(result, "pgaccel_fake_method")
        self.assertIn("missing_device_terminal", finding.classifications)

    def test_comments_and_literals_cannot_masquerade_as_dispatch(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_fake_text(int* out, size_t count) {
              // sycl::queue& q = get_queue(); q.parallel_for(...);
              const char* normal = "q.submit([&] { h.parallel_for(...); })";
              const char* raw = R"tag(queue.single_task<Kernel>([] {}))tag";
              const char fake = '}';
              for (size_t i = 0; i < count; ++i) out[i] = normal[0] + raw[0] + fake;
              return PGACCEL_OK;
            }
            """
        )
        finding = finding_for(result, "pgaccel_fake_text")
        self.assertIn("host_computation", finding.classifications)

    def test_function_try_block_dispatch_is_parsed(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_try_block(int* out) try {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            } catch (...) {
              return PGACCEL_ERROR;
            }
            """
        )
        self.assertFalse(result.findings)


class CallGraphTests(unittest.TestCase):
    def test_multihop_template_wrapper_reaches_dispatch(self) -> None:
        result = audit_fixture(
            r"""
            template <typename T>
            pgaccel_status launch_reduce(const T* data, size_t count, T* out) {
              sycl::queue& q = get_queue();
              q.submit([&](sycl::handler& h) {
                h.parallel_for<ReduceKernel<T>>(sycl::range<1>(count), [=](sycl::id<1> id) {
                  out[id[0]] = data[id[0]];
                });
              });
              return PGACCEL_OK;
            }
            template <typename T>
            pgaccel_status typed_reduce(const T* data, size_t count, T* out) {
              return launch_reduce<T>(data, count, out);
            }
            extern "C" pgaccel_status pgaccel_wrapped_reduce(
                const int* data, size_t count, int* out) {
              return typed_reduce<int>(data, count, out);
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn("typed_reduce", result.entrypoint_audits[0].detail)
        self.assertIn("launch_reduce", result.entrypoint_audits[0].detail)


class ControlFlowEvasionTests(unittest.TestCase):
    def assert_rejected(self, source: str, name: str) -> audit.Finding:
        result = audit_fixture(source)
        finding = finding_for(result, name)
        self.assertFalse(
            result.entrypoint_audits[0].ok,
            f"{name} unexpectedly passed: {result.entrypoint_audits[0].detail}",
        )
        return finding

    def test_return_before_dispatch_is_not_dominated(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_return_first(int* out) {
              sycl::queue& q = get_queue();
              return PGACCEL_OK;
              q.single_task<Kernel>([=]() { *out = 1; });
            }
            """,
            "pgaccel_return_first",
        )
        self.assertIn("undominated_success", finding.classifications)

    def test_if_false_dispatch_is_dead(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_false_branch(int* out) {
              sycl::queue& q = get_queue();
              if (false) { q.single_task<Kernel>([=]() { *out = 1; }); }
              return PGACCEL_OK;
            }
            """,
            "pgaccel_false_branch",
        )
        self.assertIn("rejected_terminal", finding.classifications)

    def test_if_zero_preprocessor_dispatch_is_absent(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_if_zero(int* out) {
              sycl::queue& q = get_queue();
            #if 0
              q.single_task<Kernel>([=]() { *out = 1; });
            #endif
              return PGACCEL_OK;
            }
            """,
            "pgaccel_if_zero",
        )
        self.assertIn("missing_device_terminal", finding.classifications)

    def test_cpu_success_sibling_does_not_share_gpu_proof(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_sibling(bool gpu, int* out) {
              sycl::queue& q = get_queue();
              if (gpu) {
                q.single_task<Kernel>([=]() { *out = 1; });
                return PGACCEL_OK;
              } else {
                *out = 2;
                return PGACCEL_OK;
              }
            }
            """,
            "pgaccel_sibling",
        )
        self.assertIn("host_output_write", finding.classifications)

    def test_empty_submit_is_not_a_terminal(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_empty_submit(int* out) {
              sycl::queue& q = get_queue();
              q.submit([&](sycl::handler& h) { (void)h; });
              return PGACCEL_OK;
            }
            """,
            "pgaccel_empty_submit",
        )
        self.assertIn("rejected_terminal", finding.classifications)

    def test_uncalled_lambda_dispatch_is_deferred(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_uncalled(int* out) {
              sycl::queue& q = get_queue();
              auto never = [&]() { q.single_task<Kernel>([=]() { *out = 1; }); };
              (void)never;
              return PGACCEL_OK;
            }
            """,
            "pgaccel_uncalled",
        )
        self.assertIn("rejected_terminal", finding.classifications)

    def test_nearest_scoped_fake_queue_shadows_real_queue(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_shadowed(int* out) {
              sycl::queue& q = get_queue();
              { FakeQueue q; q.single_task<Kernel>([=]() { *out = 1; }); }
              return PGACCEL_OK;
            }
            """,
            "pgaccel_shadowed",
        )
        self.assertIn("rejected_terminal", finding.classifications)

    def test_cpu_success_catch_is_an_independent_path(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_success_catch(int* out) try {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            } catch (...) {
              *out = 2;
              return PGACCEL_OK;
            }
            """,
            "pgaccel_success_catch",
        )
        self.assertIn("CPU-success catch", finding.message)

    def test_unknown_preprocessor_condition_fails_closed(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_pp(int* out) {
              sycl::queue& q = get_queue();
            #if GPU_FEATURE_AVAILABLE
              q.single_task<Kernel>([=]() { *out = 1; });
            #else
              q.single_task<Kernel>([=]() { *out = 2; });
            #endif
              return PGACCEL_OK;
            }
            """,
            "pgaccel_pp",
        )
        self.assertIn("ambiguous_preprocessor_condition", finding.classifications)

    def test_false_identifier_preprocessor_condition_is_not_literal_false(self) -> None:
        finding = self.assert_rejected(
            r"""
            #define GPU_FEATURE_AVAILABLE 0
            extern "C" pgaccel_status pgaccel_pp_false(int* out) {
              sycl::queue& q = get_queue();
            #if GPU_FEATURE_AVAILABLE
              q.single_task<Kernel>([=]() { *out = 1; });
            #else
              q.single_task<Kernel>([=]() { *out = 2; });
            #endif
              return PGACCEL_OK;
            }
            """,
            "pgaccel_pp_false",
        )
        self.assertIn("ambiguous_preprocessor_condition", finding.classifications)

    def test_switch_bypass_fails_closed(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_switch(int mode, int* out) {
              sycl::queue& q = get_queue();
              switch (mode) {
                case 0: return PGACCEL_OK;
                default: break;
              }
              q.single_task<Kernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            """,
            "pgaccel_switch",
        )
        self.assertIn("ambiguous_control_flow", finding.classifications)

    def test_goto_bypass_remains_rejected(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_goto(bool bypass, int* out) {
              sycl::queue& q = get_queue();
              if (bypass) goto cpu_success;
              q.single_task<Kernel>([=]() { *out = 1; });
            cpu_success:
              return PGACCEL_OK;
            }
            """,
            "pgaccel_goto",
        )
        self.assertIn("ambiguous_control_flow", finding.classifications)

    def test_read_only_kernel_does_not_prove_output_contribution(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_read_only(int* out) {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() {
                int observed = *out;
                observed = 7;
              });
              return PGACCEL_OK;
            }
            """,
            "pgaccel_read_only",
        )
        self.assertIn("rejected_terminal", finding.classifications)

    def test_alias_host_overwrite_is_tracked(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_alias_write(int* out) {
              int* alias = out;
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              *alias = 2;
              return PGACCEL_OK;
            }
            """,
            "pgaccel_alias_write",
        )
        self.assertIn("host_output_write", finding.classifications)
        self.assertIn("output_alias_tracking", finding.classifications)

    def test_unresolved_host_finalizer_on_output_fails_closed(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_host_finalize(int* out, size_t count) {
              sycl::queue& q = get_queue();
              q.parallel_for<Kernel>(sycl::range<1>(count), [=](sycl::id<1> id) {
                out[id[0]] = 1;
              });
              host_finalize(out);
              return PGACCEL_OK;
            }
            """,
            "pgaccel_host_finalize",
        )
        self.assertIn("unresolved_output_helper", finding.classifications)

    def test_unresolved_member_finalizer_on_output_fails_closed(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_member_finalize(
                Output* out, size_t count) {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { out->value = count; });
              out->finalize();
              return PGACCEL_OK;
            }
            """,
            "pgaccel_member_finalize",
        )
        self.assertIn("unresolved_output_helper", finding.classifications)

    def test_invoked_host_lambda_output_write_is_not_hidden(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_host_lambda(int* out) {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              auto finalize = [&]() { *out = 2; };
              finalize();
              return PGACCEL_OK;
            }
            """,
            "pgaccel_host_lambda",
        )
        self.assertIn("deferred_host_output_write", finding.classifications)

    def test_ternary_success_path_requires_review(self) -> None:
        finding = self.assert_rejected(
            r"""
            pgaccel_status launch(int* out) {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_ternary(bool gpu, int* out) {
              return gpu ? launch(out) : PGACCEL_OK;
            }
            """,
            "pgaccel_ternary",
        )
        self.assertIn("ambiguous_control_flow", finding.classifications)

    def test_failure_only_path_with_host_compute_is_not_exempt(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_fail_compute(
                const int* input, size_t count, int* out) {
              for (size_t index = 0; index < count; ++index) out[index] = input[index];
              return PGACCEL_ERROR_NO_DEVICE;
            }
            """,
            "pgaccel_fail_compute",
        )
        self.assertIn("host_computation", finding.classifications)

    def test_lifecycle_evidence_in_dead_decoy_is_rejected(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_shutdown() {
              if (false) {
                g_queue->wait_and_throw();
                delete g_queue;
              }
              return PGACCEL_OK;
            }
            """,
            "pgaccel_shutdown",
        )
        self.assertIn("invalid_lifecycle_contract", finding.classifications)

    def test_lifecycle_evidence_after_return_is_rejected(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_shutdown() {
              return PGACCEL_OK;
              g_queue->wait_and_throw();
              delete g_queue;
            }
            """,
            "pgaccel_shutdown",
        )
        self.assertIn("invalid_lifecycle_contract", finding.classifications)

    def test_not_identifier_is_not_a_zero_work_contract(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_not_gpu(bool gpu, int* out) {
              if (!gpu) {
                *out = 42;
                return PGACCEL_OK;
              }
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            """,
            "pgaccel_not_gpu",
        )
        self.assertIn("host_output_write", finding.classifications)
        self.assertNotIn("zero_work", finding.classifications)

    def test_macro_hidden_export_fails_inventory(self) -> None:
        result = audit_fixture(
            r"""
            #define HIDDEN_EXPORT(name) extern "C" pgaccel_status pgaccel_##name
            HIDDEN_EXPORT(hidden)(int* out) { *out = 1; return PGACCEL_OK; }
            """
        )
        classifications = {
            classification
            for finding in result.findings
            for classification in finding.classifications
        }
        self.assertIn("macro_hidden_export", classifications)

    def test_return_alias_export_is_inventoried(self) -> None:
        result = audit_fixture(
            r"""
            using status_alias = pgaccel_status;
            extern "C" status_alias pgaccel_alias(int* out) {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            """
        )
        self.assertEqual(result.entrypoints, 1)
        self.assertEqual(result.non_status_entrypoints, 1)
        self.assertFalse(result.findings)

    def test_trailing_return_export_is_inventoried(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" auto pgaccel_trailing(int* out) -> pgaccel_status {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            """
        )
        self.assertEqual(result.status_entrypoints, 1)
        self.assertFalse(result.findings)

    def test_impossible_if_constexpr_specialization_fails_closed(self) -> None:
        finding = self.assert_rejected(
            r"""
            template <typename T>
            pgaccel_status typed(int* out) {
              if constexpr (std::is_same_v<T, float>) {
                sycl::queue& q = get_queue();
                q.single_task<Kernel>([=]() { *out = 1; });
                return PGACCEL_OK;
              }
              *out = 2;
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_i64(int* out) {
              return typed<int64_t>(out);
            }
            """,
            "pgaccel_i64",
        )
        self.assertIn("template_specialization_review", finding.classifications)

    def test_unrelated_gpu_helper_cannot_bless_cpu_output(self) -> None:
        finding = self.assert_rejected(
            r"""
            pgaccel_status launch_unrelated(int* scratch) {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *scratch = 7; });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_unrelated(int* out) {
              int scratch = 0;
              launch_unrelated(&scratch);
              *out = scratch + 1;
              return PGACCEL_OK;
            }
            """,
            "pgaccel_unrelated",
        )
        self.assertIn("host_output_write", finding.classifications)

    def test_singleton_cpu_success_is_not_zero_work(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_singleton(
                const int* data, size_t count, int* out) {
              if (count <= 1) { *out = data[0]; return PGACCEL_OK; }
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = data[0]; });
              return PGACCEL_OK;
            }
            """,
            "pgaccel_singleton",
        )
        self.assertIn("host_output_write", finding.classifications)
        self.assertNotIn("zero_work", finding.classifications)

    def test_recursive_output_helper_invalidates_later_dispatch(self) -> None:
        finding = self.assert_rejected(
            r"""
            pgaccel_status recurse(int* out) { return recurse(out); }
            extern "C" pgaccel_status pgaccel_recursive_first(int* out) {
              recurse(out);
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            """,
            "pgaccel_recursive_first",
        )
        self.assertIn("recursive_helper", finding.classifications)

    def test_output_copy_is_distinguished_as_ambiguous_staging(self) -> None:
        finding = self.assert_rejected(
            r"""
            extern "C" pgaccel_status pgaccel_staging(int* out) {
              sycl::queue& q = get_queue();
              int* device_value = sycl::malloc_device<int>(1, q);
              q.single_task<Kernel>([=]() { *device_value = 1; });
              q.memcpy(out, device_value, sizeof(int));
              return PGACCEL_OK;
            }
            """,
            "pgaccel_staging",
        )
        self.assertIn("host_staging_review", finding.classifications)
        self.assertNotIn("host_output_write", finding.classifications)

    def test_pure_gpu_wrapper_shapes_pass_without_name_allowlists(self) -> None:
        bit_wrappers = "\n".join(
            f"""
            extern "C" pgaccel_status pgaccel_reduce_bit_{operation}_i{width}(
                const int* data, size_t count, int* out) {{
              return reduce_bit_{operation}_kernel<int>(data, count, out);
            }}
            """
            for operation in ("and", "or", "xor")
            for width in (16, 32, 64)
        )
        result = audit_fixture(
            f"""
            template <typename T>
            pgaccel_status tree_reduce_sycl(const T* data, size_t count, T* out) {{
              sycl::queue& queue = get_queue();
              queue.parallel_for<ReduceKernel<T>>(sycl::range<1>(count), [=](sycl::id<1> id) {{
                out[id[0]] = data[id[0]];
              }});
              return PGACCEL_OK;
            }}
            template <typename T>
            pgaccel_status reduce_bit_and_kernel(const T* data, size_t count, T* out) {{
              return tree_reduce_sycl(data, count, out);
            }}
            template <typename T>
            pgaccel_status reduce_bit_or_kernel(const T* data, size_t count, T* out) {{
              return tree_reduce_sycl(data, count, out);
            }}
            template <typename T>
            pgaccel_status reduce_bit_xor_kernel(const T* data, size_t count, T* out) {{
              return tree_reduce_sycl(data, count, out);
            }}
            extern "C" pgaccel_status pgaccel_grouped_agg_execute_ex(int* out) {{
              sycl::queue& queue = get_queue();
              queue.single_task<GroupedKernel>([=]() {{ *out = 1; }});
              return PGACCEL_OK;
            }}
            extern "C" pgaccel_status pgaccel_grouped_agg_execute(int* out) {{
              return pgaccel_grouped_agg_execute_ex(out);
            }}
            extern "C" pgaccel_status pgaccel_h3_cell_to_parent_resident_ex(int* out) {{
              sycl::queue& queue = get_queue();
              queue.parallel_for<H3Kernel>(sycl::range<1>(1), [=](sycl::id<1>) {{ *out = 1; }});
              return PGACCEL_OK;
            }}
            extern "C" pgaccel_status pgaccel_h3_cell_to_parent_resident(int* out) {{
              return pgaccel_h3_cell_to_parent_resident_ex(out);
            }}
            {bit_wrappers}
            """
        )
        self.assertEqual(result.entrypoints, 13)
        self.assertFalse(result.findings)

    def test_wrapper_to_host_implementation_fails(self) -> None:
        result = audit_fixture(
            r"""
            pgaccel_status host_implementation(const int* in, size_t count, int* out) {
              for (size_t i = 0; i < count; ++i) out[i] = in[i] * 2;
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_host_wrapper(
                const int* in, size_t count, int* out) {
              return host_implementation(in, count, out);
            }
            """
        )
        finding = finding_for(result, "pgaccel_host_wrapper")
        self.assertIn("host_computation", finding.classifications)
        self.assertIn("host_implementation", finding.message)

    def test_unresolved_helper_fails(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_missing(int* out) {
              return missing_device_helper(out);
            }
            """
        )
        finding = finding_for(result, "pgaccel_missing")
        self.assertIn("unresolved_helper", finding.classifications)
        self.assertIn("missing_device_helper", finding.message)

    def test_recursive_helper_cycle_fails(self) -> None:
        result = audit_fixture(
            r"""
            pgaccel_status first(int* out);
            pgaccel_status second(int* out) { return first(out); }
            pgaccel_status first(int* out) { return second(out); }
            extern "C" pgaccel_status pgaccel_cycle(int* out) { return first(out); }
            """
        )
        finding = finding_for(result, "pgaccel_cycle")
        self.assertIn("recursive_helper", finding.classifications)
        self.assertIn("recursive helper cycle", finding.message)
        self.assertIn("first", finding.message)
        self.assertIn("second", finding.message)

    def test_same_arity_overload_is_ambiguous(self) -> None:
        result = audit_fixture(
            r"""
            pgaccel_status overloaded(int* out) {
              sycl::queue& q = get_queue();
              q.single_task<IntKernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            pgaccel_status overloaded(float* out) {
              sycl::queue& q = get_queue();
              q.single_task<FloatKernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_ambiguous(int* out) {
              return overloaded(out);
            }
            """
        )
        finding = finding_for(result, "pgaccel_ambiguous")
        self.assertIn("ambiguous_helper", finding.classifications)

    def test_arity_disambiguates_local_overloads(self) -> None:
        result = audit_fixture(
            r"""
            pgaccel_status launch(int* out) {
              sycl::queue& q = get_queue();
              q.single_task<Kernel>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            pgaccel_status launch(int* out, int value) {
              *out = value;
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_arity(int* out) { return launch(out); }
            """
        )
        self.assertFalse(result.findings)


class CompilerValidAdversarialTests(unittest.TestCase):
    CPP_PRELUDE = r"""
        using pgaccel_status = int;
        constexpr pgaccel_status PGACCEL_OK = 0;
        constexpr pgaccel_status PGACCEL_UNSUPPORTED = 9;
        namespace std {
        using size_t = decltype(sizeof(0));
        using uint32_t = unsigned int;
        template <class Output, class Size, class Value>
        Output fill_n(Output first, Size count, const Value& value) {
          while (count-- != 0) *first++ = value;
          return first;
        }
        }
        namespace sycl {
        template <int> struct range { explicit range(std::size_t) {} };
        template <int> struct id { operator std::size_t() const { return 0; } };
        struct queue {
          template <class Function> void parallel_for(range<1>, Function) {}
        };
        struct device { static void get_devices() {} };
        }
        void observe() {}
    """

    def test_seven_compiler_valid_output_bypasses_fail_closed(self) -> None:
        result = audit_compiling_fixture(
            self.CPP_PRELUDE
            + r"""
            struct Holder { int* out; };
            static void finish(int* out, std::size_t count) {
              std::fill_n(out, count, 7);
            }
            static auto host_finalize = &finish;
            #define EARLY_SUCCESS() if (flag) return PGACCEL_OK
            #define HOST_WRITE() out[0] = 7

            extern "C" pgaccel_status pgaccel_macro_early(
                bool flag, int mode, std::size_t count, int* out) {
              (void)mode;
              sycl::queue q;
              EARLY_SUCCESS();
              q.parallel_for(sycl::range<1>(count),
                             [=](sycl::id<1> i) { out[i] = 1; });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_macro_write(
                bool flag, int mode, std::size_t count, int* out) {
              (void)flag; (void)mode;
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count),
                             [=](sycl::id<1> i) { out[i] = 1; });
              HOST_WRITE();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_namespaced_finalizer(
                bool flag, int mode, std::size_t count, int* out) {
              (void)flag; (void)mode;
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count),
                             [=](sycl::id<1> i) { out[i] = 1; });
              std::fill_n(out, count, 7);
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_pointer_finalizer(
                bool flag, int mode, std::size_t count, int* out) {
              (void)flag; (void)mode;
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count),
                             [=](sycl::id<1> i) { out[i] = 1; });
              (*host_finalize)(out, count);
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_false_zero(
                bool flag, int mode, std::size_t count, int* out) {
              (void)flag;
              sycl::queue q;
              if (mode == 0) return PGACCEL_OK;
              q.parallel_for(sycl::range<1>(count),
                             [=](sycl::id<1> i) { out[i] = 1; });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_nested_deferred(
                bool flag, int mode, std::size_t count, int* out) {
              (void)flag; (void)mode;
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                auto deferred = [=]() { out[i] = 1; };
                (void)deferred;
              });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_member_collision(
                bool flag, int mode, std::size_t count, int* out) {
              (void)flag; (void)mode; (void)out;
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                Holder holder{};
                holder.out[i] = 1;
              });
              return PGACCEL_OK;
            }
            """
        )
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        self.assertEqual(
            set(entries),
            {
                "pgaccel_macro_early",
                "pgaccel_macro_write",
                "pgaccel_namespaced_finalizer",
                "pgaccel_pointer_finalizer",
                "pgaccel_false_zero",
                "pgaccel_nested_deferred",
                "pgaccel_member_collision",
            },
        )
        for name, entry in entries.items():
            with self.subTest(name=name):
                self.assertFalse(entry.ok, entry.detail)
        self.assertIn(
            "macro_expanded_body", entries["pgaccel_macro_early"].classifications
        )
        self.assertIn(
            "host_output_write", entries["pgaccel_macro_write"].classifications
        )
        self.assertIn(
            "qualified_helper", entries["pgaccel_namespaced_finalizer"].classifications
        )
        self.assertIn(
            "unresolved_indirect_output_call",
            entries["pgaccel_pointer_finalizer"].classifications,
        )
        self.assertNotIn("zero_work", entries["pgaccel_false_zero"].classifications)
        self.assertIn(
            "rejected_terminal", entries["pgaccel_nested_deferred"].classifications
        )
        self.assertIn(
            "rejected_terminal", entries["pgaccel_member_collision"].classifications
        )
        self.assertNotIn(
            "output_identity_shadowing",
            entries["pgaccel_member_collision"].classifications,
        )

    def test_two_exact_signature_contract_bypasses_fail_closed(self) -> None:
        result = audit_compiling_fixture(
            self.CPP_PRELUDE
            + r"""
            struct pgaccel_geometry {};
            static bool runtime_skip() { return true; }
            extern "C" pgaccel_status pgaccel_init() {
              if (!runtime_skip()) sycl::device::get_devices();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_spatial_intersects(
                const pgaccel_geometry*, std::size_t count_a,
                const pgaccel_geometry*, std::size_t count_b,
                std::uint32_t*, std::size_t*, std::uint32_t*, std::size_t*,
                std::uint32_t*, std::size_t*) {
              if (count_a == 0 && count_b == 0) return PGACCEL_OK;
              if (count_a != count_b) return PGACCEL_UNSUPPORTED;
              return PGACCEL_OK;
            }
            """
        )
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        self.assertFalse(entries["pgaccel_init"].ok)
        self.assertIn(
            "invalid_lifecycle_contract",
            entries["pgaccel_init"].classifications,
        )
        self.assertFalse(entries["pgaccel_spatial_intersects"].ok)
        self.assertIn(
            "invalid_failure_only_contract",
            entries["pgaccel_spatial_intersects"].classifications,
        )

    def test_contract_and_zero_work_adversarial_corpus_compiles(self) -> None:
        cases = {
            "lifecycle_runtime_conditional_evidence": r"""
                extern "C" pgaccel_status pgaccel_init(bool skip) {
                  if (!skip) sycl::device::get_devices();
                  return PGACCEL_OK;
                }
            """,
            "lifecycle_dead_evidence": r"""
                extern "C" pgaccel_status pgaccel_init() {
                  if (false) sycl::device::get_devices();
                  return PGACCEL_OK;
                }
            """,
            "lifecycle_after_return_evidence": r"""
                extern "C" pgaccel_status pgaccel_init() {
                  return PGACCEL_OK;
                  sycl::device::get_devices();
                }
            """,
            "fail_contract_scattered_evidence": r"""
                extern "C" pgaccel_status pgaccel_spatial_intersects(
                    std::size_t count_a, std::size_t count_b) {
                  if (count_a == 0 && count_b == 0) return PGACCEL_OK;
                  if (count_a != 0) return PGACCEL_UNSUPPORTED;
                  return PGACCEL_OK;
                }
            """,
            "fail_contract_unconditional_success_with_decoys": r"""
                extern "C" pgaccel_status pgaccel_spatial_intersects(
                    std::size_t count_a, std::size_t count_b) {
                  if (count_a == 0) observe();
                  if (count_b == 0) observe();
                  if (count_a != count_b) return PGACCEL_UNSUPPORTED;
                  return PGACCEL_OK;
                }
            """,
            "failure_only_host_write_macro": r"""
                #define HOST_WRITE() out[0] = 42
                extern "C" pgaccel_status pgaccel_spatial_intersects(
                    std::size_t count_a, std::size_t count_b, int* out) {
                  if (count_a == 0 || count_b == 0) return PGACCEL_OK;
                  HOST_WRITE();
                  return PGACCEL_UNSUPPORTED;
                }
            """,
            "false_zero_count_expected_conservative": r"""
                extern "C" pgaccel_status pgaccel_case(
                    std::size_t count, int* out) {
                  sycl::queue q;
                  if (count == 0) return PGACCEL_OK;
                  q.parallel_for(sycl::range<1>(count),
                                 [=](sycl::id<1> i) { out[i] = 1; });
                  return PGACCEL_OK;
                }
            """,
            "pointer_null_falsely_zero": r"""
                extern "C" pgaccel_status pgaccel_case(
                    std::size_t count, int* out) {
                  sycl::queue q;
                  if (out == 0) return PGACCEL_OK;
                  q.parallel_for(sycl::range<1>(count),
                                 [=](sycl::id<1> i) { out[i] = 1; });
                  return PGACCEL_OK;
                }
            """,
        }
        for name, source in cases.items():
            with self.subTest(name=name):
                result = audit_compiling_fixture(self.CPP_PRELUDE + source)
                entry = result.entrypoint_audits[0]
                if name == "false_zero_count_expected_conservative":
                    self.assertTrue(entry.ok, entry.detail)
                    self.assertIn("zero_work", entry.classifications)
                else:
                    self.assertFalse(entry.ok, entry.detail)


class HostComputationAndContractTests(unittest.TestCase):
    def test_host_loop_success_is_classified(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_cpu_loop(int* out, size_t count) {
              for (size_t i = 0; i < count; ++i) out[i] = 42;
              return PGACCEL_OK;
            }
            """
        )
        finding = finding_for(result, "pgaccel_cpu_loop")
        self.assertIn("host_computation", finding.classifications)
        self.assertIn("missing_device_terminal", finding.classifications)

    def test_fake_gpu_counter_is_classified_but_comment_is_not(self) -> None:
        real = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_fake_counter(int* out) {
              *out = 42;
              pgaccel_record_gpu_exec();
              return PGACCEL_OK;
            }
            """
        )
        self.assertIn(
            "fake_gpu_counter",
            finding_for(real, "pgaccel_fake_counter").classifications,
        )

        comment = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_counter_comment(int* out) {
              // pgaccel_record_gpu_exec();
              *out = 42;
              return PGACCEL_OK;
            }
            """
        )
        self.assertNotIn(
            "fake_gpu_counter",
            finding_for(comment, "pgaccel_counter_comment").classifications,
        )

    def test_explicit_failure_only_function_passes(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_decline() {
              if (something_bad()) return PGACCEL_INVALID_ARGUMENT;
              return PGACCEL_ERROR_NO_DEVICE;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertEqual(result.entrypoint_audits[0].classifications, ("failure_only",))

    def test_success_return_prevents_failure_only_classification(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_not_failure_only(bool empty) {
              if (empty) return PGACCEL_OK;
              return PGACCEL_ERROR_NO_DEVICE;
            }
            """
        )
        self.assertTrue(result.findings)

    def test_lifecycle_contract_requires_source_evidence(self) -> None:
        valid = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_shutdown() {
              g_queue->wait_and_throw();
              delete g_queue;
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(valid.findings)
        self.assertEqual(valid.entrypoint_audits[0].classifications, ("lifecycle",))

        invalid = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_shutdown() {
              return PGACCEL_OK;
            }
            """
        )
        finding = finding_for(invalid, "pgaccel_shutdown")
        self.assertIn("invalid_lifecycle_contract", finding.classifications)

        host_work = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_shutdown(int* out, size_t count) {
              g_queue->wait_and_throw();
              for (size_t i = 0; i < count; ++i) out[i] = 1;
              delete g_queue;
              return PGACCEL_OK;
            }
            """
        )
        finding = finding_for(host_work, "pgaccel_shutdown")
        self.assertIn("invalid_lifecycle_contract", finding.classifications)

    def test_failure_only_contract_rejects_new_host_work(self) -> None:
        valid = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_spatial_intersects(
                size_t count_a, size_t count_b, int* out) {
              *out = 0;
              if (count_a == 0 || count_b == 0) return PGACCEL_OK;
              return PGACCEL_UNSUPPORTED;
            }
            """
        )
        self.assertFalse(valid.findings)
        self.assertEqual(valid.entrypoint_audits[0].classifications, ("failure_only",))

        invalid = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_spatial_intersects(
                size_t count_a, size_t count_b, int* out) {
              if (count_a == 0 || count_b == 0) return PGACCEL_OK;
              for (size_t i = 0; i < count_a; ++i) out[i] = 1;
              return PGACCEL_UNSUPPORTED;
            }
            """
        )
        finding = finding_for(invalid, "pgaccel_spatial_intersects")
        self.assertIn("invalid_failure_only_contract", finding.classifications)


class AbiInventoryTests(unittest.TestCase):
    def test_alias_and_trailing_return_round_trip(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            header = root / "fixture.h"
            source = root / "fixture.cpp"
            header.write_text(
                textwrap.dedent(
                    r"""
                    using status_alias = pgaccel_status;
                    extern "C" {
                    status_alias pgaccel_alias(int* out);
                    auto pgaccel_trailing(int* out) -> status_alias;
                    }
                    """
                ),
                encoding="utf-8",
            )
            source.write_text(
                textwrap.dedent(
                    r"""
                    using status_alias = pgaccel_status;
                    extern "C" status_alias pgaccel_alias(int* out) {
                      return PGACCEL_ERROR_NO_DEVICE;
                    }
                    extern "C" auto pgaccel_trailing(int* out) -> status_alias {
                      return PGACCEL_ERROR_NO_DEVICE;
                    }
                    """
                ),
                encoding="utf-8",
            )
            inventory = audit.audit_abi([source], [header])
        self.assertFalse(inventory.findings)
        self.assertEqual(len(inventory.definitions), 2)
        self.assertEqual(len(inventory.declarations), 2)
        self.assertEqual(inventory.definition_hash, inventory.declaration_hash)

    def test_missing_extra_and_inactive_definitions_fail_inventory(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            header = root / "fixture.h"
            source = root / "fixture.cpp"
            header.write_text(
                'extern "C" pgaccel_status pgaccel_declared_only();\n',
                encoding="utf-8",
            )
            source.write_text(
                textwrap.dedent(
                    r"""
                    extern "C" pgaccel_status pgaccel_defined_only() {
                      return PGACCEL_ERROR_NO_DEVICE;
                    }
                    #if 0
                    extern "C" pgaccel_status pgaccel_inactive() {
                      return PGACCEL_OK;
                    }
                    #endif
                    """
                ),
                encoding="utf-8",
            )
            inventory = audit.audit_abi([source], [header])
        classifications = {
            item for finding in inventory.findings for item in finding.classifications
        }
        self.assertIn("missing_abi_definition", classifications)
        self.assertIn("extra_abi_definition", classifications)
        self.assertIn("preprocessor_inventory_mismatch", classifications)
        self.assertNotEqual(inventory.source_definition_hash, inventory.definition_hash)

    def test_parameter_type_mismatch_changes_full_signature_hash(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            header = root / "fixture.h"
            source = root / "fixture.cpp"
            header.write_text(
                'extern "C" pgaccel_status pgaccel_typed(float* out);\n',
                encoding="utf-8",
            )
            source.write_text(
                textwrap.dedent(
                    r"""
                    extern "C" pgaccel_status pgaccel_typed(int* out) {
                      return PGACCEL_ERROR_NO_DEVICE;
                    }
                    """
                ),
                encoding="utf-8",
            )
            inventory = audit.audit_abi([source], [header])
        classifications = {
            item for finding in inventory.findings for item in finding.classifications
        }
        self.assertIn("abi_signature_mismatch", classifications)
        self.assertNotEqual(inventory.definition_hash, inventory.declaration_hash)
        self.assertEqual(inventory.definitions[0].parameter_types, ("int *",))
        self.assertEqual(inventory.declarations[0].parameter_types, ("float *",))

    def test_compiler_inventory_expands_token_pasted_c_export(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            header = pathlib.Path(directory) / "token_paste.h"
            header.write_text(
                textwrap.dedent(
                    r"""
                    #define ABI_JOIN_INNER(left, right) left##right
                    #define ABI_JOIN(left, right) ABI_JOIN_INNER(left, right)
                    extern "C" int ABI_JOIN(pgaccel_, hidden)(float* out);
                    """
                ),
                encoding="utf-8",
            )
            compiler_inventory = audit.compiler_header_inventory([header])
            _, findings = audit.parse_declarations(
                header, header.read_text(encoding="utf-8")
            )
        self.assertEqual(
            [symbol.name for symbol in compiler_inventory.symbols],
            ["pgaccel_hidden"],
        )
        classifications = {
            item for finding in findings for item in finding.classifications
        }
        self.assertIn("token_paste_export_risk", classifications)

    def test_mutated_immutable_manifest_is_rejected(self) -> None:
        original = audit.DEFAULT_ABI_MANIFEST.read_text(encoding="utf-8")
        with tempfile.TemporaryDirectory() as directory:
            mutated = pathlib.Path(directory) / "manifest.txt"
            mutated.write_text(original.replace("void", "int", 1), encoding="utf-8")
            with self.assertRaisesRegex(audit.ParseError, "SHA-256 mismatch"):
                audit.load_abi_manifest(mutated)

    def test_checked_in_manifest_has_literal_integrity_anchor(self) -> None:
        manifest = audit.load_abi_manifest(audit.DEFAULT_ABI_MANIFEST)
        self.assertEqual(manifest.count, 167)
        self.assertEqual(audit.EXPECTED_ABI_MANIFEST_COUNT, 167)
        self.assertEqual(
            manifest.sha256,
            "3c8a3db2cd7a070af3ebf796cb7d3189add46959c4cc2eb481554478da4ab2c6",
        )
        self.assertEqual(manifest.sha256, audit.EXPECTED_ABI_MANIFEST_SHA256)

    def test_nm_object_union_is_bound_to_manifest_names(self) -> None:
        compiler = shutil.which("clang++")
        self.assertIsNotNone(compiler)
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            object_paths: list[pathlib.Path] = []
            for suffix in ("first", "second"):
                source = root / f"{suffix}.cpp"
                object_path = root / f"{suffix}.o"
                source.write_text(
                    f'extern "C" int pgaccel_object_{suffix}() {{ return 1; }}\n',
                    encoding="utf-8",
                )
                subprocess.run(
                    [compiler, "-c", str(source), "-o", str(object_path)],
                    check=True,
                    capture_output=True,
                    text=True,
                )
                object_paths.append(object_path)
            manifest = audit.AbiManifest(
                root / "manifest.txt",
                2,
                "fixture",
                {
                    "pgaccel_object_first": "int ()",
                    "pgaccel_object_second": "int ()",
                },
            )
            evidence, findings = audit._object_abi_evidence(object_paths, manifest)
        self.assertFalse(findings)
        self.assertEqual(evidence[0]["status"], "collected")
        self.assertEqual(evidence[0]["count"], 1)
        self.assertIn("binary_sha256", evidence[0])
        self.assertEqual(evidence[1]["status"], "collected")
        self.assertEqual(evidence[2]["kind"], "combined_object_inventory")
        self.assertEqual(evidence[2]["status"], "verified")
        self.assertEqual(evidence[2]["count"], 2)


class ReleaseWiringTests(unittest.TestCase):
    def test_precommit_keeps_fixture_gate_and_real_recipe_has_headers(self) -> None:
        justfile = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")
        precommit = next(
            line for line in justfile.splitlines() if line.startswith("pre-commit:")
        )
        self.assertIn("audit-cpu-cheats-test", precommit)
        recipe = justfile[justfile.index("audit-cpu-cheats: audit-cpu-cheats-test") :]
        self.assertIn("--headers pgaccel-kernels/include/*.h --", recipe)
        self.assertIn("--abi-manifest scripts/cpu_cheat_abi_manifest.txt", recipe)
        self.assertIn("update-cpu-cheat-abi-manifest:", justfile)

    def test_release_matrix_builds_before_real_audit(self) -> None:
        matrix = (REPO_ROOT / "scripts/release_verification_matrix.sh").read_text(
            encoding="utf-8"
        )
        self.assertLess(
            matrix.index('run_logged "gpu-build"'),
            matrix.index('run_logged "cpu-cheat-audit"'),
        )
        self.assertLess(
            matrix.index('run_logged "cpu-cheat-audit"'),
            matrix.index('run_logged "install-pg-accel"'),
        )

    def test_local_packages_run_real_audit_before_creating_artifacts(self) -> None:
        justfile = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")
        package = justfile[
            justfile.index('package pg="":') : justfile.index("package-matrix:")
        ]
        package_matrix = justfile[
            justfile.index("package-matrix:") : justfile.index(
                "install-pg-accel", justfile.index("package-matrix:")
            )
        ]
        self.assertLess(
            package.index("just audit-cpu-cheats"),
            package.index("cargo pgrx package"),
        )
        self.assertLess(
            package_matrix.index("just audit-cpu-cheats"),
            package_matrix.index("for pg in"),
        )
        self.assertIn("intentionally remains blocked", package)

    def test_green_ci_uses_integrity_suite_and_release_keeps_real_gate(self) -> None:
        ci = (REPO_ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        release = (REPO_ROOT / ".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )
        release_plz = (REPO_ROOT / ".github/workflows/release-plz.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("Run CPU-cheat analyzer and ABI integrity gate", ci)
        self.assertNotIn("Run real CPU-cheat audit", ci)
        self.assertLess(
            release.index("Build kernels before CPU-cheat release gate"),
            release.index("Run CPU-cheat release gate"),
        )
        self.assertLess(
            release_plz.index("Run real CPU-cheat release gate"),
            release_plz.index("MarcoIeni/release-plz-action"),
        )


class ProductionWitnessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.source_paths = sorted((REPO_ROOT / "pgaccel-kernels/src").glob("*.cpp"))
        cls.header_paths = sorted((REPO_ROOT / "pgaccel-kernels/include").glob("*.h"))
        cls.audits = audit.audit_paths(cls.source_paths)
        cls.by_name = {
            entry.entrypoint: entry
            for file_audit in cls.audits
            for entry in file_audit.entrypoint_audits
        }
        cls.findings = {
            finding.entrypoint: finding
            for file_audit in cls.audits
            for finding in file_audit.findings
        }
        cls.abi = audit.audit_abi(
            cls.source_paths,
            cls.header_paths,
            manifest_path=audit.DEFAULT_ABI_MANIFEST,
        )

    def test_complete_real_abi_baseline_and_violation_floor(self) -> None:
        self.assertEqual(len(self.abi.definitions), 167)
        self.assertEqual(len({item.name for item in self.abi.definitions}), 167)
        self.assertEqual(len({item.name for item in self.abi.declarations}), 167)
        self.assertFalse(self.abi.findings)
        self.assertEqual(self.abi.definition_hash, self.abi.declaration_hash)
        self.assertEqual(self.abi.source_definition_hash, self.abi.definition_hash)
        self.assertEqual(self.abi.manifest["status"], "verified")
        self.assertEqual(self.abi.compiler["status"], "verified")
        self.assertEqual(self.abi.compiler["inventory_count"], 167)
        status_failed = sum(
            entry.is_status and not entry.ok
            for file_audit in self.audits
            for entry in file_audit.entrypoint_audits
        )
        non_status_failed = sum(
            not entry.is_status and not entry.ok
            for file_audit in self.audits
            for entry in file_audit.entrypoint_audits
        )
        self.assertGreaterEqual(status_failed, 82)
        self.assertGreater(non_status_failed, 0)

    def test_inventory_hashes_are_deterministic(self) -> None:
        second = audit.audit_abi(
            self.source_paths,
            self.header_paths,
            manifest_path=audit.DEFAULT_ABI_MANIFEST,
        )
        self.assertEqual(second.definition_hash, self.abi.definition_hash)
        self.assertEqual(second.declaration_hash, self.abi.declaration_hash)
        self.assertEqual(second.per_file, self.abi.per_file)

    def test_point_in_polygon_all_outside_success_is_flagged(self) -> None:
        finding = self.findings["pgaccel_point_in_polygon_bulk"]
        self.assertEqual(finding.line, 2662)
        self.assertIn("host_computation", finding.classifications)
        self.assertIn("undominated_success", finding.classifications)

    def test_sort_i64_does_not_borrow_f32_constexpr_branch(self) -> None:
        finding = self.findings["pgaccel_sort_i64"]
        self.assertEqual(finding.line, 1606)
        self.assertIn("template_specialization_review", finding.classifications)

    def test_reduction_host_finalize_is_flagged(self) -> None:
        finding = self.findings["pgaccel_reduce_sum_f32"]
        self.assertIn("host_computation", finding.classifications)
        self.assertIn("host_output_write", finding.classifications)

    def test_non_status_hash_join_build_is_audited(self) -> None:
        entry = self.by_name["pgaccel_hash_join_build"]
        self.assertFalse(entry.is_status)
        self.assertFalse(entry.ok)
        self.assertEqual(entry.line, 882)

    def test_original_eleven_wrappers_keep_large_input_gpu_evidence(self) -> None:
        wrappers = [
            *(
                f"pgaccel_reduce_bit_{operation}_i{width}"
                for operation in ("and", "or", "xor")
                for width in (16, 32, 64)
            ),
            "pgaccel_grouped_agg_execute",
            "pgaccel_h3_cell_to_parent_resident",
        ]
        self.assertEqual(len(wrappers), 11)
        for name in wrappers:
            with self.subTest(name=name):
                entry = self.by_name[name]
                self.assertFalse(entry.ok)
                self.assertIn("large_input_gpu_chain", entry.classifications)


class ReportTests(unittest.TestCase):
    def test_json_report_contains_complete_classified_inventory(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_bad(int* out, size_t count) {
              for (size_t i = 0; i < count; ++i) out[i] = 1;
              pgaccel_record_gpu_exec();
              return PGACCEL_OK;
            }
            """
        )
        with tempfile.TemporaryDirectory() as directory:
            report_path = pathlib.Path(directory) / "report.json"
            audit._write_json_report(report_path, [result])
            report = json.loads(report_path.read_text(encoding="utf-8"))
        self.assertEqual(report["schema_version"], 3)
        self.assertEqual(report["status"], "fail")
        self.assertEqual(report["summary"]["entrypoints"], 1)
        self.assertEqual(report["summary"]["entrypoints_failed"], 1)
        self.assertEqual(report["summary"]["findings"], 1)
        self.assertEqual(
            report["summary"]["classification_counts"]["fake_gpu_counter"], 1
        )
        self.assertEqual(report["entrypoints"][0]["name"], "pgaccel_bad")
        self.assertIn("host_computation", report["findings"][0]["classifications"])
        self.assertIn("fake_gpu_counter", report["findings"][0]["classifications"])


if __name__ == "__main__":
    unittest.main()
