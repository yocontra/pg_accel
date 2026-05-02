// Standalone correctness tests for the predicate-template SYCL kernels
// in pgaccel-kernels/src/expr_templates.cpp. These 5 fast-path
// templates (cmp_const, between, in_list, is_null, two_pred_and) had
// no kernel-level test coverage despite being on the hot WHERE-clause
// path for every accelerated query that classifies into one of these
// shapes.
//
// Each kernel returns:
//   PGACCEL_EXPR_TRUE  = +1  (predicate evaluated true)
//   PGACCEL_EXPR_FALSE = -1  (predicate evaluated false; also the
//                             default for NULL inputs in these
//                             templates — null propagation is
//                             handled at the executor layer, not
//                             here)
//   PGACCEL_EXPR_UNCERTAIN = 0  (returned by some other expr kernels
//                             for overflow / domain errors; not
//                             exercised by these templates)

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

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

#define ASSERT_STATUS_OK(desc, status)                                                \
  do {                                                                                \
    if ((status) == PGACCEL_OK) {                                                     \
      g_pass++;                                                                       \
    } else {                                                                          \
      fprintf(stderr, "FAIL: %s — status %d (expected OK)\n", (desc), (int)(status)); \
      g_fail++;                                                                       \
    }                                                                                 \
  } while (0)

extern "C" {
pgaccel_status pgaccel_expr_template_cmp_const(const pgaccel_batch* batch, uint32_t col_idx,
                                               uint16_t cmp_opcode, double const_val,
                                               int8_t* results);
pgaccel_status pgaccel_expr_template_between(const pgaccel_batch* batch, uint32_t col_idx,
                                             double lo, double hi, int8_t* results);
pgaccel_status pgaccel_expr_template_in_list(const pgaccel_batch* batch, uint32_t col_idx,
                                             const double* values, size_t value_count,
                                             int8_t* results);
pgaccel_status pgaccel_expr_template_is_null(const pgaccel_batch* batch, uint32_t col_idx,
                                             bool check_not_null, int8_t* results);
// pgaccel_expr_template_two_pred_and intentionally NOT declared — see the
// note further down explaining the broken-and-unused status.
}

// ---------------------------------------------------------------------------
// Helper: build a single-column int64 batch from a vector
// ---------------------------------------------------------------------------
struct OneColI64Batch {
  std::vector<int64_t> data;
  std::vector<uint8_t> nulls;
  void* col_data_ptrs[1];
  uint8_t* col_null_ptrs[1];
  pgaccel_val_tag col_types[1];
  pgaccel_batch batch;

  OneColI64Batch(std::vector<int64_t> values, std::vector<uint8_t> null_mask)
      : data(std::move(values)), nulls(std::move(null_mask)) {
    col_data_ptrs[0] = data.data();
    col_null_ptrs[0] = nulls.data();
    col_types[0] = PGACCEL_VAL_INT64;
    batch.num_rows = data.size();
    batch.num_cols = 1;
    batch.col_data = col_data_ptrs;
    batch.col_nulls = col_null_ptrs;
    batch.col_types = col_types;
  }
};

// ---------------------------------------------------------------------------
// Test: cmp_const — col < 5 / col = 5 / col > 5 / col != 5
// ---------------------------------------------------------------------------
static void test_cmp_const() {
  printf("--- test_cmp_const ---\n");

  // 10 rows, values 0..9, no nulls.
  std::vector<int64_t> vals = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9};
  std::vector<uint8_t> nulls(10, 0);
  OneColI64Batch b(std::move(vals), std::move(nulls));

  // col < 5
  {
    int8_t results[10] = {};
    pgaccel_status s =
        pgaccel_expr_template_cmp_const(&b.batch, 0, PGACCEL_EXPR_OP_LT, 5.0, results);
    ASSERT_STATUS_OK("cmp_const LT status", s);
    bool ok = true;
    for (int i = 0; i < 10; i++) {
      int8_t expect = (i < 5) ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
      if (results[i] != expect) {
        fprintf(stderr, "  LT row %d: got %d, expected %d\n", i, results[i], expect);
        ok = false;
      }
    }
    ASSERT_TRUE("cmp_const col < 5 matches expected", ok);
  }

  // col = 5
  {
    int8_t results[10] = {};
    pgaccel_expr_template_cmp_const(&b.batch, 0, PGACCEL_EXPR_OP_EQ, 5.0, results);
    bool ok = true;
    for (int i = 0; i < 10; i++) {
      int8_t expect = (i == 5) ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
      if (results[i] != expect) {
        fprintf(stderr, "  EQ row %d: got %d, expected %d\n", i, results[i], expect);
        ok = false;
      }
    }
    ASSERT_TRUE("cmp_const col = 5 matches expected", ok);
  }

  // col >= 7
  {
    int8_t results[10] = {};
    pgaccel_expr_template_cmp_const(&b.batch, 0, PGACCEL_EXPR_OP_GE, 7.0, results);
    bool ok = true;
    for (int i = 0; i < 10; i++) {
      int8_t expect = (i >= 7) ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
      if (results[i] != expect) {
        fprintf(stderr, "  GE row %d: got %d, expected %d\n", i, results[i], expect);
        ok = false;
      }
    }
    ASSERT_TRUE("cmp_const col >= 7 matches expected", ok);
  }
}

// ---------------------------------------------------------------------------
// Test: cmp_const with NULLs — null rows return FALSE per kernel contract
// ---------------------------------------------------------------------------
static void test_cmp_const_nulls() {
  printf("--- test_cmp_const_nulls ---\n");

  std::vector<int64_t> vals = {1, 2, 3, 4};
  std::vector<uint8_t> nulls = {0, 1, 0, 1};  // rows 1 and 3 are null
  OneColI64Batch b(std::move(vals), std::move(nulls));

  int8_t results[4] = {};
  pgaccel_status s = pgaccel_expr_template_cmp_const(&b.batch, 0, PGACCEL_EXPR_OP_GT, 0.0, results);
  ASSERT_STATUS_OK("cmp_const_nulls status", s);

  // All non-null values are > 0, so non-null rows → TRUE; null rows → FALSE.
  ASSERT_TRUE("non-null row 0 → TRUE", results[0] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("null row 1 → FALSE", results[1] == PGACCEL_EXPR_FALSE);
  ASSERT_TRUE("non-null row 2 → TRUE", results[2] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("null row 3 → FALSE", results[3] == PGACCEL_EXPR_FALSE);
}

// ---------------------------------------------------------------------------
// Test: between — col BETWEEN 3 AND 7 (inclusive)
// ---------------------------------------------------------------------------
static void test_between() {
  printf("--- test_between ---\n");

  std::vector<int64_t> vals = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9};
  std::vector<uint8_t> nulls(10, 0);
  OneColI64Batch b(std::move(vals), std::move(nulls));

  int8_t results[10] = {};
  pgaccel_status s = pgaccel_expr_template_between(&b.batch, 0, 3.0, 7.0, results);
  ASSERT_STATUS_OK("between status", s);

  bool ok = true;
  for (int i = 0; i < 10; i++) {
    int8_t expect = (i >= 3 && i <= 7) ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
    if (results[i] != expect) {
      fprintf(stderr, "  row %d: got %d, expected %d\n", i, results[i], expect);
      ok = false;
    }
  }
  ASSERT_TRUE("col BETWEEN 3 AND 7 matches expected", ok);
}

// ---------------------------------------------------------------------------
// Test: in_list — col IN (1, 5, 9)
// ---------------------------------------------------------------------------
static void test_in_list() {
  printf("--- test_in_list ---\n");

  std::vector<int64_t> vals = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9};
  std::vector<uint8_t> nulls(10, 0);
  OneColI64Batch b(std::move(vals), std::move(nulls));

  double in_vals[3] = {1.0, 5.0, 9.0};
  int8_t results[10] = {};
  pgaccel_status s = pgaccel_expr_template_in_list(&b.batch, 0, in_vals, 3, results);
  ASSERT_STATUS_OK("in_list status", s);

  bool ok = true;
  for (int i = 0; i < 10; i++) {
    int8_t expect = (i == 1 || i == 5 || i == 9) ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
    if (results[i] != expect) {
      fprintf(stderr, "  row %d (val=%d): got %d, expected %d\n", i, i, results[i], expect);
      ok = false;
    }
  }
  ASSERT_TRUE("col IN (1, 5, 9) matches expected", ok);
}

// ---------------------------------------------------------------------------
// Test: in_list rejects > 16 values per kernel contract
// ---------------------------------------------------------------------------
static void test_in_list_too_large() {
  printf("--- test_in_list_too_large ---\n");

  std::vector<int64_t> vals = {1};
  std::vector<uint8_t> nulls = {0};
  OneColI64Batch b(std::move(vals), std::move(nulls));

  double in_vals[20] = {};
  int8_t results[1] = {};
  pgaccel_status s = pgaccel_expr_template_in_list(&b.batch, 0, in_vals, 20, results);
  ASSERT_TRUE("in_list with 20 values returns UNSUPPORTED", s == PGACCEL_ERROR_UNSUPPORTED);
}

// ---------------------------------------------------------------------------
// Test: is_null / is_not_null
// ---------------------------------------------------------------------------
static void test_is_null() {
  printf("--- test_is_null ---\n");

  std::vector<int64_t> vals = {1, 2, 3, 4, 5};
  std::vector<uint8_t> nulls = {0, 1, 0, 1, 0};
  OneColI64Batch b(std::move(vals), std::move(nulls));

  // IS NULL — true for rows 1, 3
  {
    int8_t results[5] = {};
    pgaccel_status s = pgaccel_expr_template_is_null(&b.batch, 0, false, results);
    ASSERT_STATUS_OK("is_null status", s);
    ASSERT_TRUE("IS NULL row 0 → FALSE", results[0] == PGACCEL_EXPR_FALSE);
    ASSERT_TRUE("IS NULL row 1 → TRUE", results[1] == PGACCEL_EXPR_TRUE);
    ASSERT_TRUE("IS NULL row 2 → FALSE", results[2] == PGACCEL_EXPR_FALSE);
    ASSERT_TRUE("IS NULL row 3 → TRUE", results[3] == PGACCEL_EXPR_TRUE);
    ASSERT_TRUE("IS NULL row 4 → FALSE", results[4] == PGACCEL_EXPR_FALSE);
  }

  // IS NOT NULL — inverse of above
  {
    int8_t results[5] = {};
    pgaccel_expr_template_is_null(&b.batch, 0, true, results);
    ASSERT_TRUE("IS NOT NULL row 0 → TRUE", results[0] == PGACCEL_EXPR_TRUE);
    ASSERT_TRUE("IS NOT NULL row 1 → FALSE", results[1] == PGACCEL_EXPR_FALSE);
    ASSERT_TRUE("IS NOT NULL row 2 → TRUE", results[2] == PGACCEL_EXPR_TRUE);
    ASSERT_TRUE("IS NOT NULL row 3 → FALSE", results[3] == PGACCEL_EXPR_FALSE);
    ASSERT_TRUE("IS NOT NULL row 4 → TRUE", results[4] == PGACCEL_EXPR_TRUE);
  }
}

// ---------------------------------------------------------------------------
// NOTE: pgaccel_expr_template_two_pred_and is intentionally NOT tested here.
// ---------------------------------------------------------------------------
//
// The C++ kernel exists (pgaccel-kernels/src/expr_templates.cpp:423) and the
// Rust bridge declares it (pg_accel/src/gpu/bridge.rs:484), but no Rust
// executor code path actually calls it — `scan/exec.rs:558-593`,
// `agg/execute.rs:1539,1601`, and `preagg/mod.rs:457,617` all handle the
// `TemplateKernel::TwoPredAnd` variant by calling
// `pgaccel_expr_template_cmp_const` TWICE and AND-ing the results in Rust.
//
// An exploratory test of this kernel (commit 6a8a78b session, dropped before
// commit) confirmed that JIT compilation fails with
//   error: attribute 'id' set location to 4, but minimum is 5
//   device void* t4 [[id(4)]];
// — same Metal `[[id(N)]]` argument-buffer-collision class as the documented
// Phase 7 `__args` MSL compile error on `reduce_stats_f64` at large N.
// The kernel returns PGACCEL_OK because `parallel_for(...).wait()` doesn't
// surface the JIT failure, but the result buffer stays at its uninitialized
// state. This is harmless today because nothing calls it, but the latent
// bug (and the question of whether to delete the kernel or fix it) is
// recorded in TODO Phase 7 alongside the existing `__args` entry.

// ---------------------------------------------------------------------------
// Test: cmp_const rejects unsupported opcode
// ---------------------------------------------------------------------------
static void test_cmp_const_rejects_unsupported_opcode() {
  printf("--- test_cmp_const_rejects_unsupported_opcode ---\n");

  std::vector<int64_t> vals = {1};
  std::vector<uint8_t> nulls = {0};
  OneColI64Batch b(std::move(vals), std::move(nulls));

  int8_t results[1] = {};
  // 999 is not a valid comparison opcode — kernel must reject.
  pgaccel_status s = pgaccel_expr_template_cmp_const(&b.batch, 0, 999, 0.0, results);
  ASSERT_TRUE("invalid opcode → UNSUPPORTED", s == PGACCEL_ERROR_UNSUPPORTED);
}

// ---------------------------------------------------------------------------
// Test: empty input (num_rows == 0)
// ---------------------------------------------------------------------------
static void test_empty_batch() {
  printf("--- test_empty_batch ---\n");

  std::vector<int64_t> vals;
  std::vector<uint8_t> nulls;
  OneColI64Batch b(std::move(vals), std::move(nulls));

  int8_t results[1] = {};
  pgaccel_status s = pgaccel_expr_template_cmp_const(&b.batch, 0, PGACCEL_EXPR_OP_EQ, 0.0, results);
  ASSERT_TRUE("empty batch → OK", s == PGACCEL_OK);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
int main() {
  printf("=== pg_accel expr_templates kernel tests ===\n\n");

  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "FATAL: pgaccel_init() failed; cannot run expr_template tests\n");
    return 1;
  }

  test_empty_batch();
  test_cmp_const_rejects_unsupported_opcode();
  test_cmp_const();
  test_cmp_const_nulls();
  test_between();
  test_in_list();
  test_in_list_too_large();
  test_is_null();

  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}
