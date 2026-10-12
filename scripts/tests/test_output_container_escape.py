from __future__ import annotations

import pathlib
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(ROOT / "scripts/tests"))

from test_cpu_cheat_audit import audit_compiling_fixture  # noqa: E402

PRELUDE = r"""
using pgaccel_status = int;
constexpr int PGACCEL_OK = 0;
namespace sycl {
struct queue {
  template<class F> void single_task(F) {}
  void wait_and_throw() {}
};
}
int* escaped[2];
struct Holder { int* slots[2]; } holder;
void finish() { *escaped[0] = 42; }
void finish_holder() { *holder.slots[0] = 42; }
void mutate(int* p) { *p = 42; }
"""


def check(body):
    return audit_compiling_fixture(
        PRELUDE
        + r"""
extern "C" pgaccel_status pgaccel_probe(sycl::queue& q, int* out) {
  q.single_task([=] { *out = 1; });
  q.wait_and_throw();
"""
        + body
        + "\n return PGACCEL_OK;\n}"
    )


class OutputContainerEscapeTests(unittest.TestCase):
    def test_host_pointer_tables_cannot_hide_post_device_finalization(self):
        for body in (
            "escaped[0] = out; finish();",
            "escaped[0] = static_cast<int*>(out); finish();",
            "escaped[0] = out + 0; finish();",
            "auto* alias = out; escaped[0] = alias; finish();",
            "holder.slots[0] = out; finish_holder();",
            "auto host = [=] { escaped[0] = out; finish(); }; host();",
            "int* slots[1] = {out}; mutate(slots[0]);",
            "int* slots[1]{out}; mutate(slots[0]);",
            "int* slots[]{out}; mutate(slots[0]);",
            "int* slots[1][1]{{out}}; mutate(slots[0][0]);",
        ):
            with self.subTest(body=body):
                result = check(body)
                self.assertEqual(len(result.entrypoint_audits), 1)
                entry = result.entrypoint_audits[0]
                self.assertFalse(entry.ok, entry)
                self.assertIn("output_container_escape", entry.classifications)

    def test_escape_before_device_work_or_failure_still_requires_review(self):
        for terminal in ("return PGACCEL_OK;", "return 4;"):
            result = audit_compiling_fixture(
                PRELUDE
                + r"""
extern "C" pgaccel_status pgaccel_probe(sycl::queue& q, int* out) {
  escaped[0] = out;
  q.single_task([=] { *out = 1; });
  q.wait_and_throw();
"""
                + terminal
                + "\n}"
            )
            self.assertFalse(result.entrypoint_audits[0].ok)
            self.assertIn(
                "output_container_escape", result.entrypoint_audits[0].classifications
            )

    def test_gpu_array_stores_and_unrelated_host_arrays_still_pass(self):
        for body in (
            "int slots[2]{3, 4}; (void)slots;",
            "int slots[2] = {3, 4}; (void)slots;",
            "int slots[2]; slots[0] = 3;",
            "q.single_task([=] { int* slots[1]{out}; *slots[0] = 2; }); "
            "q.wait_and_throw();",
        ):
            with self.subTest(body=body):
                result = check(body)
                self.assertTrue(result.entrypoint_audits[0].ok, result.findings)

    def test_array_initializer_calls_are_not_hidden_as_lambda_bodies(self):
        result = check("int ignored[1]{(mutate(out), 0)}; (void)ignored;")
        self.assertFalse(result.entrypoint_audits[0].ok)
        self.assertIn(
            "output_container_escape", result.entrypoint_audits[0].classifications
        )


if __name__ == "__main__":
    unittest.main()
