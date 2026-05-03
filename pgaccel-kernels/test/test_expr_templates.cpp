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
pgaccel_status pgaccel_expr_template_two_pred_and(const pgaccel_batch* batch, uint32_t col1_idx,
                                                  uint16_t cmp1_opcode, double const1_val,
                                                  uint32_t col2_idx, uint16_t cmp2_opcode,
                                                  double const2_val, int8_t* results);
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
// Helper: build a two-column int64 batch from two vectors
// ---------------------------------------------------------------------------
struct TwoColI64Batch {
  std::vector<int64_t> data0;
  std::vector<int64_t> data1;
  std::vector<uint8_t> nulls0;
  std::vector<uint8_t> nulls1;
  void* col_data_ptrs[2];
  uint8_t* col_null_ptrs[2];
  pgaccel_val_tag col_types[2];
  pgaccel_batch batch;

  TwoColI64Batch(std::vector<int64_t> v0, std::vector<int64_t> v1, std::vector<uint8_t> n0,
                 std::vector<uint8_t> n1)
      : data0(std::move(v0)), data1(std::move(v1)), nulls0(std::move(n0)), nulls1(std::move(n1)) {
    col_data_ptrs[0] = data0.data();
    col_data_ptrs[1] = data1.data();
    col_null_ptrs[0] = nulls0.data();
    col_null_ptrs[1] = nulls1.data();
    col_types[0] = PGACCEL_VAL_INT64;
    col_types[1] = PGACCEL_VAL_INT64;
    batch.num_rows = data0.size();
    batch.num_cols = 2;
    batch.col_data = col_data_ptrs;
    batch.col_nulls = col_null_ptrs;
    batch.col_types = col_types;
  }
};

// ---------------------------------------------------------------------------
// Test: two_pred_and — col0 < 5 AND col1 > 10
// ---------------------------------------------------------------------------
//
// Validates the Metal-SSCP capture-pack refactor (10 captures → 1 struct):
// - kernel must JIT cleanly with NO `xcrun metal failed` warnings
// - result buffer must contain correct AND-of-predicates per row, NOT
//   the uninitialized state that the pre-refactor argbuffer-path failure
//   left behind
//
// Restores the assertions excluded in commit `8808636`'s
// "intentionally NOT tested" note.
static void test_two_pred_and() {
  printf("--- test_two_pred_and ---\n");

  // 13 rows. col0 = {0..9, 1, 2, 3}; col1 = {2,4,6,8,10,12,14,16,18,20,11,15,100}.
  // Predicate: col0 < 5 AND col1 > 10.
  std::vector<int64_t> v0 = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 1, 2, 3};
  std::vector<int64_t> v1 = {2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 11, 15, 100};
  std::vector<uint8_t> n0(13, 0);
  std::vector<uint8_t> n1(13, 0);
  TwoColI64Batch b(std::move(v0), std::move(v1), std::move(n0), std::move(n1));

  int8_t results[13] = {};
  pgaccel_status s = pgaccel_expr_template_two_pred_and(&b.batch, 0, PGACCEL_EXPR_OP_LT, 5.0, 1,
                                                        PGACCEL_EXPR_OP_GT, 10.0, results);
  ASSERT_STATUS_OK("two_pred_and status", s);

  // Manually computed expected: (col0 < 5) AND (col1 > 10).
  // Rows 0-4: col0 in 0..4 (T) but col1 in 2..10 (all <= 10 -> F) -> F.
  // Rows 5-9: col0 in 5..9 (F) -> F.
  // Rows 10-12: col0 in {1,2,3} (T) and col1 in {11,15,100} (T) -> T.
  int8_t expected[13] = {
      PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE,
      PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE,
      PGACCEL_EXPR_FALSE, PGACCEL_EXPR_FALSE, PGACCEL_EXPR_TRUE,  PGACCEL_EXPR_TRUE,
      PGACCEL_EXPR_TRUE,
  };

  bool ok = true;
  for (int i = 0; i < 13; i++) {
    if (results[i] != expected[i]) {
      fprintf(stderr, "  row %d (col0=%lld, col1=%lld): got %d, expected %d\n", i,
              static_cast<long long>(b.data0[i]), static_cast<long long>(b.data1[i]), results[i],
              expected[i]);
      ok = false;
    }
  }
  ASSERT_TRUE("two_pred_and (col0 < 5 AND col1 > 10) matches expected", ok);
}

// ---------------------------------------------------------------------------
// Test: two_pred_and — null propagation
// ---------------------------------------------------------------------------
//
// Per kernel contract, NULL on either column → FALSE for that row.
static void test_two_pred_and_nulls() {
  printf("--- test_two_pred_and_nulls ---\n");

  // 5 rows. col0 = 1..5, col1 = 11..15. Both predicates would otherwise be
  // TRUE for all rows (col0 < 10 AND col1 > 5). Mark row 1's col0 NULL and
  // row 3's col1 NULL.
  std::vector<int64_t> v0 = {1, 2, 3, 4, 5};
  std::vector<int64_t> v1 = {11, 12, 13, 14, 15};
  std::vector<uint8_t> n0 = {0, 1, 0, 0, 0};
  std::vector<uint8_t> n1 = {0, 0, 0, 1, 0};
  TwoColI64Batch b(std::move(v0), std::move(v1), std::move(n0), std::move(n1));

  int8_t results[5] = {};
  pgaccel_status s = pgaccel_expr_template_two_pred_and(&b.batch, 0, PGACCEL_EXPR_OP_LT, 10.0, 1,
                                                        PGACCEL_EXPR_OP_GT, 5.0, results);
  ASSERT_STATUS_OK("two_pred_and_nulls status", s);

  ASSERT_TRUE("row 0 (no nulls) → TRUE", results[0] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 1 (col0 NULL) → FALSE", results[1] == PGACCEL_EXPR_FALSE);
  ASSERT_TRUE("row 2 (no nulls) → TRUE", results[2] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 3 (col1 NULL) → FALSE", results[3] == PGACCEL_EXPR_FALSE);
  ASSERT_TRUE("row 4 (no nulls) → TRUE", results[4] == PGACCEL_EXPR_TRUE);
}

// ---------------------------------------------------------------------------
// Test: two_pred_and rejects unsupported opcodes on either side
// ---------------------------------------------------------------------------
static void test_two_pred_and_rejects_unsupported_opcode() {
  printf("--- test_two_pred_and_rejects_unsupported_opcode ---\n");

  std::vector<int64_t> v0 = {1};
  std::vector<int64_t> v1 = {1};
  std::vector<uint8_t> n0 = {0};
  std::vector<uint8_t> n1 = {0};
  TwoColI64Batch b(std::move(v0), std::move(v1), std::move(n0), std::move(n1));

  int8_t results[1] = {};
  // Bad opcode on side 1.
  pgaccel_status s1 = pgaccel_expr_template_two_pred_and(&b.batch, 0, 999, 0.0, 1,
                                                         PGACCEL_EXPR_OP_EQ, 0.0, results);
  ASSERT_TRUE("two_pred_and bad op1 → UNSUPPORTED", s1 == PGACCEL_ERROR_UNSUPPORTED);

  // Bad opcode on side 2.
  pgaccel_status s2 = pgaccel_expr_template_two_pred_and(&b.batch, 0, PGACCEL_EXPR_OP_EQ, 0.0, 1,
                                                         999, 0.0, results);
  ASSERT_TRUE("two_pred_and bad op2 → UNSUPPORTED", s2 == PGACCEL_ERROR_UNSUPPORTED);
}

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
  test_two_pred_and();
  test_two_pred_and_nulls();
  test_two_pred_and_rejects_unsupported_opcode();

  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}
