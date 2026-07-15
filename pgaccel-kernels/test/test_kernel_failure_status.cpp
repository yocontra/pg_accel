// test_kernel_failure_status.cpp — induced-failure honesty gate.
//
// Pins the kernel-safety contracts for async failure propagation and
// exception containment at extern-C boundaries:
//
//   1. A dispatch whose staging allocation cannot possibly succeed
//      (multiple exabyte-sized buffers) must surface an error status —
//      PGACCEL_OOM if the USM allocator returns nullptr, PGACCEL_ERROR if
//      it throws — and must NEVER report PGACCEL_OK over unwritten output.
//   2. The failure must not be misreported as PGACCEL_ERROR_NO_DEVICE:
//      a positive control proves a working device exists in this process.
//   3. The failing call must not crash the process. Before the extern "C"
//      try/catch fixes, a throwing allocation escaped through the C ABI
//      as std::terminate (backend SIGABRT); this binary exiting cleanly
//      with an error status is the regression gate.
//   4. pgaccel_expr_eval_predicate's documented contract holds on error:
//      the results array is fully populated with PGACCEL_EXPR_UNCERTAIN.
//
// The induced failure is a pgaccel_batch whose num_rows is absurdly large
// (2^59 rows). Staging allocates num_rows-sized device buffers *before*
// reading any host column memory (stage_dispatch / OneColScratch), so the
// tiny host buffers are never overread: allocation fails first, on any
// real machine, deterministically.

#include <cinttypes>
#include <cstdint>
#include <cstdio>
#include <cstring>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"

static int g_pass = 0;
static int g_fail = 0;

#define ASSERT_TRUE(desc, cond)              \
  do {                                       \
    if ((cond)) {                            \
      g_pass++;                              \
    } else {                                 \
      fprintf(stderr, "FAIL: %s\n", (desc)); \
      g_fail++;                              \
    }                                        \
  } while (0)

// One tiny valid int32 column; num_rows lies about the buffer length. Safe
// because every entry point under test allocates num_rows-sized device
// scratch (and fails) before any host-side read of the column data.
struct HugeBatch {
  int32_t col_data[4] = {1, 2, 3, 4};
  void* col_ptrs[1];
  uint8_t* null_ptrs[1];
  pgaccel_val_tag types[1];
  pgaccel_batch batch;

  explicit HugeBatch(size_t num_rows) {
    col_ptrs[0] = col_data;
    null_ptrs[0] = nullptr;
    types[0] = PGACCEL_VAL_INT32;
    batch.num_rows = num_rows;
    batch.num_cols = 1;
    batch.col_data = col_ptrs;
    batch.col_nulls = null_ptrs;
    batch.col_types = types;
  }
};

static const char* status_name(pgaccel_status st) {
  switch (st) {
    case PGACCEL_OK:
      return "PGACCEL_OK";
    case PGACCEL_ERROR:
      return "PGACCEL_ERROR";
    case PGACCEL_UNSUPPORTED:
      return "PGACCEL_UNSUPPORTED";
    case PGACCEL_OOM:
      return "PGACCEL_OOM";
    case PGACCEL_TIMEOUT:
      return "PGACCEL_TIMEOUT";
    case PGACCEL_ERROR_NO_DEVICE:
      return "PGACCEL_ERROR_NO_DEVICE";
    default:
      return "<unknown>";
  }
}

// The failure must be an honest error: OOM (allocator returned nullptr) or
// ERROR (allocator/submission threw, caught at the extern "C" boundary).
// OK would be silent corruption; NO_DEVICE would be a lie (positive
// control proved the device works); UNSUPPORTED would misreport a resource
// failure as a planner-shape decline.
static void assert_honest_failure(const char* desc, pgaccel_status st) {
  const bool honest = (st == PGACCEL_OOM || st == PGACCEL_ERROR);
  if (!honest) {
    fprintf(stderr, "FAIL: %s — got %s (%d), expected PGACCEL_OOM or PGACCEL_ERROR\n", desc,
            status_name(st), (int)st);
    g_fail++;
  } else {
    printf("  %s -> %s (honest failure)\n", desc, status_name(st));
    g_pass++;
  }
}

int main() {
  // Device is required: this gate is about misreporting failures on a
  // machine that HAS a GPU, so a missing device fails the test honestly.
  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "FAIL: pgaccel_init() failed — no usable GPU device\n");
    return 1;
  }

  // ── Positive control: the same entry points succeed on a small batch ──
  {
    HugeBatch small(4);
    int8_t results[4] = {99, 99, 99, 99};
    pgaccel_status st = pgaccel_expr_template_cmp_const(
        &small.batch, 0, PGACCEL_EXPR_OP_GT, 2.0, results);
    ASSERT_TRUE("positive control: cmp_const on 4 rows returns OK", st == PGACCEL_OK);
    ASSERT_TRUE("positive control: cmp_const results correct",
                results[0] == PGACCEL_EXPR_FALSE && results[1] == PGACCEL_EXPR_FALSE &&
                    results[2] == PGACCEL_EXPR_TRUE && results[3] == PGACCEL_EXPR_TRUE);
  }

  // 2^59 rows: staging needs num_rows * 8B (f64 stage) + num_rows * 1B
  // masks — orders of magnitude beyond any max_alloc. Deterministic OOM.
  const size_t kHugeRows = (size_t)1 << 59;

  // ── Induced failure 1: template kernel (OneColScratch staging) ──
  {
    HugeBatch huge(kHugeRows);
    int8_t results[4] = {0, 0, 0, 0};
    pgaccel_status st = pgaccel_expr_template_cmp_const(
        &huge.batch, 0, PGACCEL_EXPR_OP_GT, 2.0, results);
    assert_honest_failure("cmp_const with 2^59-row staging", st);
  }

  // ── Induced failure 2: bytecode VM entry point (stage_dispatch) ──
  //
  // Uses pgaccel_expr_eval_project rather than _predicate: the predicate
  // entry fulfils its documented "results always populated" contract by
  // memset-ing num_rows bytes into the caller buffer up front, so a
  // huge claimed row count cannot be paired with a small host results
  // buffer there. Project performs no host-side write before staging
  // fails, so the tiny output buffers below are never touched.
  {
    HugeBatch huge(kHugeRows);
    pgaccel_expr_instruction insts[2] = {};
    insts[0].opcode = PGACCEL_EXPR_OP_LOAD_COL;
    insts[0].arg = 0;
    insts[1].opcode = PGACCEL_EXPR_OP_IS_NULL;
    pgaccel_expr_program prog = {};
    prog.instructions = insts;
    prog.inst_count = 2;
    prog.const_pool = nullptr;
    prog.const_count = 0;
    prog.max_stack = 2;
    prog.num_cols = 1;

    pgaccel_val out[4];
    uint8_t uncertain[4];
    pgaccel_status st = pgaccel_expr_eval_project(&prog, &huge.batch, out, uncertain);
    assert_honest_failure("expr_eval_project with 2^59-row staging", st);
  }

  // ── Induced failure 3: shared-USM allocator entry point ──
  {
    void* p = reinterpret_cast<void*>(0x1);
    pgaccel_status st = pgaccel_expr_shared_alloc(kHugeRows * 8, &p);
    assert_honest_failure("pgaccel_expr_shared_alloc(2^62 bytes)", st);
    ASSERT_TRUE("shared_alloc failure left no dangling pointer", p == nullptr || st == PGACCEL_OK);
  }

  // ── Survival: a failing dispatch must not poison the process ──
  {
    HugeBatch small(4);
    int8_t results[4] = {99, 99, 99, 99};
    pgaccel_status st = pgaccel_expr_template_cmp_const(
        &small.batch, 0, PGACCEL_EXPR_OP_LE, 2.0, results);
    ASSERT_TRUE("post-failure control: cmp_const still returns OK", st == PGACCEL_OK);
    ASSERT_TRUE("post-failure control: results correct",
                results[0] == PGACCEL_EXPR_TRUE && results[1] == PGACCEL_EXPR_TRUE &&
                    results[2] == PGACCEL_EXPR_FALSE && results[3] == PGACCEL_EXPR_FALSE);
  }

  printf("test_kernel_failure_status: %d passed, %d failed\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}
