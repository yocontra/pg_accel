from __future__ import annotations

import json
import os
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
    def test_kernel_derived_opaque_resource_return_and_publication_pass(self) -> None:
        result = audit_fixture(
            r"""
            struct OpaqueState { std::vector<int> payload; };
            OpaqueState* build_device_state(int* input, size_t count) {
              sycl::queue q;
              int* device_result = sycl::malloc_shared<int>(count, q);
              q.parallel_for<OpaqueBuild>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_result[id[0]] = input[id[0]] * 2;
              }).wait();
              auto* state = new OpaqueState();
              state->payload.resize(count);
              q.memcpy(state->payload.data(), device_result, count * sizeof(int)).wait();
              return state;
            }
            extern "C" OpaqueState* pgaccel_opaque_direct(int* input, size_t count) {
              return build_device_state(input, count);
            }
            extern "C" pgaccel_status pgaccel_opaque_return_then_publish(
                int* input, size_t count, OpaqueState** out) {
              OpaqueState* state = build_device_state(input, count);
              if (state == nullptr) return PGACCEL_ERROR;
              *out = state;
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_opaque_checked(
                int* input, size_t count, OpaqueState** out) {
              sycl::queue q;
              int* device_result = sycl::malloc_shared<int>(count, q);
              q.parallel_for<OpaqueChecked>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_result[id[0]] = input[id[0]] * 3;
              }).wait();
              auto* state = new OpaqueState();
              state->payload.resize(count);
              q.memcpy(state->payload.data(), device_result, count * sizeof(int)).wait();
              *out = state;
              return PGACCEL_OK;
            }
            extern "C" OpaqueState* pgaccel_opaque_checked_wrapper(
                int* input, size_t count) {
              OpaqueState* state = nullptr;
              const pgaccel_status status =
                  pgaccel_opaque_checked(input, count, &state);
              if (status != PGACCEL_OK) return nullptr;
              return state;
            }
            extern "C" OpaqueState* pgaccel_opaque_streamed(
                int* input, size_t count) {
              sycl::queue q;
              int* device_result = sycl::malloc_shared<int>(count, q);
              size_t begin = 0;
              do {
                q.parallel_for<OpaqueStream>(sycl::range<1>(1), [=](sycl::id<1>) {
                  device_result[begin] = input[begin] * 4;
                }).wait();
                ++begin;
              } while (begin < count);
              auto* state = new OpaqueState();
              state->payload.resize(count);
              q.memcpy(state->payload.data(), device_result, count * sizeof(int)).wait();
              return state;
            }
            """
        )
        self.assertEqual(result.entrypoints, 5)
        self.assertFalse(result.findings)
        for entry in result.entrypoint_audits:
            self.assertIn("opaque_device_resource", entry.classifications)

    def test_opaque_resource_hostile_provenance_mutants_fail(self) -> None:
        result = audit_fixture(
            r"""
            struct OpaqueState { std::vector<int> payload; };
            extern "C" OpaqueState* pgaccel_opaque_host_created(size_t count) {
              auto* state = new OpaqueState();
              state->payload.resize(count);
              state->payload[0] = 42;
              return state;
            }
            extern "C" OpaqueState* pgaccel_opaque_host_overwrite(
                int* input, size_t count) {
              sycl::queue q;
              int* device_result = sycl::malloc_shared<int>(count, q);
              q.parallel_for<OpaqueOverwrite>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_result[id[0]] = input[id[0]];
              }).wait();
              auto* state = new OpaqueState();
              state->payload.resize(count);
              q.memcpy(state->payload.data(), device_result, count * sizeof(int)).wait();
              state->payload[0] = 42;
              return state;
            }
            extern "C" OpaqueState* pgaccel_opaque_unawaited(
                int* input, size_t count) {
              sycl::queue q;
              int* device_result = sycl::malloc_shared<int>(count, q);
              q.parallel_for<OpaqueUnawaited>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_result[id[0]] = input[id[0]];
              }).wait();
              auto* state = new OpaqueState();
              state->payload.resize(count);
              q.memcpy(state->payload.data(), device_result, count * sizeof(int));
              return state;
            }
            pgaccel_status publish_to_second(
                OpaqueState* const* ignored, OpaqueState** out, int* input, size_t count) {
              (void)ignored;
              sycl::queue q;
              int* device_result = sycl::malloc_shared<int>(count, q);
              q.parallel_for<OpaqueSecond>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_result[id[0]] = input[id[0]];
              }).wait();
              auto* state = new OpaqueState();
              state->payload.resize(count);
              q.memcpy(state->payload.data(), device_result, count * sizeof(int)).wait();
              *out = state;
              return PGACCEL_OK;
            }
            extern "C" OpaqueState* pgaccel_opaque_wrong_binding(
                int* input, size_t count) {
              OpaqueState* state = nullptr;
              OpaqueState* other = nullptr;
              (void)publish_to_second(&state, &other, input, count);
              return state;
            }
            struct SplitState {
              std::vector<int> counts;
              std::vector<double> results;
            };
            extern "C" SplitState* pgaccel_opaque_dummy_copy_direct(
                int* input, size_t count, double host_value) {
              sycl::queue q;
              int* device_counts = sycl::malloc_shared<int>(count, q);
              q.parallel_for<DummyDirect>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_counts[id[0]] = input[id[0]];
              }).wait();
              auto* state = new SplitState();
              state->counts.resize(count);
              state->results.resize(count);
              state->results[0] = host_value;
              q.memcpy(state->counts.data(), device_counts, count * sizeof(int)).wait();
              return state;
            }
            extern "C" SplitState* pgaccel_opaque_dummy_copy_fill(
                int* input, size_t count, double host_value) {
              sycl::queue q;
              int* device_counts = sycl::malloc_shared<int>(count, q);
              q.parallel_for<DummyFill>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_counts[id[0]] = input[id[0]];
              }).wait();
              auto* state = new SplitState();
              state->counts.resize(count);
              state->results.resize(count);
              std::fill(state->results.begin(), state->results.end(), host_value);
              q.memcpy(state->counts.data(), device_counts, count * sizeof(int)).wait();
              return state;
            }
            extern "C" SplitState* pgaccel_opaque_dummy_copy_memcpy(
                int* input, size_t count, double host_value) {
              sycl::queue q;
              int* device_counts = sycl::malloc_shared<int>(count, q);
              q.parallel_for<DummyMemcpy>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_counts[id[0]] = input[id[0]];
              }).wait();
              auto* state = new SplitState();
              state->counts.resize(count);
              state->results.resize(count);
              std::memcpy(state->results.data(), &host_value, sizeof(host_value));
              q.memcpy(state->counts.data(), device_counts, count * sizeof(int)).wait();
              return state;
            }
            void host_finalize(std::vector<double>& values, double host_value) {
              values[0] = host_value;
            }
            extern "C" SplitState* pgaccel_opaque_dummy_copy_helper(
                int* input, size_t count, double host_value) {
              sycl::queue q;
              int* device_counts = sycl::malloc_shared<int>(count, q);
              q.parallel_for<DummyHelper>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_counts[id[0]] = input[id[0]];
              }).wait();
              auto* state = new SplitState();
              state->counts.resize(count);
              state->results.resize(count);
              host_finalize(state->results, host_value);
              q.memcpy(state->counts.data(), device_counts, count * sizeof(int)).wait();
              return state;
            }
            struct FixedState {
              std::vector<int> counts;
              double result[1];
            };
            extern "C" FixedState* pgaccel_opaque_dummy_copy_fixed_array(
                int* input, size_t count, double host_value) {
              sycl::queue q;
              int* device_counts = sycl::malloc_shared<int>(count, q);
              q.parallel_for<DummyFixed>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_counts[id[0]] = input[id[0]];
              }).wait();
              auto* state = new FixedState();
              state->counts.resize(count);
              state->result[0] = host_value;
              q.memcpy(state->counts.data(), device_counts, count * sizeof(int)).wait();
              return state;
            }
            extern "C" OpaqueState* pgaccel_opaque_usm_overwrite_after_kernel(
                int* input, size_t count, int host_value) {
              sycl::queue q;
              int* shared_result = sycl::malloc_shared<int>(count, q);
              q.parallel_for<OverwriteAfterKernel>(sycl::range<1>(count), [=](sycl::id<1> id) {
                shared_result[id[0]] = input[id[0]];
              }).wait();
              q.memcpy(shared_result, &host_value, sizeof(host_value)).wait();
              auto* state = new OpaqueState();
              state->payload.resize(count);
              q.memcpy(state->payload.data(), shared_result, count * sizeof(int)).wait();
              return state;
            }
            """
        )
        self.assertEqual(result.entrypoints, 10)
        self.assertEqual(len(result.findings), 10)
        for entry in result.entrypoint_audits:
            self.assertFalse(entry.ok, entry.detail)
            self.assertNotIn("opaque_device_resource", entry.classifications)

    def test_direct_pointer_return_requires_a_proven_opaque_helper(self) -> None:
        result = audit_fixture(
            r"""
            struct OpaqueState { std::vector<int> payload; };
            OpaqueState* build_device_state(size_t count) {
              sycl::queue q;
              int* device_result = sycl::malloc_shared<int>(count, q);
              q.parallel_for<DirectOpaque>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_result[id[0]] = static_cast<int>(id[0]);
              }).wait();
              auto* state = new OpaqueState();
              state->payload.resize(count);
              q.memcpy(state->payload.data(), device_result, count * sizeof(int)).wait();
              return state;
            }
            OpaqueState* build_host_state(size_t count) {
              auto* state = new OpaqueState();
              state->payload.resize(count);
              state->payload[0] = 42;
              return state;
            }
            extern "C" OpaqueState* pgaccel_direct_device_state(size_t count) {
              return build_device_state(count);
            }
            extern "C" OpaqueState* pgaccel_direct_host_state(size_t count) {
              return build_host_state(count);
            }
            """
        )
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        self.assertTrue(entries["pgaccel_direct_device_state"].ok)
        self.assertIn(
            "opaque_device_resource",
            entries["pgaccel_direct_device_state"].classifications,
        )
        self.assertFalse(entries["pgaccel_direct_host_state"].ok)
        self.assertNotIn(
            "opaque_device_resource",
            entries["pgaccel_direct_host_state"].classifications,
        )

    def test_cross_file_calls_require_unique_clean_device_proof_and_output_binding(
        self,
    ) -> None:
        def run(sources: dict[str, str]) -> dict[str, audit.EntrypointAudit]:
            with tempfile.TemporaryDirectory() as directory:
                paths = []
                for name, source in sources.items():
                    path = pathlib.Path(directory) / name
                    path.write_text(textwrap.dedent(source), encoding="utf-8")
                    paths.append(path)
                results = audit.audit_paths(paths)
            return {
                entry.entrypoint: entry
                for result in results
                for entry in result.entrypoint_audits
            }

        clean = run(
            {
                "target.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_gpu(int* out) {
                      sycl::queue q;
                      q.single_task<ExternalGpu>([=]() { *out = 7; }).wait();
                      return PGACCEL_OK;
                    }
                """,
                "caller.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_wrapper(int* out) {
                      return pgaccel_external_gpu(out);
                    }
                """,
            }
        )
        self.assertTrue(clean["pgaccel_external_gpu"].ok)
        self.assertTrue(clean["pgaccel_external_wrapper"].ok)
        self.assertIn(
            "audited_external_call",
            clean["pgaccel_external_wrapper"].classifications,
        )

        transitive = run(
            {
                "leaf.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_leaf(int* out) {
                      sycl::queue q;
                      q.single_task<ExternalLeaf>([=]() { *out = 7; }).wait();
                      return PGACCEL_OK;
                    }
                """,
                "middle.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_middle(int* out) {
                      return pgaccel_external_leaf(out);
                    }
                """,
                "top.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_top(int* out) {
                      return pgaccel_external_middle(out);
                    }
                """,
            }
        )
        self.assertTrue(transitive["pgaccel_external_leaf"].ok)
        self.assertTrue(transitive["pgaccel_external_middle"].ok)
        self.assertTrue(transitive["pgaccel_external_top"].ok)
        self.assertIn(
            "audited_external_call",
            transitive["pgaccel_external_top"].classifications,
        )

        cyclic = run(
            {
                "cycle_a.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_cycle_a(int* out) {
                      return pgaccel_external_cycle_b(out);
                    }
                """,
                "cycle_b.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_cycle_b(int* out) {
                      return pgaccel_external_cycle_a(out);
                    }
                """,
            }
        )
        self.assertFalse(cyclic["pgaccel_external_cycle_a"].ok)
        self.assertFalse(cyclic["pgaccel_external_cycle_b"].ok)
        self.assertNotIn(
            "audited_external_call",
            cyclic["pgaccel_external_cycle_a"].classifications,
        )

        host_only = run(
            {
                "target.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_host(int* out) {
                      *out = 7;
                      return PGACCEL_OK;
                    }
                """,
                "caller.cpp": r"""
                    extern "C" pgaccel_status pgaccel_host_target_wrapper(int* out) {
                      return pgaccel_external_host(out);
                    }
                """,
            }
        )
        self.assertFalse(host_only["pgaccel_external_host"].ok)
        self.assertFalse(host_only["pgaccel_host_target_wrapper"].ok)
        self.assertIn(
            "unresolved_helper",
            host_only["pgaccel_host_target_wrapper"].classifications,
        )

        unresolved = run(
            {
                "caller.cpp": r"""
                    extern "C" pgaccel_status pgaccel_missing_target_wrapper(int* out) {
                      return pgaccel_missing_external(out);
                    }
                """
            }
        )
        self.assertFalse(unresolved["pgaccel_missing_target_wrapper"].ok)
        self.assertIn(
            "unresolved_helper",
            unresolved["pgaccel_missing_target_wrapper"].classifications,
        )

        failure_only = run(
            {
                "target.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_decline(int* out) {
                      (void)out;
                      return PGACCEL_UNSUPPORTED;
                    }
                """,
                "caller.cpp": r"""
                    extern "C" pgaccel_status pgaccel_decline_target_wrapper(int* out) {
                      return pgaccel_external_decline(out);
                    }
                """,
            }
        )
        self.assertTrue(failure_only["pgaccel_external_decline"].ok)
        self.assertFalse(failure_only["pgaccel_decline_target_wrapper"].ok)
        self.assertIn(
            "unresolved_helper",
            failure_only["pgaccel_decline_target_wrapper"].classifications,
        )

        duplicate = run(
            {
                "target_a.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_duplicate(int* out) {
                      sycl::queue q;
                      q.single_task<ExternalDuplicateA>([=]() { *out = 1; }).wait();
                      return PGACCEL_OK;
                    }
                """,
                "target_b.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_duplicate(int* out) {
                      sycl::queue q;
                      q.single_task<ExternalDuplicateB>([=]() { *out = 2; }).wait();
                      return PGACCEL_OK;
                    }
                """,
                "caller.cpp": r"""
                    extern "C" pgaccel_status pgaccel_duplicate_target_wrapper(int* out) {
                      return pgaccel_external_duplicate(out);
                    }
                """,
            }
        )
        self.assertFalse(duplicate["pgaccel_duplicate_target_wrapper"].ok)
        self.assertIn(
            "unresolved_helper",
            duplicate["pgaccel_duplicate_target_wrapper"].classifications,
        )

        wrong_binding = run(
            {
                "target.cpp": r"""
                    extern "C" pgaccel_status pgaccel_external_bound(
                        const int* input, int* result) {
                      sycl::queue q;
                      q.single_task<ExternalBound>([=]() { *result = input[0]; }).wait();
                      return PGACCEL_OK;
                    }
                """,
                "caller.cpp": r"""
                    extern "C" pgaccel_status pgaccel_wrong_binding(int* out) {
                      sycl::queue q;
                      int* staging = sycl::malloc_shared<int>(1, q);
                      return pgaccel_external_bound(out, staging);
                    }
                """,
            }
        )
        self.assertTrue(wrong_binding["pgaccel_external_bound"].ok)
        self.assertFalse(wrong_binding["pgaccel_wrong_binding"].ok)
        self.assertNotIn(
            "audited_external_call",
            wrong_binding["pgaccel_wrong_binding"].classifications,
        )

    def test_neutral_output_init_requires_a_clean_returned_helper_on_every_success(
        self,
    ) -> None:
        result = audit_fixture(
            r"""
            struct OpaqueState { std::vector<int> payload; };
            pgaccel_status build_checked(
                int* input, size_t count, OpaqueState** out) {
              sycl::queue q;
              int* device_result = sycl::malloc_shared<int>(count, q);
              q.parallel_for<CheckedBuild>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_result[id[0]] = input[id[0]];
              }).wait();
              auto* state = new OpaqueState();
              state->payload.resize(count);
              q.memcpy(state->payload.data(), device_result, count * sizeof(int)).wait();
              *out = state;
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_checked_wrapper(
                int* input, size_t count, OpaqueState** out) {
              *out = nullptr;
              return build_checked(input, count, out);
            }
            extern "C" pgaccel_status pgaccel_checked_partial_wrapper(
                int* input, size_t count, bool run_device, OpaqueState** out) {
              *out = nullptr;
              if (run_device) return build_checked(input, count, out);
              return PGACCEL_OK;
            }
            """
        )
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        self.assertTrue(entries["pgaccel_checked_wrapper"].ok)
        self.assertIn(
            "opaque_device_resource",
            entries["pgaccel_checked_wrapper"].classifications,
        )
        self.assertFalse(entries["pgaccel_checked_partial_wrapper"].ok)
        self.assertIn(
            "undominated_success",
            entries["pgaccel_checked_partial_wrapper"].classifications,
        )

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

    def test_template_void_helper_and_false_terminal_preserve_device_proof(self) -> None:
        result = audit_fixture(
            r"""
            template <typename T>
            void write_device(T* out) {
              sycl::queue q;
              q.single_task<WriteDevice<T>>([=]() { *out = T{7}; }).wait();
            }
            template <typename T>
            bool write_if_ready(T* out, bool ready) {
              if (!ready) return false;
              write_device<T>(out);
              return true;
            }
            extern "C" pgaccel_status pgaccel_template_bool_write(
                int* out, bool ready) {
              if (!write_if_ready<int>(out, ready)) return PGACCEL_ERROR;
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn(
            "large_input_gpu_chain",
            result.entrypoint_audits[0].classifications,
        )

    def test_template_helper_with_trailing_default_reaches_dispatch(self) -> None:
        result = audit_fixture(
            r"""
            template <typename T, typename Op>
            pgaccel_status device_reduce(
                const T* data, size_t count, T* out, T identity, Op op,
                bool identity_from_first = false) {
              sycl::queue& q = get_queue();
              q.single_task<ReduceKernel<T>>([=]() {
                *out = identity_from_first ? data[0] : op(identity, data[0]);
              });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_defaulted_reduce(
                const int* data, size_t count, int* out) {
              return device_reduce<int>(data, count, out, 0,
                                        [](int a, int b) { return a + b; });
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn(
            "large_input_gpu_chain", result.entrypoint_audits[0].classifications
        )

    def test_default_argument_does_not_bless_host_output_helper(self) -> None:
        result = audit_fixture(
            r"""
            template <typename T>
            pgaccel_status host_reduce(
                const T* data, size_t count, T* out, bool ignored = false) {
              sycl::queue& q = get_queue();
              int unrelated = 0;
              q.single_task<Decoy>([=]() mutable { unrelated = 1; });
              for (size_t i = 0; i < count; ++i) *out += data[i];
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_defaulted_host_reduce(
                const int* data, size_t count, int* out) {
              return host_reduce<int>(data, count, out);
            }
            """
        )
        finding = finding_for(result, "pgaccel_defaulted_host_reduce")
        self.assertIn("host_computation", finding.classifications)
        self.assertNotIn(
            "large_input_gpu_chain", result.entrypoint_audits[0].classifications
        )

    def test_sycl_queue_reference_is_service_context_not_output(self) -> None:
        result = audit_fixture(
            r"""
            pgaccel_status copy_result(sycl::queue& q, int* out) {
              int* device_result = sycl::malloc_device<int>(1, q);
              q.single_task<Kernel>([=]() { *device_result = 42; }).wait_and_throw();
              q.memcpy(out, device_result, sizeof(int)).wait_and_throw();
              sycl::free(device_result, q);
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_queue_context(int* out) {
              sycl::queue& q = get_queue();
              return copy_result(q, out);
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn("device_copyback", result.entrypoint_audits[0].classifications)

    def test_sycl_queue_reference_cannot_hide_host_output(self) -> None:
        result = audit_fixture(
            r"""
            pgaccel_status host_result(sycl::queue& q, int* out) {
              int* unrelated = sycl::malloc_device<int>(1, q);
              q.single_task<Decoy>([=]() { *unrelated = 42; }).wait_and_throw();
              *out = 42;
              sycl::free(unrelated, q);
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_queue_context_host(int* out) {
              sycl::queue& q = get_queue();
              return host_result(q, out);
            }
            """
        )
        finding = finding_for(result, "pgaccel_queue_context_host")
        self.assertIn("host_output_write", finding.classifications)

    def test_const_helper_argument_is_validation_not_output_contribution(self) -> None:
        result = audit_fixture(
            r"""
            bool validate_value(const int* value) { return *value >= 0; }
            extern "C" pgaccel_status pgaccel_const_validation(
                int* out, size_t count) {
              if (!validate_value(out)) return PGACCEL_ERROR;
              sycl::queue q;
              q.parallel_for<Kernel>(sycl::range<1>(count),
                  [=](sycl::id<1> i) { out[i] = 1; });
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)

    def test_cast_away_const_helper_remains_output_contributing(self) -> None:
        result = audit_fixture(
            r"""
            pgaccel_status mutate_value(const int* value) {
              *const_cast<int*>(value) = 42;
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_const_mutation(
                int* out, int* decoy) {
              sycl::queue q;
              q.single_task<Kernel>([=]() { *decoy = 1; });
              pgaccel_status st = mutate_value(out);
              return st;
            }
            """
        )
        entry = result.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertIn("undominated_success", entry.classifications)
        self.assertIn("output helper mutate_value", entry.detail)

    def test_else_output_assignment_is_not_a_shadow_declaration(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_else_assignment(
                bool first, int* out) {
              if (first) *out = 0; else *out = 1;
              return PGACCEL_ERROR;
            }
            """
        )
        entry = result.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertNotIn("output_identity_shadowing", entry.classifications)

    def test_symbolic_validation_detail_can_accompany_device_output(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_validation_detail(
                int* out, int* detail, size_t count) {
              if (detail == nullptr) return PGACCEL_ERROR;
              *detail = PGACCEL_TEST_DETAIL_NONE;
              if (count == 0) return PGACCEL_OK;
              if (out == nullptr) {
                *detail = PGACCEL_TEST_DETAIL_CONTRACT;
                return PGACCEL_INVALID_ARGUMENT;
              }
              sycl::queue q;
              q.parallel_for<Kernel>(sycl::range<1>(count),
                  [=](sycl::id<1> i) { out[i] = 1; });
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn(
            "validation_metadata", result.entrypoint_audits[0].classifications
        )

    def test_validation_detail_rejects_input_or_result_writes(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_computed_detail(
                const int* data, int* out, int* detail, size_t count) {
              *detail = PGACCEL_TEST_DETAIL_NONE;
              if (data == nullptr) {
                *detail = count;
                return PGACCEL_INVALID_ARGUMENT;
              }
              sycl::queue q;
              q.single_task<Kernel>([=]() { *out = data[0]; });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_detail_decoy(
                int* result, int* decoy) {
              *result = PGACCEL_TEST_DETAIL_NONE;
              sycl::queue q;
              q.single_task<Decoy>([=]() { *decoy = 1; });
              return PGACCEL_OK;
            }
            """
        )
        for entry in result.entrypoint_audits:
            with self.subTest(entry.entrypoint):
                self.assertFalse(entry.ok, entry.detail)
                self.assertIn("host_output_write", entry.classifications)
        self.assertFalse(result.entrypoint_audits[0].ok)

    def test_explicit_trailing_return_device_lambda_is_structural(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_trailing_lambda(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              q.parallel_for<Kernel>(sycl::range<1>(count),
                  [=](sycl::id<1> i) -> void { out[i] = i[0] ? 1 : 0; });
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn("device_dispatch", result.entrypoint_audits[0].classifications)

    def test_explicit_trailing_return_host_lambda_remains_deferred(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_trailing_host_lambda(
                int* out, size_t count) {
              sycl::queue q;
              q.parallel_for<Kernel>(sycl::range<1>(count),
                  [=](sycl::id<1> i) { out[i] = 1; });
              auto finish = [&]() -> int { *out = 42; return 0; };
              finish();
              return PGACCEL_OK;
            }
            """
        )
        finding = finding_for(result, "pgaccel_trailing_host_lambda")
        self.assertIn("deferred_host_output_write", finding.classifications)

    def test_device_if_constexpr_does_not_require_host_specialization(self) -> None:
        result = audit_fixture(
            r"""
            template <bool First>
            pgaccel_status constexpr_device(sycl::queue& q, int* out) {
              q.single_task<Kernel<First>>([=]() {
                if constexpr (First) { *out = 1; } else { *out = 2; }
              });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_constexpr_device(int* out) {
              sycl::queue q;
              return constexpr_device<true>(q, out);
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertNotIn(
            "template_specialization_review",
            result.entrypoint_audits[0].classifications,
        )

    def test_exact_ternary_device_selection_proves_every_success_arm(self) -> None:
        result = audit_fixture(
            r"""
            pgaccel_status launch_a(sycl::queue& q, int* out) {
              q.single_task<KernelA>([=]() { *out = 1; });
              return PGACCEL_OK;
            }
            pgaccel_status launch_b(sycl::queue& q, int* out) {
              q.single_task<KernelB>([=]() { *out = 2; });
              return PGACCEL_OK;
            }
            pgaccel_status host_branch(int* out) {
              *out = 42;
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_device_selection(
                bool first, int* out) {
              sycl::queue q;
              return first ? launch_a(q, out) : launch_b(q, out);
            }
            extern "C" pgaccel_status pgaccel_host_selection(
                bool first, int* out) {
              sycl::queue q;
              return first ? launch_a(q, out) : host_branch(out);
            }
            extern "C" pgaccel_status pgaccel_success_selection(
                bool first, int* out) {
              sycl::queue q;
              return first ? launch_a(q, out) : PGACCEL_OK;
            }
            """
        )
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        valid = entries["pgaccel_device_selection"]
        self.assertTrue(valid.ok, valid.detail)
        self.assertIn("validated_device_selection", valid.classifications)
        for name in ("pgaccel_host_selection", "pgaccel_success_selection"):
            with self.subTest(name):
                self.assertFalse(entries[name].ok, entries[name].detail)
                self.assertIn("ambiguous_control_flow", entries[name].classifications)

    def test_typed_constant_identity_is_exact_zero_work(self) -> None:
        result = audit_fixture(
            r"""
            template <typename T>
            pgaccel_status typed_reduce(const T* data, size_t count, T* out) {
              if (count == 0) {
                *out = static_cast<T>(~T{0});
                return PGACCEL_OK;
              }
              sycl::queue& q = get_queue();
              q.single_task<Kernel<T>>([=]() { *out = data[0]; });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_typed_identity(
                const int* data, size_t count, int* out) {
              return typed_reduce<int>(data, count, out);
            }
            extern "C" pgaccel_status pgaccel_unit_identity(
                const uint8_t* data, size_t count, uint8_t* out) {
              if (count == 0) { *out = 1; return PGACCEL_OK; }
              sycl::queue& q = get_queue();
              q.single_task<UnitKernel>([=]() { *out = data[0]; });
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        for entry in result.entrypoint_audits:
            self.assertIn("zero_work", entry.classifications)

    def test_zero_work_identity_cannot_read_input_or_call_host(self) -> None:
        result = audit_fixture(
            r"""
            int host_identity();
            extern "C" pgaccel_status pgaccel_zero_reads_input(
                const int* data, size_t count, int* out) {
              if (count == 0) { *out = data[0]; return PGACCEL_OK; }
              sycl::queue& q = get_queue();
              q.single_task<ReadKernel>([=]() { *out = data[0]; });
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_zero_calls_host(
                const int* data, size_t count, int* out) {
              if (count == 0) { *out = host_identity(); return PGACCEL_OK; }
              sycl::queue& q = get_queue();
              q.single_task<CallKernel>([=]() { *out = data[0]; });
              return PGACCEL_OK;
            }
            """
        )
        for name in ("pgaccel_zero_reads_input", "pgaccel_zero_calls_host"):
            with self.subTest(name=name):
                finding = finding_for(result, name)
                self.assertIn("host_output_write", finding.classifications)


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

    def test_nonstatus_exact_null_decline_is_failure_only(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" void* pgaccel_decline_nullptr(const int* data, size_t count) {
              (void)data;
              (void)count;
              return nullptr;
            }
            extern "C" void* pgaccel_decline_null(const int* data) {
              (void)data;
              return NULL;
            }
            """
        )
        self.assertEqual(result.entrypoints, 2)
        self.assertFalse(result.findings)
        for entry in result.entrypoint_audits:
            self.assertIn("failure_only", entry.classifications)
            self.assertIn("exact null", entry.detail)

    def test_nonstatus_decline_hostile_success_and_gpu_mutants_fail(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" void* pgaccel_nonnull_mutant() {
              static int value = 1;
              return &value;
            }
            extern "C" void* pgaccel_conditional_mutant(bool succeed) {
              static int value = 1;
              if (succeed) return &value;
              return nullptr;
            }
            extern "C" void* pgaccel_launch_then_null_mutant(int* out) {
              sycl::queue q;
              q.single_task<LaunchThenNull>([=]() { *out = 1; }).wait();
              return nullptr;
            }
            extern "C" void* pgaccel_counter_then_null_mutant() {
              pgaccel_record_gpu_exec();
              return nullptr;
            }
            extern "C" int pgaccel_integer_zero_mutant() {
              return 0;
            }
            pgaccel_status clean_gpu_helper(int* out) {
              sycl::queue q;
              q.single_task<HelperThenNull>([=]() { *out = 1; }).wait();
              return PGACCEL_OK;
            }
            extern "C" void* pgaccel_helper_then_null_mutant(int* out) {
              (void)clean_gpu_helper(out);
              return nullptr;
            }
            """
        )
        self.assertEqual(result.entrypoints, 6)
        self.assertEqual(len(result.findings), 6)
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        for entry in entries.values():
            self.assertFalse(entry.ok, entry.detail)
            self.assertNotIn("failure_only", entry.classifications)
        self.assertIn(
            "missing_device_terminal", entries["pgaccel_nonnull_mutant"].classifications
        )
        self.assertIn(
            "missing_device_terminal",
            entries["pgaccel_conditional_mutant"].classifications,
        )
        self.assertIn(
            "device_dispatch",
            entries["pgaccel_launch_then_null_mutant"].classifications,
        )
        self.assertIn(
            "fake_gpu_counter",
            entries["pgaccel_counter_then_null_mutant"].classifications,
        )

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
    def test_ooo_overlap_diagnostic_contract_is_exact(self) -> None:
        valid = audit_fixture(
            r"""
            struct pgaccel_ooo_overlap_report { uint64_t wall_ns; };
            extern "C" pgaccel_status pgaccel_sort_window_overlap_probe(
                size_t count, uint32_t spin, pgaccel_ooo_overlap_report* out) {
              if (out == nullptr) return PGACCEL_ERROR;
              std::memset(out, 0, sizeof(*out));
              auto* ooo = pgaccel_get_ooo_queue();
              if (ooo == nullptr || ooo->is_in_order()) return PGACCEL_UNSUPPORTED;
              for (size_t i = 0; i < count; ++i) { (void)i; }
              run_probe_once();
              pgaccel_record_gpu_exec();
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(valid.findings)
        self.assertIn(
            "lifecycle",
            valid.entrypoint_audits[0].classifications,
        )

        for body in (
            r"""
              if (out == nullptr) return PGACCEL_ERROR;
              auto* ooo = pgaccel_get_ooo_queue();
              if (ooo == nullptr || ooo->is_in_order()) return PGACCEL_UNSUPPORTED;
              pgaccel_record_gpu_exec();
              return PGACCEL_OK;
            """,
            r"""
              if (out == nullptr) return PGACCEL_ERROR;
              auto* ooo = pgaccel_get_ooo_queue();
              if (ooo == nullptr || ooo->is_in_order()) return PGACCEL_UNSUPPORTED;
              run_probe_once();
              sycl::queue q;
              q.single_task<SubstituteProbe>([=]() { out->wall_ns = 1; }).wait();
              pgaccel_record_gpu_exec();
              return PGACCEL_OK;
            """,
        ):
            hostile = audit_fixture(
                "struct pgaccel_ooo_overlap_report { uint64_t wall_ns; };\n"
                'extern "C" pgaccel_status pgaccel_sort_window_overlap_probe('
                "size_t count, uint32_t spin, pgaccel_ooo_overlap_report* out) {\n"
                + textwrap.dedent(body)
                + "}\n"
            )
            with self.subTest(body=body):
                self.assertTrue(hostile.findings)
                self.assertIn(
                    "invalid_lifecycle_contract",
                    hostile.entrypoint_audits[0].classifications,
                )

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

    def test_neutral_output_init_on_unconditional_failure_edge(self) -> None:
        valid = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_failure_init(
                int* out, size_t count, bool invalid) {
              if (invalid) {
                if (out != nullptr) *out = 0;
                return PGACCEL_ERROR;
              }
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count),
                  [=](sycl::id<1> i) { out[i] = 1; });
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(valid.findings)
        self.assertIn(
            "failure_neutral_init", valid.entrypoint_audits[0].classifications
        )

        hostile = {
            "non_neutral": r"""
                extern "C" pgaccel_status pgaccel_non_neutral(
                    int* out, size_t count, bool invalid) {
                  if (invalid) { *out = 42; return PGACCEL_ERROR; }
                  sycl::queue q;
                  q.parallel_for(sycl::range<1>(count),
                      [=](sycl::id<1> i) { out[i] = 1; });
                  return PGACCEL_OK;
                }
            """,
            "conditional_failure": r"""
                extern "C" pgaccel_status pgaccel_conditional_failure(
                    int* out, size_t count, bool invalid, bool fatal) {
                  if (invalid) {
                    *out = 0;
                    if (fatal) return PGACCEL_ERROR;
                  }
                  sycl::queue q;
                  q.parallel_for(sycl::range<1>(count),
                      [=](sycl::id<1> i) { out[i] = 1; });
                  return PGACCEL_OK;
                }
            """,
            "early_success": r"""
                extern "C" pgaccel_status pgaccel_early_success(
                    int* out, size_t count, bool invalid, bool recover) {
                  if (invalid) {
                    *out = 0;
                    if (recover) return PGACCEL_OK;
                    return PGACCEL_ERROR;
                  }
                  sycl::queue q;
                  q.parallel_for(sycl::range<1>(count),
                      [=](sycl::id<1> i) { out[i] = 1; });
                  return PGACCEL_OK;
                }
            """,
            "later_failure": r"""
                extern "C" pgaccel_status pgaccel_later_failure(
                    int* out, size_t count, bool invalid, bool fatal) {
                  if (invalid) *out = 0;
                  if (fatal) return PGACCEL_ERROR;
                  sycl::queue q;
                  q.parallel_for(sycl::range<1>(count),
                      [=](sycl::id<1> i) { out[i] = 1; });
                  return PGACCEL_OK;
                }
            """,
            "break_escape": r"""
                extern "C" pgaccel_status pgaccel_break_escape(
                    int* out, size_t count, bool invalid, bool recover) {
                  if (invalid) {
                    while (recover) { *out = 0; break; }
                    return PGACCEL_ERROR;
                  }
                  sycl::queue q;
                  q.parallel_for(sycl::range<1>(count),
                      [=](sycl::id<1> i) { out[i] = 1; });
                  return PGACCEL_OK;
                }
            """,
            "goto_escape": r"""
                extern "C" pgaccel_status pgaccel_goto_escape(
                    int* out, size_t count, bool invalid) {
                  if (invalid) { *out = 0; goto failed; }
                  sycl::queue q;
                  q.parallel_for(sycl::range<1>(count),
                      [=](sycl::id<1> i) { out[i] = 1; });
                  return PGACCEL_OK;
                failed:
                  return PGACCEL_ERROR;
                }
            """,
            "escaping_alias": r"""
                extern "C" pgaccel_status pgaccel_alias_failure_init(
                    int* out, size_t count, bool invalid) {
                  int* alias = out;
                  if (invalid) { *alias = 0; return PGACCEL_ERROR; }
                  sycl::queue q;
                  q.parallel_for(sycl::range<1>(count),
                      [=](sycl::id<1> i) { out[i] = 1; });
                  return PGACCEL_OK;
                }
            """,
        }
        for name, source in hostile.items():
            with self.subTest(name):
                result = audit_fixture(source)
                self.assertTrue(result.findings)
                self.assertIn(
                    "host_output_write", result.entrypoint_audits[0].classifications
                )

    def test_zero_work_only_success_with_nonempty_failure_passes(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_zero_only(
                const int* data, size_t count, int* out) {
              if (count == 0) {
                if (out != nullptr) *out = 0;
                return PGACCEL_OK;
              }
              if (data == nullptr || out == nullptr) return PGACCEL_ERROR;
              return PGACCEL_UNSUPPORTED;
            }
            """
        )
        self.assertFalse(result.findings)
        entry = result.entrypoint_audits[0]
        self.assertIn("failure_only", entry.classifications)
        self.assertIn("zero_work", entry.classifications)

    def test_zero_work_only_contract_rejects_nonempty_host_output(self) -> None:
        result = audit_fixture(
            r"""
            extern "C" pgaccel_status pgaccel_zero_only_host(
                const int* data, size_t count, int* out) {
              if (count == 0) { *out = 0; return PGACCEL_OK; }
              *out = data[0];
              return PGACCEL_UNSUPPORTED;
            }
            """
        )
        finding = finding_for(result, "pgaccel_zero_only_host")
        self.assertIn("host_output_write", finding.classifications)

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

    def test_failure_only_named_detail_is_diagnostic_only(self) -> None:
        valid = audit_compiling_fixture(
            r"""
            using pgaccel_status = int;
            constexpr pgaccel_status PGACCEL_OK = 0;
            constexpr pgaccel_status PGACCEL_ERROR = 1;
            constexpr pgaccel_status PGACCEL_UNSUPPORTED = 2;
            constexpr int PGACCEL_WIDGET_DETAIL_INVALID = 9;
            extern "C" pgaccel_status pgaccel_detail_decline(bool invalid, int* detail) {
              if (invalid) {
                *detail = PGACCEL_WIDGET_DETAIL_INVALID;
                return PGACCEL_ERROR;
              }
              return PGACCEL_UNSUPPORTED;
            }
            """
        )
        self.assertFalse(valid.findings)

        hostile = {
            "numeric": r"""
                using pgaccel_status = int;
                constexpr pgaccel_status PGACCEL_ERROR = 1;
                extern "C" pgaccel_status pgaccel_numeric_detail(int* detail) {
                  *detail = 9;
                  return PGACCEL_ERROR;
                }
            """,
            "result": r"""
                using pgaccel_status = int;
                constexpr pgaccel_status PGACCEL_ERROR = 1;
                constexpr int PGACCEL_WIDGET_DETAIL_INVALID = 9;
                extern "C" pgaccel_status pgaccel_named_result(int* result) {
                  *result = PGACCEL_WIDGET_DETAIL_INVALID;
                  return PGACCEL_ERROR;
                }
            """,
            "detail_offset": r"""
                using pgaccel_status = int;
                constexpr pgaccel_status PGACCEL_ERROR = 1;
                constexpr int PGACCEL_WIDGET_DETAIL_INVALID = 9;
                extern "C" pgaccel_status pgaccel_detail_offset(int* detail) {
                  detail[1] = PGACCEL_WIDGET_DETAIL_INVALID;
                  return PGACCEL_ERROR;
                }
            """,
            "success": r"""
                using pgaccel_status = int;
                constexpr pgaccel_status PGACCEL_OK = 0;
                constexpr pgaccel_status PGACCEL_ERROR = 1;
                constexpr int PGACCEL_WIDGET_DETAIL_INVALID = 9;
                extern "C" pgaccel_status pgaccel_detail_success(bool invalid, int* detail) {
                  *detail = PGACCEL_WIDGET_DETAIL_INVALID;
                  if (invalid) return PGACCEL_ERROR;
                  return PGACCEL_OK;
                }
            """,
        }
        for name, source in hostile.items():
            with self.subTest(name=name):
                result = audit_compiling_fixture(source)
                self.assertTrue(result.findings)
                self.assertIn(
                    "host_output_write", result.entrypoint_audits[0].classifications
                )

    def test_zero_row_contract_allows_only_failure_terminal_detail(self) -> None:
        result = audit_compiling_fixture(
            r"""
            using pgaccel_status = int;
            constexpr pgaccel_status PGACCEL_OK = 0;
            constexpr pgaccel_status PGACCEL_UNSUPPORTED = 1;
            constexpr pgaccel_status PGACCEL_INVALID_ARGUMENT = 2;
            constexpr int PGACCEL_SPATIAL_DETAIL_NONE = 0;
            constexpr int PGACCEL_SPATIAL_DETAIL_CONTRACT = 1;
            struct Request { unsigned long count; };
            static pgaccel_status resident_validate_request_contract(
                const Request* request, int* detail) {
              if (request == nullptr) return PGACCEL_INVALID_ARGUMENT;
              *detail = PGACCEL_SPATIAL_DETAIL_NONE;
              return request->count == 0 ? PGACCEL_OK : PGACCEL_INVALID_ARGUMENT;
            }
            extern "C" pgaccel_status pgaccel_spatial_eval_resident_ex(
                const Request* request, int* detail) {
              if (detail == nullptr) return PGACCEL_INVALID_ARGUMENT;
              if (request == nullptr) {
                *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
                return PGACCEL_INVALID_ARGUMENT;
              }
              if (request->count != 0) {
                *detail = PGACCEL_SPATIAL_DETAIL_CONTRACT;
                return PGACCEL_UNSUPPORTED;
              }
              return resident_validate_request_contract(request, detail);
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn("failure_diagnostic", result.entrypoint_audits[0].classifications)


class ConstDescriptorMemberProvenanceTests(unittest.TestCase):
    def test_only_exact_mutable_pointer_member_alias_proves_output(self) -> None:
        result = audit_compiling_fixture(
            CompilerValidAdversarialTests.CPP_PRELUDE
            + r"""
            typedef struct {
              int* output;
              const int* input;
              int scalar;
            } Descriptor;

            extern "C" pgaccel_status pgaccel_member_alias(
                const Descriptor* descriptor, std::size_t count) {
              int* output = descriptor->output;
              sycl::queue queue;
              queue.parallel_for(sycl::range<1>(count),
                  [=](sycl::id<1> id) { output[id] = 1; });
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn(
            "output_alias_tracking", result.entrypoint_audits[0].classifications
        )

    def test_atomic_ref_mutation_preserves_derived_output_provenance(self) -> None:
        prelude = (
            CompilerValidAdversarialTests.CPP_PRELUDE
            + r"""
            namespace sycl {
            template <class T> struct atomic_ref {
              T& value;
              explicit atomic_ref(T& target) : value(target) {}
              T fetch_or(T operand) {
                T previous = value; value |= operand; return previous;
              }
              T load() const { return value; }
            };
            }
            struct Descriptor { unsigned int* flags; };
            """
        )
        valid = audit_compiling_fixture(
            prelude
            + r"""
            extern "C" pgaccel_status pgaccel_atomic_write(
                const Descriptor* descriptor, std::size_t count) {
              unsigned int* flags = descriptor->flags;
              sycl::queue queue;
              queue.parallel_for(sycl::range<1>(count), [=](sycl::id<1>) {
                sycl::atomic_ref<unsigned int> atomic(flags[0]);
                atomic.fetch_or(1u);
              });
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(valid.findings)

        load_only = audit_compiling_fixture(
            prelude
            + r"""
            extern "C" pgaccel_status pgaccel_atomic_load(
                const Descriptor* descriptor, std::size_t count) {
              unsigned int* flags = descriptor->flags;
              sycl::queue queue;
              queue.parallel_for(sycl::range<1>(count), [=](sycl::id<1>) {
                sycl::atomic_ref<unsigned int> atomic(flags[0]);
                (void)atomic.load();
              });
              return PGACCEL_OK;
            }
            """
        )
        entry = load_only.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertIn("rejected_terminal", entry.classifications)

        hostile = {
            "host": r"""
                unsigned int* flags = descriptor->flags;
                sycl::atomic_ref<unsigned int> atomic(flags[0]);
                atomic.fetch_or(1u);
            """,
            "direct_root": r"""
                sycl::queue queue;
                queue.parallel_for(sycl::range<1>(count), [=](sycl::id<1>) {
                  sycl::atomic_ref<unsigned int> atomic(descriptor->flags[0]);
                  atomic.fetch_or(1u);
                });
            """,
        }
        for name, body in hostile.items():
            with self.subTest(name=name):
                result = audit_compiling_fixture(
                    prelude
                    + f"""extern "C" pgaccel_status pgaccel_atomic_{name}(
                            const Descriptor* descriptor, std::size_t count) {{
                          {body}
                          return PGACCEL_OK;
                        }}"""
                )
                self.assertFalse(result.entrypoint_audits[0].ok)

    def test_const_and_unselected_descriptor_members_stay_red(self) -> None:
        sources = {
            "const_member": r"""
                const int* input = descriptor->input;
                sycl::queue queue;
                queue.parallel_for(sycl::range<1>(count),
                    [=](sycl::id<1> id) { (void)input[id]; });
            """,
            "direct_root": r"""
                const Descriptor* alias = descriptor;
                sycl::queue queue;
                queue.parallel_for(sycl::range<1>(count),
                    [=](sycl::id<1> id) { alias->output[id] = 1; });
            """,
            "aggregate_view": r"""
                View view{descriptor->output};
                sycl::queue queue;
                queue.parallel_for(sycl::range<1>(count),
                    [=](sycl::id<1> id) { view.output[id] = 1; });
            """,
            "assigned_view": r"""
                View view{};
                view.output = descriptor->output;
                sycl::queue queue;
                queue.parallel_for(sycl::range<1>(count),
                    [=](sycl::id<1> id) { view.output[id] = 1; });
            """,
        }
        prelude = (
            CompilerValidAdversarialTests.CPP_PRELUDE
            + r"""
            struct Descriptor { int* output; const int* input; int scalar; };
            struct View { int* output; };
            """
        )
        for name, body in sources.items():
            with self.subTest(name=name):
                result = audit_compiling_fixture(
                    prelude
                    + f"""extern "C" pgaccel_status pgaccel_{name}(
                            const Descriptor* descriptor, std::size_t count) {{
                          {body}
                          return PGACCEL_OK;
                        }}"""
                )
                entry = result.entrypoint_audits[0]
                self.assertFalse(entry.ok, entry.detail)
                self.assertIn("missing_device_terminal", entry.classifications)

    def test_member_alias_selection_and_unrelated_calls_stay_red(self) -> None:
        prelude = (
            CompilerValidAdversarialTests.CPP_PRELUDE
            + r"""
            struct Descriptor { int* output; };
            static int* pick_host(int* host, int*) { return host; }
            static void observe_pointer(int*) {}
            """
        )
        initializers = {
            "ternary": "choose_host ? host : descriptor->output",
            "helper": "pick_host(host, descriptor->output)",
            "unrelated_call": "(observe_pointer(descriptor->output), host)",
        }
        for name, initializer in initializers.items():
            with self.subTest(name=name):
                result = audit_compiling_fixture(
                    prelude
                    + f"""extern "C" pgaccel_status pgaccel_member_{name}(
                            const Descriptor* descriptor, bool choose_host,
                            std::size_t count) {{
                          int host_value = 0;
                          int* host = &host_value;
                          int* output = {initializer};
                          sycl::queue queue;
                          queue.parallel_for(sycl::range<1>(count),
                              [=](sycl::id<1> id) {{ output[id] = 1; }});
                          return PGACCEL_OK;
                        }}"""
                )
                entry = result.entrypoint_audits[0]
                self.assertFalse(entry.ok, entry.detail)
                self.assertNotIn("output_alias_tracking", entry.classifications)


class ResidentV5RegressionTests(unittest.TestCase):
    COPYBACK_PRELUDE = r"""
        using size_t = decltype(sizeof(0));
        using uint64_t = unsigned long long;
        using pgaccel_status = int;
        constexpr pgaccel_status PGACCEL_OK = 0;
        constexpr pgaccel_status PGACCEL_ERROR = 1;
        namespace sycl {
        template <int> struct range { explicit range(size_t) {} };
        template <int> struct id { operator size_t() const { return 0; } };
        struct event { void wait_and_throw() {} };
        struct handler {
          template <class Function> void parallel_for(range<1>, Function) {}
          template <class Function> void single_task(Function) {}
        };
        template <class T, int Dimensions> struct local_accessor {
          local_accessor(range<Dimensions>, handler&) {}
          T& operator[](size_t) const { static T value{}; return value; }
        };
        struct queue {
          template <class Function> event parallel_for(range<1>, Function) { return {}; }
          template <class Function> event submit(Function fn) {
            handler h;
            fn(h);
            return {};
          }
          event memcpy(void*, const void*, size_t) { return {}; }
          event memset(void*, int, size_t) { return {}; }
          void wait_and_throw() {}
        };
        struct device { static void get_devices() {} };
        inline void* malloc_shared(size_t, queue&) { static int value; return &value; }
        inline void* malloc_device(size_t, queue&) { static int value; return &value; }
        inline void free(void*, queue&) {}
        }
        namespace std {
        inline void* memcpy(void* dst, const void*, size_t) { return dst; }
        template <class T> void swap(T& left, T& right) {
          T value = left;
          left = right;
          right = value;
        }
        }
        static bool initialized = false;
        static uint64_t counter = 0;
        static sycl::queue queue_value;
        static sycl::queue* g_queue = &queue_value;
    """

    def test_kernel_written_usm_copyback_forms_are_proven(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_queue_copyback(int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* device_result = static_cast<int*>(sycl::malloc_device(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                device_result[i] = 1;
              }).wait_and_throw();
              q.memcpy(out, device_result, count).wait_and_throw();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_shared_copyback(int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* shared_result = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                shared_result[i] = 1;
              }).wait_and_throw();
              std::memcpy(out, shared_result, count);
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        for entry in result.entrypoint_audits:
            self.assertIn("device_copyback", entry.classifications)

    def test_typed_slab_projection_with_intermediate_readback_is_proven(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_projected_slab(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* slab = static_cast<int*>(
                  sycl::malloc_shared(2 * count * sizeof(int), q));
              const size_t output_offset = count;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                const int* input = reinterpret_cast<const int*>(slab);
                int* projected = reinterpret_cast<int*>(slab + output_offset);
                projected[i] = input[i] + 1;
              }).wait_and_throw();
              int completion = 0;
              q.memcpy(&completion, slab + output_offset, sizeof(int)).wait_and_throw();
              if (completion < 0) {
                sycl::free(slab, q);
                return PGACCEL_ERROR;
              }
              q.memcpy(out, slab + output_offset, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn("device_copyback", result.entrypoint_audits[0].classifications)

    def test_resident_raw_usm_resource_binding_is_terminal_safe(self) -> None:
        result = audit_fixture(
            r"""
            struct ResidentState {
              int* device_index;
              std::vector<int> host_payload;
            };
            void build_index(int* device_index, size_t count) {
              sycl::queue q;
              q.parallel_for<BuildIndex>(sycl::range<1>(count), [=](sycl::id<1> id) {
                device_index[id[0]] = static_cast<int>(id[0]);
              }).wait();
            }
            extern "C" ResidentState* pgaccel_resident_raw_usm(size_t count) {
              sycl::queue q;
              int* device_index = sycl::malloc_device<int>(count, q);
              build_index(device_index, count);
              auto* state = new ResidentState();
              state->device_index = device_index;
              return state;
            }
            extern "C" ResidentState* pgaccel_resident_overwritten(size_t count) {
              sycl::queue q;
              int* device_index = sycl::malloc_device<int>(count, q);
              build_index(device_index, count);
              auto* state = new ResidentState();
              state->device_index = device_index;
              q.fill(device_index, 0, count).wait();
              return state;
            }
            extern "C" ResidentState* pgaccel_resident_freed(size_t count) {
              sycl::queue q;
              int* device_index = sycl::malloc_device<int>(count, q);
              build_index(device_index, count);
              auto* state = new ResidentState();
              state->device_index = device_index;
              sycl::free(device_index, q);
              return state;
            }
            extern "C" ResidentState* pgaccel_resident_copied_over(
                const int* host_values, size_t count) {
              sycl::queue q;
              int* device_index = sycl::malloc_device<int>(count, q);
              build_index(device_index, count);
              auto* state = new ResidentState();
              state->device_index = device_index;
              q.copy(host_values, device_index, count).wait();
              return state;
            }
            extern "C" ResidentState* pgaccel_resident_alias_host_write(
                size_t count, int host_value) {
              sycl::queue q;
              int* device_index = sycl::malloc_device<int>(count, q);
              int* alias = device_index;
              build_index(device_index, count);
              alias[0] = host_value;
              auto* state = new ResidentState();
              state->device_index = alias;
              return state;
            }
            extern "C" ResidentState* pgaccel_resident_mixed_host_payload(
                size_t count, int host_value) {
              sycl::queue q;
              int* device_index = sycl::malloc_device<int>(count, q);
              build_index(device_index, count);
              auto* state = new ResidentState();
              state->device_index = device_index;
              state->host_payload.resize(count);
              state->host_payload[0] = host_value;
              return state;
            }
            """
        )
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        self.assertTrue(entries["pgaccel_resident_raw_usm"].ok)
        self.assertIn(
            "opaque_device_resource",
            entries["pgaccel_resident_raw_usm"].classifications,
        )
        for name in (
            "pgaccel_resident_overwritten",
            "pgaccel_resident_freed",
            "pgaccel_resident_copied_over",
            "pgaccel_resident_alias_host_write",
            "pgaccel_resident_mixed_host_payload",
        ):
            with self.subTest(name):
                self.assertFalse(entries[name].ok, entries[name].detail)
                self.assertNotIn(
                    "opaque_device_resource", entries[name].classifications
                )

    def test_member_count_batched_queue_work_is_collectively_awaited(self) -> None:
        positive = audit_fixture(
            r"""
            struct BatchRequest { size_t count; int* output; };
            extern "C" pgaccel_status pgaccel_member_count_batches(
                const BatchRequest* request) {
              const size_t selected_count = request->count;
              if (selected_count == 0) return PGACCEL_OK;
              sycl::queue q;
              auto* output = request->output;
              q.memset(output, 0, selected_count * sizeof(int));
              for (size_t start = 0; start < selected_count;) {
                const size_t batch = std::min(size_t{64}, selected_count - start);
                q.parallel_for<BatchWrite>(sycl::range<1>(batch), [=](sycl::id<1> id) {
                  output[start + id[0]] = 7;
                });
                start += batch;
              }
              q.wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(positive.findings)
        entry = positive.entrypoint_audits[0]
        self.assertIn("zero_work", entry.classifications)
        self.assertIn("device_launch_orchestration", entry.classifications)

        unawaited = audit_fixture(
            r"""
            struct BatchRequest { size_t count; int* output; };
            extern "C" pgaccel_status pgaccel_unawaited_batches(
                const BatchRequest* request) {
              const size_t selected_count = request->count;
              if (selected_count == 0) return PGACCEL_OK;
              sycl::queue q;
              auto* output = request->output;
              for (size_t start = 0; start < selected_count;) {
                const size_t batch = std::min(size_t{64}, selected_count - start);
                q.parallel_for<UnawaitedBatch>(sycl::range<1>(batch), [=](sycl::id<1> id) {
                  output[start + id[0]] = 7;
                });
                start += batch;
              }
              return PGACCEL_OK;
            }
            """
        )
        self.assertTrue(unawaited.findings)
        self.assertIn(
            "host_computation",
            unawaited.entrypoint_audits[0].classifications,
        )

        nonzero_initializer = audit_fixture(
            r"""
            struct BatchRequest { size_t count; int* output; };
            extern "C" pgaccel_status pgaccel_nonzero_batch_init(
                const BatchRequest* request) {
              const size_t selected_count = request->count;
              if (selected_count == 0) return PGACCEL_OK;
              sycl::queue q;
              auto* output = request->output;
              q.memset(output, 42, selected_count * sizeof(int));
              q.parallel_for<NonzeroBatch>(sycl::range<1>(selected_count), [=](sycl::id<1> id) {
                output[id[0]] = 7;
              });
              q.wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        self.assertTrue(nonzero_initializer.findings)
        self.assertIn(
            "unresolved_output_helper",
            nonzero_initializer.entrypoint_audits[0].classifications,
        )

    def test_read_only_typed_slab_projection_does_not_prove_copyback(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_read_only_projection(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* slab = static_cast<int*>(
                  sycl::malloc_shared(2 * count * sizeof(int), q));
              const size_t output_offset = count;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                const int* projected = reinterpret_cast<const int*>(slab + output_offset);
                int ignored = projected[i];
                (void)ignored;
              }).wait_and_throw();
              q.memcpy(out, slab + output_offset, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        entry = result.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertNotIn("device_copyback", entry.classifications)

    def test_terminal_offset_zero_output_path_is_exact(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_terminal_offset_zero(
                const int* offsets, size_t count, int* out) {
              if (count == 0) return PGACCEL_OK;
              const size_t output_count = offsets[count];
              if (output_count == 0) return PGACCEL_OK;
              sycl::queue q;
              q.parallel_for(sycl::range<1>(output_count), [=](sycl::id<1> i) {
                out[i] = 1;
              }).wait_and_throw();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_computed_offset_zero(
                const int* offsets, size_t count, int* out) {
              if (count == 0) return PGACCEL_OK;
              const size_t output_count = offsets[count] + 1;
              if (output_count == 0) return PGACCEL_OK;
              sycl::queue q;
              q.parallel_for(sycl::range<1>(output_count), [=](sycl::id<1> i) {
                out[i] = 1;
              }).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        self.assertTrue(entries["pgaccel_terminal_offset_zero"].ok)
        self.assertFalse(entries["pgaccel_computed_offset_zero"].ok)

    def test_guaranteed_once_device_launch_orchestration_is_proven(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_orchestrated_copyback(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* device_result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              size_t stage = 0;
              do {
                q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                  device_result[i] = static_cast<int>(stage + 1);
                }).wait_and_throw();
                ++stage;
              } while (stage < 2);
              q.memcpy(out, device_result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn(
            "device_launch_orchestration",
            result.entrypoint_audits[0].classifications,
        )

    def test_device_launch_orchestration_mutants_fail_closed(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            static void host_prepare(const int*) {}

            extern "C" pgaccel_status pgaccel_zero_iteration_loop(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* device_result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              for (size_t stage = 0; stage < 2; ++stage) {
                q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                  device_result[i] = 1;
                }).wait_and_throw();
              }
              q.memcpy(out, device_result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }

            extern "C" pgaccel_status pgaccel_loop_host_output(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* device_result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              do {
                out[0] = 7;
                q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                  device_result[i] = 1;
                }).wait_and_throw();
              } while (false);
              q.memcpy(out, device_result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }

            extern "C" pgaccel_status pgaccel_loop_host_staging(
                const int* input, size_t count, int* out) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* device_result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              do {
                device_result[0] = input[0];
                q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                  device_result[i] = 1;
                }).wait_and_throw();
              } while (false);
              q.memcpy(out, device_result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }

            extern "C" pgaccel_status pgaccel_loop_host_helper(
                const int* input, size_t count, int* out) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* device_result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              do {
                host_prepare(input);
                q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                  device_result[i] = 1;
                }).wait_and_throw();
              } while (false);
              q.memcpy(out, device_result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }

            extern "C" pgaccel_status pgaccel_loop_unawaited_launch(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* device_result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              do {
                q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                  device_result[i] = 1;
                });
              } while (false);
              q.memcpy(out, device_result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        self.assertEqual(result.entrypoints, 5)
        for entry in result.entrypoint_audits:
            self.assertFalse(entry.ok, entry.entrypoint)
            self.assertIn("host_computation", entry.classifications)

    def test_radix_style_device_orchestration_is_proven(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            static pgaccel_status prepare_device(
                sycl::queue& q, int* device, size_t count) {
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                device[i] = 1;
              }).wait_and_throw();
              return PGACCEL_OK;
            }

            extern "C" pgaccel_status pgaccel_radix_orchestration(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue* q = &queue_value;
              int* first = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), *q));
              int* second = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), *q));
              int* result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), *q));
              size_t stage = 0;
              do {
                q->memset(second, 0, count * sizeof(int)).wait_and_throw();
                prepare_device(*q, second, count);
                q->submit([&](sycl::handler& h) {
                  sycl::local_accessor<int, 1> scratch(sycl::range<1>(1), h);
                  h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                    scratch[0] = second[i];
                    second[i] = scratch[0] + 1;
                  });
                }).wait_and_throw();
                std::swap(first, second);
                ++stage;
              } while (stage < 1);
              q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                result[i] = first[i];
              }).wait_and_throw();
              q->memcpy(out, result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn(
            "device_launch_orchestration",
            result.entrypoint_audits[0].classifications,
        )

    def test_radix_style_orchestration_mutants_fail_closed(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            static void host_prepare(int* device) { device[0] = 9; }

            extern "C" pgaccel_status pgaccel_command_group_host_staging(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* shared = static_cast<int*>(
                  sycl::malloc_shared(count * sizeof(int), q));
              int* result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              do {
                q.submit([&](sycl::handler& h) {
                  shared[0] = 7;
                  h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                    result[i] = shared[i];
                  });
                }).wait_and_throw();
              } while (false);
              q.memcpy(out, result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }

            extern "C" pgaccel_status pgaccel_loop_memset_output(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              do {
                q.memset(out, 0, count * sizeof(int)).wait_and_throw();
                q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                  result[i] = 1;
                }).wait_and_throw();
              } while (false);
              q.memcpy(out, result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }

            extern "C" pgaccel_status pgaccel_loop_scalar_swap(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              size_t first = 0;
              size_t second = 1;
              do {
                std::swap(first, second);
                q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                  result[i] = static_cast<int>(first);
                }).wait_and_throw();
              } while (false);
              q.memcpy(out, result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }

            extern "C" pgaccel_status pgaccel_loop_unproven_helper(
                int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* result = static_cast<int*>(
                  sycl::malloc_device(count * sizeof(int), q));
              do {
                host_prepare(result);
                q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                  result[i] = 1;
                }).wait_and_throw();
              } while (false);
              q.memcpy(out, result, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        self.assertEqual(result.entrypoints, 4)
        for entry in result.entrypoint_audits:
            self.assertFalse(entry.ok, entry.entrypoint)
            self.assertIn("host_computation", entry.classifications)

    def test_unawaited_wrong_space_and_unwritten_copybacks_fail(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_unawaited(int* out, size_t count) {
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; });
              q.memcpy(out, buffer, count).wait_and_throw();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_host_reads_device(int* out, size_t count) {
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_device(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; })
                  .wait_and_throw();
              std::memcpy(out, buffer, count);
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_unwritten_source(int* out, size_t count) {
              sycl::queue q;
              int* source = static_cast<int*>(sycl::malloc_shared(count, q));
              int* other = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { other[i] = 1; })
                  .wait_and_throw();
              q.memcpy(out, source, count).wait_and_throw();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_host_staging_overwrite(
                int* out, size_t count) {
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; })
                  .wait_and_throw();
              buffer[0] = 7;
              std::memcpy(out, buffer, count);
              return PGACCEL_OK;
            }
            """
        )
        self.assertEqual(len(result.entrypoint_audits), 4)
        for entry in result.entrypoint_audits:
            with self.subTest(entry.entrypoint):
                self.assertFalse(entry.ok, entry.detail)
                self.assertNotIn("device_copyback", entry.classifications)

    def test_copyback_requires_exact_pointer_expressions_and_positive_size(
        self,
    ) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_exact_copyback(int* out, size_t count) {
              if (count == 0) return PGACCEL_OK;
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; })
                  .wait_and_throw();
              q.memcpy(out + 0, buffer + 0, count * sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_comma_destination(int* out, size_t count) {
              static int scratch;
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; })
                  .wait_and_throw();
              q.memcpy((out, &scratch), buffer, sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_comma_source(int* out, size_t count) {
              static int host_value;
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; })
                  .wait_and_throw();
              q.memcpy(out, (buffer, &host_value), sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_zero_copyback(int* out, size_t count) {
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; })
                  .wait_and_throw();
              q.memcpy(out, buffer, 0).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        self.assertTrue(entries["pgaccel_exact_copyback"].ok)
        self.assertIn(
            "device_copyback", entries["pgaccel_exact_copyback"].classifications
        )
        for name in {
            "pgaccel_comma_destination",
            "pgaccel_comma_source",
            "pgaccel_zero_copyback",
        }:
            with self.subTest(name):
                self.assertFalse(entries[name].ok, entries[name].detail)
                self.assertNotIn("device_copyback", entries[name].classifications)

    def test_sequential_constant_offset_copybacks_share_device_producer(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_offset_copybacks(
                int* out_first, int* out_second) {
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_device(2 * sizeof(int), q));
              q.parallel_for(sycl::range<1>(2), [=](sycl::id<1> i) {
                buffer[i] = 1;
              }).wait_and_throw();
              q.memcpy(out_first, buffer, sizeof(int)).wait_and_throw();
              q.memcpy(out_second, buffer + 1, sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(result.findings)
        self.assertIn("device_copyback", result.entrypoint_audits[0].classifications)

    def test_runtime_pointer_offset_does_not_prove_copyback(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_runtime_offset(
                int* out, size_t offset) {
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_device(2 * sizeof(int), q));
              q.parallel_for(sycl::range<1>(2), [=](sycl::id<1> i) {
                buffer[i] = 1;
              }).wait_and_throw();
              q.memcpy(out, buffer + offset, sizeof(int)).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        entry = result.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertNotIn("device_copyback", entry.classifications)

    def test_same_line_copyback_does_not_mask_later_host_overwrite(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_same_line_overwrite(
                int* out, size_t count) {
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; })
                  .wait_and_throw();
              std::memcpy(out, buffer, count); out[0] = 7;
              return PGACCEL_OK;
            }
            """
        )
        entry = result.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertIn("host_output_write", entry.classifications)

    def test_copyback_requires_live_sycl_usm_provenance(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            namespace fake {
            inline void* malloc_shared(size_t, sycl::queue&) {
              static int host_value;
              return &host_value;
            }
            }
            extern "C" pgaccel_status pgaccel_fake_usm(int* out, size_t count) {
              sycl::queue q;
              int* buffer = static_cast<int*>(fake::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; })
                  .wait_and_throw();
              std::memcpy(out, buffer, sizeof(int));
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_reassigned_usm(int* out, size_t count) {
              static int host_value;
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_shared(count, q));
              buffer = &host_value;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { buffer[i] = 1; })
                  .wait_and_throw();
              std::memcpy(out, buffer, sizeof(int));
              return PGACCEL_OK;
            }
            """
        )
        for entry in result.entrypoint_audits:
            with self.subTest(entry.entrypoint):
                self.assertFalse(entry.ok, entry.detail)
                self.assertNotIn("device_copyback", entry.classifications)

    def test_post_declaration_member_aliases_propagate_through_casts(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            struct AliasHolder { int* ptr; };
            extern "C" pgaccel_status pgaccel_member_cast(int* out, size_t count) {
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { out[i] = 1; })
                  .wait_and_throw();
              AliasHolder holder{};
              holder.ptr = static_cast<int*>(out);
              holder.ptr[0] = 7;
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_member_paren(int* out, size_t count) {
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { out[i] = 1; })
                  .wait_and_throw();
              AliasHolder holder{};
              holder.ptr = (out);
              holder.ptr[0] = 7;
              return PGACCEL_OK;
            }
            """
        )
        for entry in result.entrypoint_audits:
            with self.subTest(entry.entrypoint):
                self.assertFalse(entry.ok, entry.detail)
                self.assertIn("host_output_write", entry.classifications)

    def test_dead_lambda_writes_do_not_prove_copyback(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_dead_lambda_write(
                int* out, size_t count) {
              sycl::queue q;
              int* buffer = static_cast<int*>(sycl::malloc_shared(count, q));
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) {
                if (false) buffer[i] = 1;
              }).wait_and_throw();
              std::memcpy(out, buffer, sizeof(int));
              return PGACCEL_OK;
            }
            """
        )
        entry = result.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertNotIn("device_copyback", entry.classifications)

    def test_post_kernel_alias_member_and_pointer_arithmetic_overwrites_fail(
        self,
    ) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            struct Holder { int* ptr; };
            extern "C" pgaccel_status pgaccel_brace(int* out, size_t count) {
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { out[i] = 1; })
                  .wait_and_throw();
              auto alias{out}; alias[0] = 7; return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_aggregate(int* out, size_t count) {
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { out[i] = 1; })
                  .wait_and_throw();
              Holder holder{out}; holder.ptr[0] = 7; return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_arithmetic(int* out, size_t count) {
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { out[i] = 1; })
                  .wait_and_throw();
              *(out + 0) = 7; return PGACCEL_OK;
            }
            """
        )
        for entry in result.entrypoint_audits:
            with self.subTest(entry.entrypoint):
                self.assertFalse(entry.ok, entry.detail)
                self.assertIn("host_output_write", entry.classifications)

    def test_local_status_is_not_an_output_alias_and_nested_zero_is_neutral(
        self,
    ) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            static pgaccel_status launch(int* out, size_t count) {
              sycl::queue q;
              q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> i) { out[i] = 1; })
                  .wait_and_throw();
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_status_local(int* out, size_t count) {
              pgaccel_status st = launch(out, count);
              return st;
            }
            extern "C" pgaccel_status pgaccel_nested_zero(int* out, size_t count) {
              if (count == 0) { if (out) *out = 0; return PGACCEL_OK; }
              return launch(out, count);
            }
            """
        )
        self.assertFalse(result.findings)
        entries = {entry.entrypoint: entry for entry in result.entrypoint_audits}
        self.assertNotIn(
            "output_alias_tracking", entries["pgaccel_status_local"].classifications
        )
        self.assertIn("zero_work", entries["pgaccel_nested_zero"].classifications)

    def test_exact_support_contracts_are_immutable_and_evidence_bound(self) -> None:
        with self.assertRaises(TypeError):
            audit.LIFECYCLE_CONTRACTS["pgaccel_fake"] = object()  # type: ignore[index]
        valid = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" uint64_t pgaccel_gpu_exec_count() { return counter; }
            extern "C" void pgaccel_reset_gpu_exec_count() { counter = 0; }
            extern "C" pgaccel_status pgaccel_init() {
              if (initialized) return PGACCEL_OK;
              sycl::device::get_devices(); initialized = true; return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_shutdown() {
              if (!initialized) return PGACCEL_OK;
              g_queue->wait_and_throw(); delete g_queue; return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_expr_device_copy_to_host(
                void* dst, const void* src, size_t bytes) {
              if (bytes == 0) return PGACCEL_OK;
              if (!dst || !src) return PGACCEL_ERROR;
              sycl::queue q; q.memcpy(dst, src, bytes).wait_and_throw(); return PGACCEL_OK;
            }
            """
        )
        self.assertFalse(valid.findings)

        invalid = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            static bool runtime_skip() { return true; }
            extern "C" pgaccel_status pgaccel_init() {
              if (runtime_skip()) return PGACCEL_OK;
              sycl::device::get_devices(); return PGACCEL_OK;
            }
            extern "C" uint64_t pgaccel_gpu_exec_count() { return 42; }
            """
        )
        for entry in invalid.entrypoint_audits:
            self.assertFalse(entry.ok, entry.detail)
            self.assertIn("invalid_lifecycle_contract", entry.classifications)

        wrong_polarity = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_init() {
              if (!initialized) return PGACCEL_OK;
              sycl::device::get_devices(); return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_shutdown() {
              if (initialized) return PGACCEL_OK;
              g_queue->wait_and_throw(); delete g_queue; return PGACCEL_OK;
            }
            """
        )
        for entry in wrong_polarity.entrypoint_audits:
            self.assertFalse(entry.ok, entry.detail)
            self.assertIn("invalid_lifecycle_contract", entry.classifications)

        compound_guard = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            static bool runtime_skip() { return true; }
            extern "C" pgaccel_status pgaccel_init() {
              if (initialized || runtime_skip()) return PGACCEL_OK;
              sycl::device::get_devices(); return PGACCEL_OK;
            }
            """
        )
        entry = compound_guard.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertIn("invalid_lifecycle_contract", entry.classifications)

    def test_transfer_contract_requires_exact_awaited_queue_copy(self) -> None:
        result = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_expr_device_copy_from_host(
                void* dst, const void* src, size_t bytes) {
              if (bytes == 0) return PGACCEL_OK;
              std::memcpy(dst, src, bytes);
              return PGACCEL_OK;
            }
            extern "C" pgaccel_status pgaccel_expr_device_copy_to_host(
                void* dst, const void* src, size_t bytes) {
              if (bytes == 0) return PGACCEL_OK;
              sycl::queue q;
              q.memcpy(dst, src, 0).wait_and_throw();
              return PGACCEL_OK;
            }
            """
        )
        self.assertEqual(len(result.entrypoint_audits), 2)
        for entry in result.entrypoint_audits:
            with self.subTest(entry.entrypoint):
                self.assertFalse(entry.ok, entry.detail)
                self.assertIn("invalid_lifecycle_contract", entry.classifications)

    def test_allocation_and_free_contracts_bind_output_to_usm_evidence(self) -> None:
        valid = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_expr_shared_alloc(size_t bytes, void** out) {
              if (out == nullptr) return PGACCEL_ERROR;
              *out = nullptr;
              if (bytes == 0) return PGACCEL_OK;
              sycl::queue q;
              void* ptr = sycl::malloc_shared(bytes, q);
              if (ptr == nullptr) return PGACCEL_ERROR;
              *out = ptr;
              return PGACCEL_OK;
            }
            extern "C" void pgaccel_expr_shared_free(void* ptr) {
              if (ptr == nullptr) return;
              sycl::queue q;
              sycl::free(ptr, q);
            }
            """
        )
        self.assertFalse(valid.findings)

        invalid = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_expr_shared_alloc(size_t bytes, void** out) {
              sycl::queue q;
              void* ptr = sycl::malloc_shared(bytes, q);
              (void)ptr;
              *out = reinterpret_cast<void*>(42);
              return PGACCEL_OK;
            }
            """
        )
        entry = invalid.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertIn("invalid_lifecycle_contract", entry.classifications)

        discarded = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_expr_shared_alloc(size_t bytes, void** out) {
              *out = nullptr;
              if (bytes == 0) return PGACCEL_OK;
              sycl::queue q;
              void* ptr = sycl::malloc_shared(bytes, q);
              (void)ptr;
              return PGACCEL_OK;
            }
            """
        )
        entry = discarded.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertIn("invalid_lifecycle_contract", entry.classifications)

        uncopied = audit_compiling_fixture(
            self.COPYBACK_PRELUDE
            + r"""
            extern "C" pgaccel_status pgaccel_expr_device_alloc_copy(
                const void* src, size_t bytes, void** out) {
              if (bytes == 0) { *out = nullptr; return PGACCEL_OK; }
              sycl::queue q;
              void* copied = sycl::malloc_shared(bytes, q);
              void* published = sycl::malloc_shared(bytes, q);
              std::memcpy(copied, src, bytes);
              *out = published;
              return PGACCEL_OK;
            }
            """
        )
        entry = uncopied.entrypoint_audits[0]
        self.assertFalse(entry.ok, entry.detail)
        self.assertIn("invalid_lifecycle_contract", entry.classifications)


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

    def test_release_object_validation_is_mandatory_exact_and_fresh(self) -> None:
        missing_evidence, missing = audit._release_object_validation([], [])
        self.assertTrue(missing)
        self.assertEqual(missing_evidence[0]["status"], "missing")
        self.assertIn("missing_object_inventory", missing[0].classifications)

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            input_path = root / "kernel.cpp"
            input_path.write_text("// input\n", encoding="utf-8")
            build_marker = root / "build.marker"
            build_marker.touch()
            wrong = root / "unrelated.so"
            wrong.write_bytes(b"not the kernel library")
            _, wrong_findings = audit._release_object_validation(
                [wrong], [input_path], build_marker
            )
            self.assertIn("wrong_release_object", wrong_findings[0].classifications)

            shared = root / "libpgaccel_kernels_shared.so"
            shared.write_bytes(b"kernel library")
            base = max(
                input_path.stat().st_mtime,
                shared.stat().st_mtime,
                build_marker.stat().st_mtime,
            )
            os.utime(build_marker, (base - 1, base - 1))
            os.utime(shared, (base, base))
            os.utime(input_path, (base + 2, base + 2))
            stale_evidence, stale = audit._release_object_validation(
                [shared], [input_path], build_marker
            )
            self.assertEqual(stale_evidence[0]["status"], "invalid")
            self.assertIn("stale_release_object", stale[0].classifications)

            os.utime(shared, (base + 4, base + 4))
            fresh_evidence, fresh = audit._release_object_validation(
                [shared], [input_path], build_marker
            )
            self.assertFalse(fresh)
            self.assertEqual(fresh_evidence[0]["status"], "verified")
            self.assertEqual(fresh_evidence[0]["input_count"], 1)
            self.assertIn("input_inventory_sha256", fresh_evidence[0])

            missing_marker_evidence, missing_marker = audit._release_object_validation(
                [shared], [input_path]
            )
            self.assertEqual(missing_marker_evidence[0]["status"], "invalid")
            self.assertIn("missing_build_marker", missing_marker[0].classifications)


class ReleaseWiringTests(unittest.TestCase):
    def test_precommit_keeps_fixture_gate_and_real_recipe_has_headers(self) -> None:
        justfile = (REPO_ROOT / "Justfile").read_text(encoding="utf-8")
        precommit = next(
            line for line in justfile.splitlines() if line.startswith("pre-commit:")
        )
        self.assertIn("audit-cpu-cheats-test", precommit)
        recipe = justfile[justfile.index("audit-cpu-cheats: audit-cpu-cheats-test") :]
        self.assertLess(recipe.index("just gpu-build"), recipe.index("--objects"))
        self.assertLess(recipe.index("-delete"), recipe.index("just gpu-build"))
        self.assertIn("libpgaccel_kernels_shared", recipe)
        self.assertIn('--objects "$objects"', recipe)
        self.assertIn('--build-marker "$build_marker"', recipe)
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

    def test_cli_never_implicitly_discovers_release_objects(self) -> None:
        analyzer = (REPO_ROOT / "scripts/cpu_cheat_audit.py").read_text(
            encoding="utf-8"
        )
        main = analyzer[analyzer.index("def main(") :]
        self.assertIn("require_release_object=True", main)
        self.assertIn("build_marker=args.build_marker", main)
        self.assertNotIn("libpgaccel_kernels_shared.*", main)

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
        real_gate = release_plz[
            release_plz.index("Run real CPU-cheat release gate") : release_plz.index(
                "MarcoIeni/release-plz-action"
            )
        ]
        self.assertIn("just audit-cpu-cheats", real_gate)


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
        status_names = sorted(
            entry.entrypoint for entry in self.by_name.values() if entry.is_status
        )[:82]
        # Preserve the original production-name floor as code turns green: a
        # name-specific/file-specific exemption would bless one of these exact
        # ABI entrypoints even though its body is replaced by hostile host work.
        mutant_source = "\n".join(
            f"""extern "C" pgaccel_status {name}(int* out) {{
                  *out = 42;
                  return PGACCEL_OK;
                }}"""
            for name in status_names
        )
        mutant_audit = audit.audit_source(
            pathlib.Path("production-name-mutants.cpp"), mutant_source
        )
        status_failed = sum(
            entry.is_status and not entry.ok for entry in mutant_audit.entrypoint_audits
        )
        self.assertEqual(len(status_names), 82)
        self.assertGreaterEqual(status_failed, 82)

    def test_inventory_hashes_are_deterministic(self) -> None:
        second = audit.audit_abi(
            self.source_paths,
            self.header_paths,
            manifest_path=audit.DEFAULT_ABI_MANIFEST,
        )
        self.assertEqual(second.definition_hash, self.abi.definition_hash)
        self.assertEqual(second.declaration_hash, self.abi.declaration_hash)
        self.assertEqual(second.per_file, self.abi.per_file)

    def test_point_in_polygon_staged_copyback_is_proven(self) -> None:
        entry = self.by_name["pgaccel_point_in_polygon_bulk"]
        self.assertTrue(entry.ok, entry.detail)
        self.assertEqual(entry.path.name, "spatial_dispatch.cpp")
        self.assertIn("large_input_gpu_chain", entry.classifications)
        self.assertIn("awaited queue memcpy", entry.detail)

    def test_sort_i64_uses_independently_proven_concrete_dispatch(self) -> None:
        entry = self.by_name["pgaccel_sort_i64"]
        self.assertTrue(entry.ok, entry.detail)
        self.assertEqual(entry.path.name, "sort.cpp")
        self.assertIn("large_input_gpu_chain", entry.classifications)
        self.assertNotIn("template_specialization_review", entry.classifications)

    def test_reduction_device_finalize_is_source_proven(self) -> None:
        entry = self.by_name["pgaccel_reduce_sum_f32"]
        self.assertTrue(entry.ok, entry.detail)
        self.assertEqual(entry.path.name, "reduce.cpp")
        self.assertIn("large_input_gpu_chain", entry.classifications)
        self.assertIn("zero_work", entry.classifications)
        self.assertNotIn("host_output_write", entry.classifications)

    def test_non_status_hash_join_build_is_audited(self) -> None:
        entry = self.by_name["pgaccel_hash_join_build"]
        self.assertFalse(entry.is_status)
        self.assertFalse(entry.ok)
        self.assertEqual(entry.path.name, "hash_join.cpp")
        self.assertEqual(entry.return_type, "pgaccel_hash_table *")

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
