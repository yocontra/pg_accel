from __future__ import annotations

import json
import pathlib
import sys
import tempfile
import textwrap
import unittest


SCRIPTS_DIR = pathlib.Path(__file__).resolve().parents[1]
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
                const int* values,
                size_t count) {
              sycl::queue& queue = get_queue();
              queue.parallel_for<Kernel>(sycl::range<1>(count), [=](sycl::id<1>) {});
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
        self.assertEqual(
            result.entrypoint_audits[0].classifications, ("device_dispatch",)
        )

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
                h.parallel_for<ReduceKernel<T>>(sycl::range<1>(count), [=](sycl::id<1>) {});
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

    def test_current_eleven_wrapper_shapes_pass_without_name_allowlists(self) -> None:
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
            pgaccel_status tree_reduce_sycl(const T*, size_t count, T*) {{
              sycl::queue& queue = get_queue();
              queue.parallel_for<ReduceKernel<T>>(sycl::range<1>(count), [=](sycl::id<1>) {{}});
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
        self.assertIn("first (line", finding.message)
        self.assertIn("second (line", finding.message)

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
        self.assertEqual(report["schema_version"], 1)
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
