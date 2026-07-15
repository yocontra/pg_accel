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
#include <limits>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_fused.h"

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
pgaccel_status pgaccel_expr_shared_alloc(size_t bytes, void** out);
void pgaccel_expr_shared_free(void* ptr);
pgaccel_status pgaccel_expr_template_cmp_const(const pgaccel_batch* batch, uint32_t col_idx,
                                               uint16_t cmp_opcode, double const_val,
                                               int8_t* results);
pgaccel_status pgaccel_expr_template_cmp_const_count(const pgaccel_batch* batch, uint32_t col_idx,
                                                     uint16_t cmp_opcode, double const_val,
                                                     size_t* true_count, size_t* uncertain_count);
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
pgaccel_status pgaccel_expr_template_two_pred_and_count(const pgaccel_batch* batch,
                                                        uint32_t col1_idx, uint16_t cmp1_opcode,
                                                        double const1_val, uint32_t col2_idx,
                                                        uint16_t cmp2_opcode, double const2_val,
                                                        size_t* true_count,
                                                        size_t* uncertain_count);
pgaccel_status pgaccel_expr_template_cmp_const_count_usm(pgaccel_expr_usm_col col, size_t row_count,
                                                         uint16_t cmp_opcode, double const_val,
                                                         size_t* true_count,
                                                         size_t* uncertain_count);
pgaccel_status pgaccel_expr_template_cmp_const_mask_usm(pgaccel_expr_usm_col col, size_t row_count,
                                                        uint16_t cmp_opcode, double const_val,
                                                        uint8_t* selection, size_t* true_count,
                                                        size_t* uncertain_count);
pgaccel_status pgaccel_expr_template_cmp_const_reduce_f32_usm(
    pgaccel_expr_usm_col pred_col, uint16_t cmp_opcode, double const_val,
    pgaccel_expr_usm_col value_col, size_t row_count, float* out_sum, float* out_min,
    float* out_max, int64_t* out_value_count, size_t* true_count, size_t* uncertain_count);
pgaccel_status pgaccel_expr_template_two_pred_and_count_usm(pgaccel_expr_usm_col col1,
                                                            uint16_t cmp1_opcode, double const1_val,
                                                            pgaccel_expr_usm_col col2,
                                                            uint16_t cmp2_opcode, double const2_val,
                                                            size_t row_count, size_t* true_count,
                                                            size_t* uncertain_count);
pgaccel_status pgaccel_expr_template_two_pred_and_mask_usm(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, size_t row_count, uint8_t* selection,
    size_t* true_count, size_t* uncertain_count);
pgaccel_status pgaccel_expr_template_two_pred_and_reduce_f32_usm(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, pgaccel_expr_usm_col value_col, size_t row_count,
    float* out_sum, float* out_min, float* out_max, int64_t* out_value_count, size_t* true_count,
    size_t* uncertain_count);
}

template <typename T>
static T* alloc_shared_array(const char* desc, size_t count) {
  void* ptr = nullptr;
  pgaccel_status s = pgaccel_expr_shared_alloc(count * sizeof(T), &ptr);
  ASSERT_STATUS_OK(desc, s);
  ASSERT_TRUE(desc, ptr != nullptr);
  return static_cast<T*>(ptr);
}

static void test_expr_shared_alloc_free() {
  printf("--- test_expr_shared_alloc_free ---\n");

  void* ptr = nullptr;
  pgaccel_status s = pgaccel_expr_shared_alloc(64, &ptr);
  ASSERT_STATUS_OK("shared alloc status", s);
  ASSERT_TRUE("shared alloc returns pointer", ptr != nullptr);
  if (ptr != nullptr) {
    std::memset(ptr, 0xab, 64);
    pgaccel_expr_shared_free(ptr);
  }

  void* zero = reinterpret_cast<void*>(0x1);
  s = pgaccel_expr_shared_alloc(0, &zero);
  ASSERT_STATUS_OK("zero-byte shared alloc status", s);
  ASSERT_TRUE("zero-byte shared alloc returns null pointer", zero == nullptr);

  s = pgaccel_expr_shared_alloc(16, nullptr);
  ASSERT_TRUE("shared alloc rejects null out pointer", s == PGACCEL_ERROR);
  pgaccel_expr_shared_free(nullptr);
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

struct OneColI32Batch {
  std::vector<int32_t> data;
  std::vector<uint8_t> nulls;
  void* col_data_ptrs[1];
  uint8_t* col_null_ptrs[1];
  pgaccel_val_tag col_types[1];
  pgaccel_batch batch;

  OneColI32Batch(std::vector<int32_t> values, std::vector<uint8_t> null_mask)
      : data(std::move(values)), nulls(std::move(null_mask)) {
    col_data_ptrs[0] = data.data();
    col_null_ptrs[0] = nulls.data();
    col_types[0] = PGACCEL_VAL_INT32;
    batch.num_rows = data.size();
    batch.num_cols = 1;
    batch.col_data = col_data_ptrs;
    batch.col_nulls = col_null_ptrs;
    batch.col_types = col_types;
  }
};

struct OneColF64Batch {
  std::vector<double> data;
  std::vector<uint8_t> nulls;
  void* col_data_ptrs[1];
  uint8_t* col_null_ptrs[1];
  pgaccel_val_tag col_types[1];
  pgaccel_batch batch;

  OneColF64Batch(std::vector<double> values, std::vector<uint8_t> null_mask)
      : data(std::move(values)), nulls(std::move(null_mask)) {
    col_data_ptrs[0] = data.data();
    col_null_ptrs[0] = nulls.data();
    col_types[0] = PGACCEL_VAL_FLOAT64;
    batch.num_rows = data.size();
    batch.num_cols = 1;
    batch.col_data = col_data_ptrs;
    batch.col_nulls = col_null_ptrs;
    batch.col_types = col_types;
  }
};

struct OneColF32Batch {
  std::vector<float> data;
  std::vector<uint8_t> nulls;
  void* col_data_ptrs[1];
  uint8_t* col_null_ptrs[1];
  pgaccel_val_tag col_types[1];
  pgaccel_batch batch;

  OneColF32Batch(std::vector<float> values, std::vector<uint8_t> null_mask)
      : data(std::move(values)), nulls(std::move(null_mask)) {
    col_data_ptrs[0] = data.data();
    col_null_ptrs[0] = nulls.data();
    col_types[0] = PGACCEL_VAL_FLOAT32;
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

static void test_cmp_const_count() {
  printf("--- test_cmp_const_count ---\n");

  std::vector<int64_t> vals = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9};
  std::vector<uint8_t> nulls(10, 0);
  OneColI64Batch b(std::move(vals), std::move(nulls));

  size_t true_count = 999;
  size_t uncertain_count = 999;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count(&b.batch, 0, PGACCEL_EXPR_OP_GE, 6.0,
                                                           &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count status", s);
  ASSERT_TRUE("cmp_const_count true rows", true_count == 4);
  ASSERT_TRUE("cmp_const_count uncertain rows", uncertain_count == 0);
}

static void test_cmp_const_count_nulls() {
  printf("--- test_cmp_const_count_nulls ---\n");

  std::vector<int64_t> vals = {1, 2, 3, 4, 5};
  std::vector<uint8_t> nulls = {0, 1, 0, 1, 0};
  OneColI64Batch b(std::move(vals), std::move(nulls));

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count(&b.batch, 0, PGACCEL_EXPR_OP_GT, 0.0,
                                                           &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count nulls status", s);
  ASSERT_TRUE("cmp_const_count skips null rows", true_count == 3);
  ASSERT_TRUE("cmp_const_count nulls uncertain rows", uncertain_count == 0);
}

static void test_cmp_const_count_with_null_col_entry() {
  printf("--- test_cmp_const_count_with_null_col_entry ---\n");

  std::vector<float> vals = {1.0f, 5.0f, 7.0f, 9.0f};
  std::vector<uint8_t> nulls = {1, 1, 1, 1};
  OneColF32Batch b(std::move(vals), std::move(nulls));
  b.col_null_ptrs[0] = nullptr;

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count(&b.batch, 0, PGACCEL_EXPR_OP_GT, 6.0,
                                                           &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count f32 null col entry status", s);
  ASSERT_TRUE("cmp_const_count f32 null col entry treats all rows valid", true_count == 2);
  ASSERT_TRUE("cmp_const_count f32 null col entry uncertain rows", uncertain_count == 0);
}

static void test_cmp_const_count_nan_semantics() {
  printf("--- test_cmp_const_count_nan_semantics ---\n");

  const double nan = std::numeric_limits<double>::quiet_NaN();
  std::vector<double> vals = {1.0, nan, 2.0, nan};
  std::vector<uint8_t> nulls(4, 0);
  OneColF64Batch b(std::move(vals), std::move(nulls));

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count(&b.batch, 0, PGACCEL_EXPR_OP_EQ, nan,
                                                           &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count NaN equality status", s);
  ASSERT_TRUE("PG NaN = NaN rows counted", true_count == 2);
  ASSERT_TRUE("cmp_const_count NaN uncertain rows", uncertain_count == 0);
}

static void test_cmp_const_count_f32_fast_path() {
  printf("--- test_cmp_const_count_f32_fast_path ---\n");

  const float nan = std::numeric_limits<float>::quiet_NaN();
  std::vector<float> vals = {1.0f, nan, 2.5f, 5.0f, 7.5f};
  std::vector<uint8_t> nulls(5, 0);
  OneColF32Batch b(std::move(vals), std::move(nulls));

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count(&b.batch, 0, PGACCEL_EXPR_OP_GT, 2.5,
                                                           &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count f32 fast-path status", s);
  ASSERT_TRUE("cmp_const_count f32 PG NaN ordering", true_count == 3);
  ASSERT_TRUE("cmp_const_count f32 uncertain rows", uncertain_count == 0);
}

static void test_cmp_const_count_i32_non_integral_const_fallback() {
  printf("--- test_cmp_const_count_i32_non_integral_const_fallback ---\n");

  std::vector<int32_t> vals = {48, 49, 50, 51};
  std::vector<uint8_t> nulls(4, 0);
  OneColI32Batch b(std::move(vals), std::move(nulls));

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count(&b.batch, 0, PGACCEL_EXPR_OP_LT, 49.5,
                                                           &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count i32 non-integral const fallback status", s);
  ASSERT_TRUE("cmp_const_count i32 fallback preserves fractional boundary", true_count == 2);
  ASSERT_TRUE("cmp_const_count i32 fallback uncertain rows", uncertain_count == 0);
}

static void test_cmp_const_count_usm() {
  printf("--- test_cmp_const_count_usm ---\n");

  constexpr size_t n = 4;
  float* vals = alloc_shared_array<float>("cmp_const_count_usm values alloc", n);
  uint8_t* nulls = alloc_shared_array<uint8_t>("cmp_const_count_usm nulls alloc", n);
  if (vals == nullptr || nulls == nullptr) {
    pgaccel_expr_shared_free(vals);
    pgaccel_expr_shared_free(nulls);
    return;
  }

  vals[0] = 1.0f;
  vals[1] = 8.0f;
  vals[2] = 7.0f;
  vals[3] = 9.0f;
  nulls[0] = 0;
  nulls[1] = 1;
  nulls[2] = 0;
  nulls[3] = 0;

  pgaccel_expr_usm_col col{vals, nulls, PGACCEL_VAL_FLOAT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count_usm(col, n, PGACCEL_EXPR_OP_GT, 6.0,
                                                               &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count_usm explicit null mask status", s);
  ASSERT_TRUE("cmp_const_count_usm skips explicit null rows", true_count == 2);
  ASSERT_TRUE("cmp_const_count_usm explicit null mask uncertain rows", uncertain_count == 0);

  col.nulls = nullptr;
  true_count = 0;
  uncertain_count = 0;
  s = pgaccel_expr_template_cmp_const_count_usm(col, n, PGACCEL_EXPR_OP_GT, 6.0, &true_count,
                                                &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count_usm null mask pointer status", s);
  ASSERT_TRUE("cmp_const_count_usm null mask pointer treats all rows valid", true_count == 3);
  ASSERT_TRUE("cmp_const_count_usm null mask pointer uncertain rows", uncertain_count == 0);

  pgaccel_expr_shared_free(nulls);
  pgaccel_expr_shared_free(vals);
}

static void test_cmp_const_count_usm_multigroup_all_valid() {
  printf("--- test_cmp_const_count_usm_multigroup_all_valid ---\n");

  constexpr size_t n = 1025;
  float* vals = alloc_shared_array<float>("cmp_const_count_usm multigroup values alloc", n);
  if (vals == nullptr) {
    return;
  }

  for (size_t i = 0; i < n; ++i) {
    vals[i] = static_cast<float>(i);
  }

  pgaccel_expr_usm_col col{vals, nullptr, PGACCEL_VAL_FLOAT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count_usm(col, n, PGACCEL_EXPR_OP_GT, 512.0,
                                                               &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count_usm multigroup status", s);
  ASSERT_TRUE("cmp_const_count_usm multigroup true rows", true_count == 512);
  ASSERT_TRUE("cmp_const_count_usm multigroup uncertain rows", uncertain_count == 0);

  pgaccel_expr_shared_free(vals);
}

static void test_cmp_const_count_usm_device_finalizer_many_groups() {
  printf("--- test_cmp_const_count_usm_device_finalizer_many_groups ---\n");

  // More than 256 first-stage groups forces every finalizer lane to consume
  // multiple partials, exercising the device reduction rather than a one-pass
  // copy of a small partial array.
  constexpr size_t n = 300123;
  constexpr size_t miss_stride = 997;
  int32_t* vals =
      alloc_shared_array<int32_t>("cmp_const_count_usm device finalizer values alloc", n);
  if (vals == nullptr)
    return;

  for (size_t i = 0; i < n; ++i) {
    vals[i] = (i % miss_stride == 0) ? 0 : 1;
  }
  const size_t expected = n - ((n - 1) / miss_stride + 1);

  pgaccel_expr_usm_col col{vals, nullptr, PGACCEL_VAL_INT32};
  size_t true_count = 0;
  size_t uncertain_count = 99;
  pgaccel_reset_gpu_exec_count();
  const uint64_t before = pgaccel_gpu_exec_count();
  const pgaccel_status status = pgaccel_expr_template_cmp_const_count_usm(
      col, n, PGACCEL_EXPR_OP_EQ, 1.0, &true_count, &uncertain_count);

  ASSERT_STATUS_OK("cmp_const_count_usm device finalizer status", status);
  ASSERT_TRUE("cmp_const_count_usm device finalizer exact count", true_count == expected);
  ASSERT_TRUE("cmp_const_count_usm device finalizer uncertain rows", uncertain_count == 0);
  ASSERT_TRUE("cmp_const_count_usm device finalizer records GPU operation",
              pgaccel_gpu_exec_count() == before + 1);

  pgaccel_expr_shared_free(vals);
}

static void test_cmp_const_count_usm_strided_rows_and_nulls() {
  printf("--- test_cmp_const_count_usm_strided_rows_and_nulls ---\n");

  constexpr size_t n = 1025;
  int32_t* vals = alloc_shared_array<int32_t>("cmp_const_count_usm stride values alloc", n);
  uint8_t* nulls = alloc_shared_array<uint8_t>("cmp_const_count_usm stride nulls alloc", n);
  if (vals == nullptr || nulls == nullptr) {
    pgaccel_expr_shared_free(vals);
    pgaccel_expr_shared_free(nulls);
    return;
  }

  for (size_t i = 0; i < n; ++i) {
    vals[i] = 1;
    nulls[i] = 0;
  }
  nulls[300] = 1;
  nulls[1024] = 1;

  pgaccel_expr_usm_col col{vals, nulls, PGACCEL_VAL_INT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count_usm(col, n, PGACCEL_EXPR_OP_EQ, 1.0,
                                                               &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count_usm stride status", s);
  ASSERT_TRUE("cmp_const_count_usm stride visits late rows and nulls", true_count == 1023);
  ASSERT_TRUE("cmp_const_count_usm stride uncertain rows", uncertain_count == 0);

  pgaccel_expr_shared_free(nulls);
  pgaccel_expr_shared_free(vals);
}

static void test_cmp_const_count_usm_f32_nan_const() {
  printf("--- test_cmp_const_count_usm_f32_nan_const ---\n");

  constexpr size_t n = 4;
  const float nan = std::numeric_limits<float>::quiet_NaN();
  float* vals = alloc_shared_array<float>("cmp_const_count_usm f32 NaN values alloc", n);
  if (vals == nullptr) {
    return;
  }

  vals[0] = 1.0f;
  vals[1] = nan;
  vals[2] = 2.0f;
  vals[3] = nan;
  double const_nan = static_cast<double>(nan);

  pgaccel_expr_usm_col col{vals, nullptr, PGACCEL_VAL_FLOAT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count_usm(
      col, n, PGACCEL_EXPR_OP_EQ, const_nan, &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count_usm f32 NaN const status", s);
  ASSERT_TRUE("cmp_const_count_usm f32 NaN const PG equality", true_count == 2);
  ASSERT_TRUE("cmp_const_count_usm f32 NaN const uncertain rows", uncertain_count == 0);

  pgaccel_expr_shared_free(vals);
}

static void test_cmp_const_count_usm_int64() {
  printf("--- test_cmp_const_count_usm_int64 ---\n");

  constexpr size_t n = 5;
  int64_t* vals = alloc_shared_array<int64_t>("cmp_const_count_usm int64 values alloc", n);
  uint8_t* nulls = alloc_shared_array<uint8_t>("cmp_const_count_usm int64 nulls alloc", n);
  if (vals == nullptr || nulls == nullptr) {
    pgaccel_expr_shared_free(vals);
    pgaccel_expr_shared_free(nulls);
    return;
  }

  vals[0] = 41;
  vals[1] = 42;
  vals[2] = 43;
  vals[3] = 44;
  vals[4] = 45;
  nulls[0] = 0;
  nulls[1] = 0;
  nulls[2] = 1;
  nulls[3] = 0;
  nulls[4] = 0;

  pgaccel_expr_usm_col col{vals, nulls, PGACCEL_VAL_INT64};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_count_usm(col, n, PGACCEL_EXPR_OP_GE, 43.0,
                                                               &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count_usm int64 integral const status", s);
  ASSERT_TRUE("cmp_const_count_usm int64 skips null rows", true_count == 2);
  ASSERT_TRUE("cmp_const_count_usm int64 integral const uncertain rows", uncertain_count == 0);

  true_count = 0;
  uncertain_count = 0;
  s = pgaccel_expr_template_cmp_const_count_usm(col, n, PGACCEL_EXPR_OP_LT, 42.5, &true_count,
                                                &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count_usm int64 fractional const status", s);
  ASSERT_TRUE("cmp_const_count_usm int64 fractional boundary", true_count == 2);
  ASSERT_TRUE("cmp_const_count_usm int64 fractional const uncertain rows", uncertain_count == 0);

  true_count = 99;
  uncertain_count = 99;
  s = pgaccel_expr_template_cmp_const_count_usm(col, n, PGACCEL_EXPR_OP_LT, 9223372036854775808.0,
                                                &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_count_usm int64 upper-bound fallback status", s);
  ASSERT_TRUE("cmp_const_count_usm int64 upper-bound fallback true rows", true_count == 4);
  ASSERT_TRUE("cmp_const_count_usm int64 upper-bound fallback uncertain rows",
              uncertain_count == 0);

  pgaccel_expr_shared_free(nulls);
  pgaccel_expr_shared_free(vals);
}

static void test_cmp_const_count_usm_rejects_bad_inputs() {
  printf("--- test_cmp_const_count_usm_rejects_bad_inputs ---\n");

  int32_t value = 1;
  pgaccel_expr_usm_col col{&value, nullptr, PGACCEL_VAL_INT32};
  size_t true_count = 123;
  size_t uncertain_count = 456;

  pgaccel_status s = pgaccel_expr_template_cmp_const_count_usm(col, 1, PGACCEL_EXPR_OP_GT, 0.0,
                                                               nullptr, &uncertain_count);
  ASSERT_TRUE("cmp_const_count_usm rejects null true_count", s == PGACCEL_ERROR);

  col.values = nullptr;
  true_count = 123;
  uncertain_count = 456;
  s = pgaccel_expr_template_cmp_const_count_usm(col, 1, PGACCEL_EXPR_OP_GT, 0.0, &true_count,
                                                &uncertain_count);
  ASSERT_TRUE("cmp_const_count_usm rejects null values with rows", s == PGACCEL_ERROR);
  ASSERT_TRUE("cmp_const_count_usm null values clears true count", true_count == 0);
  ASSERT_TRUE("cmp_const_count_usm null values clears uncertain count", uncertain_count == 0);
}

static void test_cmp_const_mask_usm_selection_and_count() {
  printf("--- test_cmp_const_mask_usm_selection_and_count ---\n");

  constexpr size_t n = 5;
  float* vals = alloc_shared_array<float>("cmp_const_mask_usm values alloc", n);
  uint8_t* nulls = alloc_shared_array<uint8_t>("cmp_const_mask_usm nulls alloc", n);
  uint8_t* selection = alloc_shared_array<uint8_t>("cmp_const_mask_usm selection alloc", n);
  if (vals == nullptr || nulls == nullptr || selection == nullptr) {
    pgaccel_expr_shared_free(selection);
    pgaccel_expr_shared_free(nulls);
    pgaccel_expr_shared_free(vals);
    return;
  }

  vals[0] = 1.0f;
  vals[1] = 8.0f;
  vals[2] = 7.0f;
  vals[3] = 9.0f;
  vals[4] = 4.0f;
  nulls[0] = 0;
  nulls[1] = 1;
  nulls[2] = 0;
  nulls[3] = 0;
  nulls[4] = 0;
  std::memset(selection, 9, n);

  pgaccel_expr_usm_col col{vals, nulls, PGACCEL_VAL_FLOAT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_cmp_const_mask_usm(
      col, n, PGACCEL_EXPR_OP_GT, 6.0, selection, &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_mask_usm status", s);
  ASSERT_TRUE("cmp_const_mask_usm true rows", true_count == 2);
  ASSERT_TRUE("cmp_const_mask_usm uncertain rows", uncertain_count == 0);
  ASSERT_TRUE("cmp_const_mask_usm row 0 false", selection[0] == 0);
  ASSERT_TRUE("cmp_const_mask_usm row 1 null false", selection[1] == 0);
  ASSERT_TRUE("cmp_const_mask_usm row 2 true", selection[2] == 1);
  ASSERT_TRUE("cmp_const_mask_usm row 3 true", selection[3] == 1);
  ASSERT_TRUE("cmp_const_mask_usm row 4 false", selection[4] == 0);

  pgaccel_expr_shared_free(selection);
  pgaccel_expr_shared_free(nulls);
  pgaccel_expr_shared_free(vals);
}

static void test_cmp_const_reduce_f32_usm_quarantine() {
  printf("--- test_cmp_const_reduce_f32_usm_quarantine ---\n");

  constexpr size_t n = 6;
  float* pred_vals = alloc_shared_array<float>("cmp_const_reduce_f32_usm pred values alloc", n);
  uint8_t* pred_nulls = alloc_shared_array<uint8_t>("cmp_const_reduce_f32_usm pred nulls alloc", n);
  float* value_vals = alloc_shared_array<float>("cmp_const_reduce_f32_usm value alloc", n);
  uint8_t* value_nulls =
      alloc_shared_array<uint8_t>("cmp_const_reduce_f32_usm value nulls alloc", n);
  if (pred_vals == nullptr || pred_nulls == nullptr || value_vals == nullptr ||
      value_nulls == nullptr) {
    pgaccel_expr_shared_free(value_nulls);
    pgaccel_expr_shared_free(value_vals);
    pgaccel_expr_shared_free(pred_nulls);
    pgaccel_expr_shared_free(pred_vals);
    return;
  }

  const float pred_init[n] = {1.0f, 7.0f, 8.0f, 9.0f, 10.0f, 2.0f};
  const float value_init[n] = {10.0f, 20.0f, 30.0f, 40.0f, 50.0f, 60.0f};
  for (size_t i = 0; i < n; ++i) {
    pred_vals[i] = pred_init[i];
    pred_nulls[i] = 0;
    value_vals[i] = value_init[i];
    value_nulls[i] = 0;
  }
  pred_nulls[2] = 1;
  value_nulls[3] = 1;

  pgaccel_expr_usm_col pred_col{pred_vals, pred_nulls, PGACCEL_VAL_FLOAT32};
  pgaccel_expr_usm_col value_col{value_vals, value_nulls, PGACCEL_VAL_FLOAT32};
  float out_sum = -1.0f;
  float out_min = -1.0f;
  float out_max = -1.0f;
  int64_t out_value_count = -1;
  size_t true_count = 99;
  size_t uncertain_count = 99;
  pgaccel_reset_gpu_exec_count();
  pgaccel_status s = pgaccel_expr_template_cmp_const_reduce_f32_usm(
      pred_col, PGACCEL_EXPR_OP_GT, 6.0, value_col, n, &out_sum, &out_min, &out_max,
      &out_value_count, &true_count, &uncertain_count);
  ASSERT_TRUE("cmp_const_reduce_f32_usm valid nonempty is unsupported", s == PGACCEL_UNSUPPORTED);
  ASSERT_TRUE("cmp_const_reduce_f32_usm preserves outputs",
              out_sum == -1.0f && out_min == -1.0f && out_max == -1.0f && out_value_count == -1 &&
                  true_count == 99 && uncertain_count == 99);
  ASSERT_TRUE("cmp_const_reduce_f32_usm launches no GPU work", pgaccel_gpu_exec_count() == 0);

  s = pgaccel_expr_template_cmp_const_reduce_f32_usm(
      pred_col, PGACCEL_EXPR_OP_GT, 6.0, value_col, 0, &out_sum, &out_min, &out_max,
      &out_value_count, &true_count, &uncertain_count);
  ASSERT_STATUS_OK("cmp_const_reduce_f32_usm empty status", s);
  ASSERT_TRUE("cmp_const_reduce_f32_usm empty identity",
              out_sum == 0.0f && out_min == 0.0f && out_max == 0.0f && out_value_count == 0 &&
                  true_count == 0 && uncertain_count == 0);
  ASSERT_TRUE("cmp_const_reduce_f32_usm empty launches no GPU work", pgaccel_gpu_exec_count() == 0);

  pgaccel_expr_shared_free(value_nulls);
  pgaccel_expr_shared_free(value_vals);
  pgaccel_expr_shared_free(pred_nulls);
  pgaccel_expr_shared_free(pred_vals);
}

// ---------------------------------------------------------------------------
// Test: cmp_const with no top-level null pointer array — all rows valid
// ---------------------------------------------------------------------------
static void test_cmp_const_without_col_nulls_array() {
  printf("--- test_cmp_const_without_col_nulls_array ---\n");

  std::vector<int64_t> vals = {1, 2, 3, 4};
  std::vector<uint8_t> nulls = {1, 1, 1, 1};
  OneColI64Batch b(std::move(vals), std::move(nulls));
  b.batch.col_nulls = nullptr;

  int8_t results[4] = {};
  pgaccel_status s = pgaccel_expr_template_cmp_const(&b.batch, 0, PGACCEL_EXPR_OP_GT, 0.0, results);
  ASSERT_STATUS_OK("cmp_const without col_nulls status", s);

  ASSERT_TRUE("row 0 treated as valid", results[0] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 1 treated as valid", results[1] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 2 treated as valid", results[2] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 3 treated as valid", results[3] == PGACCEL_EXPR_TRUE);
}

// ---------------------------------------------------------------------------
// Test: cmp_const with null per-column mask pointer — column is all-valid
// ---------------------------------------------------------------------------
static void test_cmp_const_with_null_col_entry() {
  printf("--- test_cmp_const_with_null_col_entry ---\n");

  std::vector<int64_t> vals = {1, 2, 3, 4};
  std::vector<uint8_t> nulls = {1, 1, 1, 1};
  OneColI64Batch b(std::move(vals), std::move(nulls));
  b.col_null_ptrs[0] = nullptr;

  int8_t results[4] = {};
  pgaccel_status s = pgaccel_expr_template_cmp_const(&b.batch, 0, PGACCEL_EXPR_OP_GT, 0.0, results);
  ASSERT_STATUS_OK("cmp_const with null col entry status", s);

  ASSERT_TRUE("row 0 treated as valid", results[0] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 1 treated as valid", results[1] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 2 treated as valid", results[2] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 3 treated as valid", results[3] == PGACCEL_EXPR_TRUE);
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
// Test: is_null with no null mask reports all rows as not null
// ---------------------------------------------------------------------------
static void test_is_null_without_null_mask_reports_all_not_null() {
  printf("--- test_is_null_without_null_mask_reports_all_not_null ---\n");

  std::vector<int64_t> vals = {1, 2, 3};
  std::vector<uint8_t> nulls = {1, 1, 1};
  OneColI64Batch b(std::move(vals), std::move(nulls));
  b.col_null_ptrs[0] = nullptr;

  {
    int8_t results[3] = {};
    pgaccel_status s = pgaccel_expr_template_is_null(&b.batch, 0, false, results);
    ASSERT_STATUS_OK("is_null without mask status", s);
    ASSERT_TRUE("IS NULL row 0 → FALSE", results[0] == PGACCEL_EXPR_FALSE);
    ASSERT_TRUE("IS NULL row 1 → FALSE", results[1] == PGACCEL_EXPR_FALSE);
    ASSERT_TRUE("IS NULL row 2 → FALSE", results[2] == PGACCEL_EXPR_FALSE);
  }

  {
    int8_t results[3] = {};
    pgaccel_status s = pgaccel_expr_template_is_null(&b.batch, 0, true, results);
    ASSERT_STATUS_OK("is_not_null without mask status", s);
    ASSERT_TRUE("IS NOT NULL row 0 → TRUE", results[0] == PGACCEL_EXPR_TRUE);
    ASSERT_TRUE("IS NOT NULL row 1 → TRUE", results[1] == PGACCEL_EXPR_TRUE);
    ASSERT_TRUE("IS NOT NULL row 2 → TRUE", results[2] == PGACCEL_EXPR_TRUE);
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

struct TwoColF32I32Batch {
  std::vector<float> data0;
  std::vector<int32_t> data1;
  std::vector<uint8_t> nulls0;
  std::vector<uint8_t> nulls1;
  void* col_data_ptrs[2];
  uint8_t* col_null_ptrs[2];
  pgaccel_val_tag col_types[2];
  pgaccel_batch batch;

  TwoColF32I32Batch(std::vector<float> v0, std::vector<int32_t> v1, std::vector<uint8_t> n0,
                    std::vector<uint8_t> n1)
      : data0(std::move(v0)), data1(std::move(v1)), nulls0(std::move(n0)), nulls1(std::move(n1)) {
    col_data_ptrs[0] = data0.data();
    col_data_ptrs[1] = data1.data();
    col_null_ptrs[0] = nulls0.data();
    col_null_ptrs[1] = nulls1.data();
    col_types[0] = PGACCEL_VAL_FLOAT32;
    col_types[1] = PGACCEL_VAL_INT32;
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

static void test_two_pred_and_count() {
  printf("--- test_two_pred_and_count ---\n");

  std::vector<int64_t> v0 = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 1, 2, 3};
  std::vector<int64_t> v1 = {2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 11, 15, 100};
  std::vector<uint8_t> n0(13, 0);
  std::vector<uint8_t> n1(13, 0);
  TwoColI64Batch b(std::move(v0), std::move(v1), std::move(n0), std::move(n1));

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_count(&b.batch, 0, PGACCEL_EXPR_OP_LT, 5.0,
                                                              1, PGACCEL_EXPR_OP_GT, 10.0,
                                                              &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count status", s);
  ASSERT_TRUE("two_pred_and_count true rows", true_count == 3);
  ASSERT_TRUE("two_pred_and_count uncertain rows", uncertain_count == 0);
}

static void test_two_pred_and_count_nulls() {
  printf("--- test_two_pred_and_count_nulls ---\n");

  std::vector<int64_t> v0 = {1, 2, 3, 4, 5};
  std::vector<int64_t> v1 = {11, 12, 13, 14, 15};
  std::vector<uint8_t> n0 = {0, 1, 0, 0, 0};
  std::vector<uint8_t> n1 = {0, 0, 0, 1, 0};
  TwoColI64Batch b(std::move(v0), std::move(v1), std::move(n0), std::move(n1));

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_count(&b.batch, 0, PGACCEL_EXPR_OP_LT, 10.0,
                                                              1, PGACCEL_EXPR_OP_GT, 5.0,
                                                              &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count nulls status", s);
  ASSERT_TRUE("two_pred_and_count skips either-side null rows", true_count == 3);
  ASSERT_TRUE("two_pred_and_count nulls uncertain rows", uncertain_count == 0);
}

static void test_two_pred_and_count_mixed_null_pointer_entries() {
  printf("--- test_two_pred_and_count_mixed_null_pointer_entries ---\n");

  std::vector<int64_t> v0 = {1, 2, 3, 4};
  std::vector<int64_t> v1 = {10, 10, 10, 10};
  std::vector<uint8_t> n0 = {1, 1, 1, 1};
  std::vector<uint8_t> n1 = {0, 1, 0, 1};
  TwoColI64Batch b(std::move(v0), std::move(v1), std::move(n0), std::move(n1));
  b.col_null_ptrs[0] = nullptr;

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_count(&b.batch, 0, PGACCEL_EXPR_OP_LT, 5.0,
                                                              1, PGACCEL_EXPR_OP_EQ, 10.0,
                                                              &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count mixed null pointer status", s);
  ASSERT_TRUE("two_pred_and_count mixed null pointer true rows", true_count == 2);
  ASSERT_TRUE("two_pred_and_count mixed null pointer uncertain rows", uncertain_count == 0);
}

static void test_two_pred_and_count_f32_i32_fast_path() {
  printf("--- test_two_pred_and_count_f32_i32_fast_path ---\n");

  std::vector<float> v0 = {0.0f, 1.0f, 2.0f, 3.0f, 4.0f, 5.0f};
  std::vector<int32_t> v1 = {10, 60, 40, 55, 70, 80};
  std::vector<uint8_t> n0(6, 0);
  std::vector<uint8_t> n1(6, 0);
  TwoColF32I32Batch b(std::move(v0), std::move(v1), std::move(n0), std::move(n1));

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_count(&b.batch, 0, PGACCEL_EXPR_OP_GT, 1.5,
                                                              1, PGACCEL_EXPR_OP_LT, 70.0,
                                                              &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count f32/i32 fast-path status", s);
  ASSERT_TRUE("two_pred_and_count f32/i32 true rows", true_count == 2);
  ASSERT_TRUE("two_pred_and_count f32/i32 uncertain rows", uncertain_count == 0);
}

static void test_two_pred_and_count_i32_non_integral_const_fallback() {
  printf("--- test_two_pred_and_count_i32_non_integral_const_fallback ---\n");

  std::vector<float> v0 = {2.0f, 2.0f, 2.0f, 2.0f};
  std::vector<int32_t> v1 = {68, 69, 70, 71};
  std::vector<uint8_t> n0(4, 0);
  std::vector<uint8_t> n1(4, 0);
  TwoColF32I32Batch b(std::move(v0), std::move(v1), std::move(n0), std::move(n1));

  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_count(&b.batch, 0, PGACCEL_EXPR_OP_GE, 2.0,
                                                              1, PGACCEL_EXPR_OP_LT, 69.5,
                                                              &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count i32 non-integral const fallback status", s);
  ASSERT_TRUE("two_pred_and_count i32 fallback preserves fractional boundary", true_count == 2);
  ASSERT_TRUE("two_pred_and_count i32 fallback uncertain rows", uncertain_count == 0);
}

static void test_two_pred_and_count_usm_mixed_null_pointer_entries() {
  printf("--- test_two_pred_and_count_usm_mixed_null_pointer_entries ---\n");

  constexpr size_t n = 4;
  int32_t* col1_vals = alloc_shared_array<int32_t>("two_pred_and_count_usm col1 values alloc", n);
  float* col2_vals = alloc_shared_array<float>("two_pred_and_count_usm col2 values alloc", n);
  uint8_t* col2_nulls = alloc_shared_array<uint8_t>("two_pred_and_count_usm col2 nulls alloc", n);
  if (col1_vals == nullptr || col2_vals == nullptr || col2_nulls == nullptr) {
    pgaccel_expr_shared_free(col1_vals);
    pgaccel_expr_shared_free(col2_vals);
    pgaccel_expr_shared_free(col2_nulls);
    return;
  }

  for (size_t i = 0; i < n; ++i) {
    col1_vals[i] = static_cast<int32_t>(i + 1);
    col2_vals[i] = 10.0f;
  }
  col2_nulls[0] = 0;
  col2_nulls[1] = 1;
  col2_nulls[2] = 0;
  col2_nulls[3] = 1;

  pgaccel_expr_usm_col col1{col1_vals, nullptr, PGACCEL_VAL_INT32};
  pgaccel_expr_usm_col col2{col2_vals, col2_nulls, PGACCEL_VAL_FLOAT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_count_usm(col1, PGACCEL_EXPR_OP_LT, 5.0,
                                                                  col2, PGACCEL_EXPR_OP_EQ, 10.0, n,
                                                                  &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count_usm mixed null pointer status", s);
  ASSERT_TRUE("two_pred_and_count_usm mixed null pointer true rows", true_count == 2);
  ASSERT_TRUE("two_pred_and_count_usm mixed null pointer uncertain rows", uncertain_count == 0);

  pgaccel_expr_shared_free(col2_nulls);
  pgaccel_expr_shared_free(col2_vals);
  pgaccel_expr_shared_free(col1_vals);
}

static void test_two_pred_and_count_usm_multigroup_all_valid() {
  printf("--- test_two_pred_and_count_usm_multigroup_all_valid ---\n");

  constexpr size_t n = 1025;
  int32_t* col1_vals =
      alloc_shared_array<int32_t>("two_pred_and_count_usm multigroup col1 values alloc", n);
  float* col2_vals =
      alloc_shared_array<float>("two_pred_and_count_usm multigroup col2 values alloc", n);
  if (col1_vals == nullptr || col2_vals == nullptr) {
    pgaccel_expr_shared_free(col1_vals);
    pgaccel_expr_shared_free(col2_vals);
    return;
  }

  for (size_t i = 0; i < n; ++i) {
    col1_vals[i] = static_cast<int32_t>(i);
    col2_vals[i] = static_cast<float>(i);
  }

  pgaccel_expr_usm_col col1{col1_vals, nullptr, PGACCEL_VAL_INT32};
  pgaccel_expr_usm_col col2{col2_vals, nullptr, PGACCEL_VAL_FLOAT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_count_usm(col1, PGACCEL_EXPR_OP_GT, 256.0,
                                                                  col2, PGACCEL_EXPR_OP_LT, 768.0,
                                                                  n, &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count_usm multigroup status", s);
  ASSERT_TRUE("two_pred_and_count_usm multigroup true rows", true_count == 511);
  ASSERT_TRUE("two_pred_and_count_usm multigroup uncertain rows", uncertain_count == 0);

  pgaccel_expr_shared_free(col2_vals);
  pgaccel_expr_shared_free(col1_vals);
}

static void test_two_pred_and_count_usm_strided_rows_and_nulls() {
  printf("--- test_two_pred_and_count_usm_strided_rows_and_nulls ---\n");

  constexpr size_t n = 1025;
  int32_t* col1_vals =
      alloc_shared_array<int32_t>("two_pred_and_count_usm stride col1 values alloc", n);
  int32_t* col2_vals =
      alloc_shared_array<int32_t>("two_pred_and_count_usm stride col2 values alloc", n);
  uint8_t* col2_nulls =
      alloc_shared_array<uint8_t>("two_pred_and_count_usm stride col2 nulls alloc", n);
  if (col1_vals == nullptr || col2_vals == nullptr || col2_nulls == nullptr) {
    pgaccel_expr_shared_free(col1_vals);
    pgaccel_expr_shared_free(col2_vals);
    pgaccel_expr_shared_free(col2_nulls);
    return;
  }

  for (size_t i = 0; i < n; ++i) {
    col1_vals[i] = static_cast<int32_t>(i);
    col2_vals[i] = 10;
    col2_nulls[i] = 0;
  }
  col2_nulls[300] = 1;
  col2_nulls[1024] = 1;

  pgaccel_expr_usm_col col1{col1_vals, nullptr, PGACCEL_VAL_INT32};
  pgaccel_expr_usm_col col2{col2_vals, col2_nulls, PGACCEL_VAL_INT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_count_usm(col1, PGACCEL_EXPR_OP_GE, 0.0,
                                                                  col2, PGACCEL_EXPR_OP_EQ, 10.0, n,
                                                                  &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count_usm stride status", s);
  ASSERT_TRUE("two_pred_and_count_usm stride visits late rows and nulls", true_count == 1023);
  ASSERT_TRUE("two_pred_and_count_usm stride uncertain rows", uncertain_count == 0);

  pgaccel_expr_shared_free(col2_nulls);
  pgaccel_expr_shared_free(col2_vals);
  pgaccel_expr_shared_free(col1_vals);
}

static void test_two_pred_and_count_usm_f32_nan_const() {
  printf("--- test_two_pred_and_count_usm_f32_nan_const ---\n");

  constexpr size_t n = 4;
  const float nan = std::numeric_limits<float>::quiet_NaN();
  float* col1_vals =
      alloc_shared_array<float>("two_pred_and_count_usm f32 NaN col1 values alloc", n);
  int32_t* col2_vals =
      alloc_shared_array<int32_t>("two_pred_and_count_usm f32 NaN col2 values alloc", n);
  if (col1_vals == nullptr || col2_vals == nullptr) {
    pgaccel_expr_shared_free(col1_vals);
    pgaccel_expr_shared_free(col2_vals);
    return;
  }

  col1_vals[0] = nan;
  col1_vals[1] = 1.0f;
  col1_vals[2] = nan;
  col1_vals[3] = 2.0f;
  col2_vals[0] = 1;
  col2_vals[1] = 1;
  col2_vals[2] = 2;
  col2_vals[3] = 2;

  pgaccel_expr_usm_col col1{col1_vals, nullptr, PGACCEL_VAL_FLOAT32};
  pgaccel_expr_usm_col col2{col2_vals, nullptr, PGACCEL_VAL_INT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_count_usm(
      col1, PGACCEL_EXPR_OP_EQ, static_cast<double>(nan), col2, PGACCEL_EXPR_OP_EQ, 1.0, n,
      &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count_usm f32 NaN const status", s);
  ASSERT_TRUE("two_pred_and_count_usm f32 NaN const PG equality", true_count == 1);
  ASSERT_TRUE("two_pred_and_count_usm f32 NaN const uncertain rows", uncertain_count == 0);

  pgaccel_expr_shared_free(col2_vals);
  pgaccel_expr_shared_free(col1_vals);
}

static void test_two_pred_and_count_usm_int64() {
  printf("--- test_two_pred_and_count_usm_int64 ---\n");

  constexpr size_t n = 5;
  int64_t* col1_vals =
      alloc_shared_array<int64_t>("two_pred_and_count_usm int64 col1 values alloc", n);
  int32_t* col2_vals =
      alloc_shared_array<int32_t>("two_pred_and_count_usm int64 col2 values alloc", n);
  uint8_t* col1_nulls =
      alloc_shared_array<uint8_t>("two_pred_and_count_usm int64 col1 nulls alloc", n);
  if (col1_vals == nullptr || col2_vals == nullptr || col1_nulls == nullptr) {
    pgaccel_expr_shared_free(col1_vals);
    pgaccel_expr_shared_free(col2_vals);
    pgaccel_expr_shared_free(col1_nulls);
    return;
  }

  col1_vals[0] = 10;
  col1_vals[1] = 20;
  col1_vals[2] = 30;
  col1_vals[3] = 40;
  col1_vals[4] = 50;
  col2_vals[0] = 1;
  col2_vals[1] = 1;
  col2_vals[2] = 1;
  col2_vals[3] = 2;
  col2_vals[4] = 1;
  col1_nulls[0] = 0;
  col1_nulls[1] = 1;
  col1_nulls[2] = 0;
  col1_nulls[3] = 0;
  col1_nulls[4] = 0;

  pgaccel_expr_usm_col col1{col1_vals, col1_nulls, PGACCEL_VAL_INT64};
  pgaccel_expr_usm_col col2{col2_vals, nullptr, PGACCEL_VAL_INT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;

  pgaccel_status s = pgaccel_expr_template_two_pred_and_count_usm(col1, PGACCEL_EXPR_OP_GE, 30.0,
                                                                  col2, PGACCEL_EXPR_OP_EQ, 1.0, n,
                                                                  &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count_usm int64 integral const status", s);
  ASSERT_TRUE("two_pred_and_count_usm int64 true rows", true_count == 2);
  ASSERT_TRUE("two_pred_and_count_usm int64 uncertain rows", uncertain_count == 0);

  true_count = 0;
  uncertain_count = 0;
  s = pgaccel_expr_template_two_pred_and_count_usm(col1, PGACCEL_EXPR_OP_LT, 40.5, col2,
                                                   PGACCEL_EXPR_OP_EQ, 1.0, n, &true_count,
                                                   &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_count_usm int64 fractional const status", s);
  ASSERT_TRUE("two_pred_and_count_usm int64 fractional boundary", true_count == 2);
  ASSERT_TRUE("two_pred_and_count_usm int64 fractional uncertain rows", uncertain_count == 0);

  pgaccel_expr_shared_free(col1_nulls);
  pgaccel_expr_shared_free(col2_vals);
  pgaccel_expr_shared_free(col1_vals);
}

static void test_two_pred_and_count_usm_rejects_bad_inputs() {
  printf("--- test_two_pred_and_count_usm_rejects_bad_inputs ---\n");

  int32_t col1_value = 1;
  int32_t col2_value = 2;
  pgaccel_expr_usm_col col1{&col1_value, nullptr, PGACCEL_VAL_INT32};
  pgaccel_expr_usm_col col2{&col2_value, nullptr, PGACCEL_VAL_INT32};
  size_t true_count = 123;
  size_t uncertain_count = 456;

  pgaccel_status s = pgaccel_expr_template_two_pred_and_count_usm(
      col1, PGACCEL_EXPR_OP_GT, 0.0, col2, PGACCEL_EXPR_OP_LT, 3.0, 1, nullptr, &uncertain_count);
  ASSERT_TRUE("two_pred_and_count_usm rejects null true_count", s == PGACCEL_ERROR);

  pgaccel_expr_usm_col null_col1{nullptr, nullptr, PGACCEL_VAL_INT32};
  true_count = 123;
  uncertain_count = 456;
  s = pgaccel_expr_template_two_pred_and_count_usm(null_col1, PGACCEL_EXPR_OP_GT, 0.0, col2,
                                                   PGACCEL_EXPR_OP_LT, 3.0, 1, &true_count,
                                                   &uncertain_count);
  ASSERT_TRUE("two_pred_and_count_usm rejects null col1 values", s == PGACCEL_ERROR);
  ASSERT_TRUE("two_pred_and_count_usm null col1 clears true count", true_count == 0);
  ASSERT_TRUE("two_pred_and_count_usm null col1 clears uncertain count", uncertain_count == 0);

  pgaccel_expr_usm_col null_col2{nullptr, nullptr, PGACCEL_VAL_INT32};
  true_count = 123;
  uncertain_count = 456;
  s = pgaccel_expr_template_two_pred_and_count_usm(col1, PGACCEL_EXPR_OP_GT, 0.0, null_col2,
                                                   PGACCEL_EXPR_OP_LT, 3.0, 1, &true_count,
                                                   &uncertain_count);
  ASSERT_TRUE("two_pred_and_count_usm rejects null col2 values", s == PGACCEL_ERROR);
  ASSERT_TRUE("two_pred_and_count_usm null col2 clears true count", true_count == 0);
  ASSERT_TRUE("two_pred_and_count_usm null col2 clears uncertain count", uncertain_count == 0);

  true_count = 123;
  uncertain_count = 456;
  s = pgaccel_expr_template_two_pred_and_count_usm(col1, 999, 0.0, col2, PGACCEL_EXPR_OP_LT, 3.0, 1,
                                                   &true_count, &uncertain_count);
  ASSERT_TRUE("two_pred_and_count_usm rejects bad op1", s == PGACCEL_UNSUPPORTED);
  ASSERT_TRUE("two_pred_and_count_usm bad op1 clears true count", true_count == 0);
  ASSERT_TRUE("two_pred_and_count_usm bad op1 clears uncertain count", uncertain_count == 0);

  true_count = 123;
  uncertain_count = 456;
  s = pgaccel_expr_template_two_pred_and_count_usm(col1, PGACCEL_EXPR_OP_GT, 0.0, col2, 999, 3.0, 1,
                                                   &true_count, &uncertain_count);
  ASSERT_TRUE("two_pred_and_count_usm rejects bad op2", s == PGACCEL_UNSUPPORTED);
  ASSERT_TRUE("two_pred_and_count_usm bad op2 clears true count", true_count == 0);
  ASSERT_TRUE("two_pred_and_count_usm bad op2 clears uncertain count", uncertain_count == 0);

  pgaccel_expr_usm_col bool_col{&col1_value, nullptr, PGACCEL_VAL_BOOL};
  true_count = 123;
  uncertain_count = 456;
  s = pgaccel_expr_template_two_pred_and_count_usm(bool_col, PGACCEL_EXPR_OP_GT, 0.0, col2,
                                                   PGACCEL_EXPR_OP_LT, 3.0, 1, &true_count,
                                                   &uncertain_count);
  ASSERT_TRUE("two_pred_and_count_usm rejects unsupported col1 type", s == PGACCEL_UNSUPPORTED);

  true_count = 123;
  uncertain_count = 456;
  s = pgaccel_expr_template_two_pred_and_count_usm(col1, PGACCEL_EXPR_OP_GT, 0.0, bool_col,
                                                   PGACCEL_EXPR_OP_LT, 3.0, 1, &true_count,
                                                   &uncertain_count);
  ASSERT_TRUE("two_pred_and_count_usm rejects unsupported col2 type", s == PGACCEL_UNSUPPORTED);
}

static void test_two_pred_and_mask_usm_selection_and_count() {
  printf("--- test_two_pred_and_mask_usm_selection_and_count ---\n");

  constexpr size_t n = 6;
  int32_t* col1_vals = alloc_shared_array<int32_t>("two_pred_and_mask_usm col1 values alloc", n);
  float* col2_vals = alloc_shared_array<float>("two_pred_and_mask_usm col2 values alloc", n);
  uint8_t* col2_nulls = alloc_shared_array<uint8_t>("two_pred_and_mask_usm col2 nulls alloc", n);
  uint8_t* selection = alloc_shared_array<uint8_t>("two_pred_and_mask_usm selection alloc", n);
  if (col1_vals == nullptr || col2_vals == nullptr || col2_nulls == nullptr ||
      selection == nullptr) {
    pgaccel_expr_shared_free(selection);
    pgaccel_expr_shared_free(col2_nulls);
    pgaccel_expr_shared_free(col2_vals);
    pgaccel_expr_shared_free(col1_vals);
    return;
  }

  for (size_t i = 0; i < n; ++i) {
    col1_vals[i] = static_cast<int32_t>(i);
    col2_vals[i] = static_cast<float>(10 - i);
    col2_nulls[i] = 0;
  }
  col2_nulls[4] = 1;
  std::memset(selection, 9, n);

  pgaccel_expr_usm_col col1{col1_vals, nullptr, PGACCEL_VAL_INT32};
  pgaccel_expr_usm_col col2{col2_vals, col2_nulls, PGACCEL_VAL_FLOAT32};
  size_t true_count = 0;
  size_t uncertain_count = 0;
  pgaccel_status s = pgaccel_expr_template_two_pred_and_mask_usm(
      col1, PGACCEL_EXPR_OP_GE, 2.0, col2, PGACCEL_EXPR_OP_LT, 8.0, n, selection, &true_count,
      &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_mask_usm status", s);
  ASSERT_TRUE("two_pred_and_mask_usm true rows", true_count == 2);
  ASSERT_TRUE("two_pred_and_mask_usm uncertain rows", uncertain_count == 0);
  ASSERT_TRUE("two_pred_and_mask_usm row 0 false", selection[0] == 0);
  ASSERT_TRUE("two_pred_and_mask_usm row 1 false", selection[1] == 0);
  ASSERT_TRUE("two_pred_and_mask_usm row 2 false", selection[2] == 0);
  ASSERT_TRUE("two_pred_and_mask_usm row 3 true", selection[3] == 1);
  ASSERT_TRUE("two_pred_and_mask_usm row 4 null false", selection[4] == 0);
  ASSERT_TRUE("two_pred_and_mask_usm row 5 true", selection[5] == 1);

  pgaccel_expr_shared_free(selection);
  pgaccel_expr_shared_free(col2_nulls);
  pgaccel_expr_shared_free(col2_vals);
  pgaccel_expr_shared_free(col1_vals);
}

static void test_two_pred_and_reduce_f32_usm_quarantine() {
  printf("--- test_two_pred_and_reduce_f32_usm_quarantine ---\n");

  constexpr size_t n = 7;
  int32_t* col1_vals =
      alloc_shared_array<int32_t>("two_pred_and_reduce_f32_usm col1 values alloc", n);
  float* col2_vals = alloc_shared_array<float>("two_pred_and_reduce_f32_usm col2 values alloc", n);
  uint8_t* col2_nulls =
      alloc_shared_array<uint8_t>("two_pred_and_reduce_f32_usm col2 nulls alloc", n);
  float* value_vals =
      alloc_shared_array<float>("two_pred_and_reduce_f32_usm value values alloc", n);
  uint8_t* value_nulls =
      alloc_shared_array<uint8_t>("two_pred_and_reduce_f32_usm value nulls alloc", n);
  if (col1_vals == nullptr || col2_vals == nullptr || col2_nulls == nullptr ||
      value_vals == nullptr || value_nulls == nullptr) {
    pgaccel_expr_shared_free(value_nulls);
    pgaccel_expr_shared_free(value_vals);
    pgaccel_expr_shared_free(col2_nulls);
    pgaccel_expr_shared_free(col2_vals);
    pgaccel_expr_shared_free(col1_vals);
    return;
  }

  for (size_t i = 0; i < n; ++i) {
    col1_vals[i] = static_cast<int32_t>(i);
    col2_vals[i] = static_cast<float>(10 - i);
    col2_nulls[i] = 0;
    value_vals[i] = static_cast<float>(i + 1);
    value_nulls[i] = 0;
  }
  col2_nulls[4] = 1;
  value_nulls[5] = 1;

  pgaccel_expr_usm_col col1{col1_vals, nullptr, PGACCEL_VAL_INT32};
  pgaccel_expr_usm_col col2{col2_vals, col2_nulls, PGACCEL_VAL_FLOAT32};
  pgaccel_expr_usm_col value_col{value_vals, value_nulls, PGACCEL_VAL_FLOAT32};
  float out_sum = -1.0f;
  float out_min = -1.0f;
  float out_max = -1.0f;
  int64_t out_value_count = -1;
  size_t true_count = 99;
  size_t uncertain_count = 99;
  pgaccel_reset_gpu_exec_count();
  pgaccel_status s = pgaccel_expr_template_two_pred_and_reduce_f32_usm(
      col1, PGACCEL_EXPR_OP_GE, 2.0, col2, PGACCEL_EXPR_OP_LT, 8.0, value_col, n, &out_sum,
      &out_min, &out_max, &out_value_count, &true_count, &uncertain_count);
  ASSERT_TRUE("two_pred_and_reduce_f32_usm valid nonempty is unsupported",
              s == PGACCEL_UNSUPPORTED);
  ASSERT_TRUE("two_pred_and_reduce_f32_usm preserves outputs",
              out_sum == -1.0f && out_min == -1.0f && out_max == -1.0f && out_value_count == -1 &&
                  true_count == 99 && uncertain_count == 99);
  ASSERT_TRUE("two_pred_and_reduce_f32_usm launches no GPU work", pgaccel_gpu_exec_count() == 0);

  s = pgaccel_expr_template_two_pred_and_reduce_f32_usm(
      col1, PGACCEL_EXPR_OP_GE, 2.0, col2, PGACCEL_EXPR_OP_LT, 8.0, value_col, 0, &out_sum,
      &out_min, &out_max, &out_value_count, &true_count, &uncertain_count);
  ASSERT_STATUS_OK("two_pred_and_reduce_f32_usm empty status", s);
  ASSERT_TRUE("two_pred_and_reduce_f32_usm empty identity",
              out_sum == 0.0f && out_min == 0.0f && out_max == 0.0f && out_value_count == 0 &&
                  true_count == 0 && uncertain_count == 0);
  ASSERT_TRUE("two_pred_and_reduce_f32_usm empty launches no GPU work",
              pgaccel_gpu_exec_count() == 0);

  pgaccel_expr_shared_free(value_nulls);
  pgaccel_expr_shared_free(value_vals);
  pgaccel_expr_shared_free(col2_nulls);
  pgaccel_expr_shared_free(col2_vals);
  pgaccel_expr_shared_free(col1_vals);
}

// ---------------------------------------------------------------------------
// Test: two_pred_and with one all-valid pointer and one explicit mask
// ---------------------------------------------------------------------------
static void test_two_pred_and_mixed_null_pointer_entries() {
  printf("--- test_two_pred_and_mixed_null_pointer_entries ---\n");

  std::vector<int64_t> v0 = {1, 2, 3, 4};
  std::vector<int64_t> v1 = {10, 10, 10, 10};
  std::vector<uint8_t> n0 = {1, 1, 1, 1};
  std::vector<uint8_t> n1 = {0, 1, 0, 1};
  TwoColI64Batch b(std::move(v0), std::move(v1), std::move(n0), std::move(n1));
  b.col_null_ptrs[0] = nullptr;

  int8_t results[4] = {};
  pgaccel_status s = pgaccel_expr_template_two_pred_and(&b.batch, 0, PGACCEL_EXPR_OP_LT, 5.0, 1,
                                                        PGACCEL_EXPR_OP_EQ, 10.0, results);
  ASSERT_STATUS_OK("two_pred_and mixed null pointer status", s);

  ASSERT_TRUE("row 0 valid on both columns → TRUE", results[0] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 1 explicit col1 NULL → FALSE", results[1] == PGACCEL_EXPR_FALSE);
  ASSERT_TRUE("row 2 valid on both columns → TRUE", results[2] == PGACCEL_EXPR_TRUE);
  ASSERT_TRUE("row 3 explicit col1 NULL → FALSE", results[3] == PGACCEL_EXPR_FALSE);
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

static void test_fused_multi_reduce_quarantine() {
  printf("--- test_fused_multi_reduce_quarantine ---\n");

  constexpr size_t n = 8192;
  std::vector<float> values(n, 1.0f);
  pgaccel_reduce_col cols[2] = {
      {PGACCEL_FUSED_SUM, values.data()},
      {PGACCEL_FUSED_COUNT, nullptr},
  };
  float results[2] = {-11.0f, -22.0f};
  size_t pass_count = 99;

  pgaccel_reset_gpu_exec_count();
  pgaccel_status s = pgaccel_fused_filter_multi_reduce_f32(nullptr, n, PGACCEL_CMP_ALWAYS_TRUE,
                                                           0.0f, cols, 2, results, &pass_count);
  ASSERT_TRUE("fused multi-reduce valid nonempty is unsupported", s == PGACCEL_UNSUPPORTED);
  ASSERT_TRUE("fused multi-reduce preserves outputs",
              results[0] == -11.0f && results[1] == -22.0f && pass_count == 99);
  ASSERT_TRUE("fused multi-reduce launches no GPU work", pgaccel_gpu_exec_count() == 0);

  s = pgaccel_fused_filter_multi_reduce_f32(nullptr, 0, PGACCEL_CMP_ALWAYS_TRUE, 0.0f, cols, 2,
                                            results, &pass_count);
  ASSERT_TRUE("fused multi-reduce empty is unsupported", s == PGACCEL_UNSUPPORTED);
  ASSERT_TRUE("fused multi-reduce empty preserves outputs",
              results[0] == -11.0f && results[1] == -22.0f && pass_count == 99);
  ASSERT_TRUE("fused multi-reduce empty launches no GPU work", pgaccel_gpu_exec_count() == 0);
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

  test_expr_shared_alloc_free();
  test_empty_batch();
  test_cmp_const_rejects_unsupported_opcode();
  test_cmp_const();
  test_cmp_const_nulls();
  test_cmp_const_count();
  test_cmp_const_count_nulls();
  test_cmp_const_count_with_null_col_entry();
  test_cmp_const_count_nan_semantics();
  test_cmp_const_count_f32_fast_path();
  test_cmp_const_count_i32_non_integral_const_fallback();
  test_cmp_const_count_usm();
  test_cmp_const_count_usm_multigroup_all_valid();
  test_cmp_const_count_usm_device_finalizer_many_groups();
  test_cmp_const_count_usm_strided_rows_and_nulls();
  test_cmp_const_count_usm_f32_nan_const();
  test_cmp_const_count_usm_int64();
  test_cmp_const_count_usm_rejects_bad_inputs();
  test_cmp_const_mask_usm_selection_and_count();
  test_cmp_const_reduce_f32_usm_quarantine();
  test_cmp_const_without_col_nulls_array();
  test_cmp_const_with_null_col_entry();
  test_between();
  test_in_list();
  test_in_list_too_large();
  test_is_null();
  test_is_null_without_null_mask_reports_all_not_null();
  test_two_pred_and();
  test_two_pred_and_nulls();
  test_two_pred_and_count();
  test_two_pred_and_count_nulls();
  test_two_pred_and_count_mixed_null_pointer_entries();
  test_two_pred_and_count_f32_i32_fast_path();
  test_two_pred_and_count_i32_non_integral_const_fallback();
  test_two_pred_and_count_usm_mixed_null_pointer_entries();
  test_two_pred_and_count_usm_multigroup_all_valid();
  test_two_pred_and_count_usm_strided_rows_and_nulls();
  test_two_pred_and_count_usm_f32_nan_const();
  test_two_pred_and_count_usm_int64();
  test_two_pred_and_count_usm_rejects_bad_inputs();
  test_two_pred_and_mask_usm_selection_and_count();
  test_two_pred_and_reduce_f32_usm_quarantine();
  test_two_pred_and_mixed_null_pointer_entries();
  test_two_pred_and_rejects_unsupported_opcode();
  test_fused_multi_reduce_quarantine();

  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}
