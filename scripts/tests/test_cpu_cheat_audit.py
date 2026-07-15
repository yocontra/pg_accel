from __future__ import annotations

import json
import pathlib
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


class ReleaseWiringTests(unittest.TestCase):
    def test_precommit_keeps_fixture_gate_and_real_recipe_has_headers(self) -> None:
        justfile = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")
        precommit = next(
            line for line in justfile.splitlines() if line.startswith("pre-commit:")
        )
        self.assertIn("audit-cpu-cheats-test", precommit)
        recipe = justfile[justfile.index("audit-cpu-cheats: audit-cpu-cheats-test") :]
        self.assertIn("--headers pgaccel-kernels/include/*.h --", recipe)

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

    def test_ci_and_tag_release_cannot_omit_real_gate(self) -> None:
        ci = (REPO_ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
        release = (REPO_ROOT / ".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )
        release_plz = (REPO_ROOT / ".github/workflows/release-plz.yml").read_text(
            encoding="utf-8"
        )
        self.assertLess(
            ci.index("Run standalone GPU kernel gate"),
            ci.index("Run real CPU-cheat audit after GPU build"),
        )
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
        cls.abi = audit.audit_abi(cls.source_paths, cls.header_paths)

    def test_complete_real_abi_baseline_and_violation_floor(self) -> None:
        self.assertEqual(len(self.abi.definitions), 167)
        self.assertEqual(len({item.name for item in self.abi.definitions}), 167)
        self.assertEqual(len({item.name for item in self.abi.declarations}), 167)
        self.assertFalse(self.abi.findings)
        self.assertEqual(self.abi.definition_hash, self.abi.declaration_hash)
        self.assertEqual(self.abi.source_definition_hash, self.abi.definition_hash)
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
        second = audit.audit_abi(self.source_paths, self.header_paths)
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
        self.assertEqual(report["schema_version"], 2)
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
