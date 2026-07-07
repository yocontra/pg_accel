// expr_templates.cpp — Pre-compiled template kernels for common WHERE patterns.
//
// SYCL parallel_for kernels for the 5 template predicates that cover the
// vast majority of real WHERE clauses without going through the bytecode
// interpreter. Each template stages its referenced column to a contiguous
// shared-memory `double` buffer + null mask, then dispatches one
// `parallel_for` over rows.
//
// Templates:
//   1. col <cmp> const                    (pgaccel_expr_template_cmp_const)
//   2. col BETWEEN lo AND hi (inclusive)  (pgaccel_expr_template_between)
//   3. col IN (v0, ..., vN)               (pgaccel_expr_template_in_list)
//   4. col IS NULL / IS NOT NULL          (pgaccel_expr_template_is_null)
//   5. col1 <cmp1> const1 AND col2 <cmp2> const2  (..._two_pred_and)
//
// Per CLAUDE.md rule #11/#12 (GPU-only, SYCL-only) — the previous host
// for-loop implementation is replaced with `sycl::parallel_for`. Compare
// semantics match PG's NaN-aware ordering (NaN sorts greatest, NaN=NaN is
// true). The host-side `read_col_f64` staging step preserves the original
// type-cast matrix (int32 / int64 / float32 / float64 / bool / date /
// timestamp) so kernel sees only `double`.

#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <type_traits>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"


namespace {

// PG-compatible NaN-aware comparison opcodes encoded as small integers
// the device kernel can dispatch on. Matches `pgaccel_expr_opcode` values
// used by `get_cmp` below; kept local because the device kernel reads
// these as plain integers, not enum values.
constexpr uint16_t OP_LT = PGACCEL_EXPR_OP_LT;
constexpr uint16_t OP_LE = PGACCEL_EXPR_OP_LE;
constexpr uint16_t OP_GT = PGACCEL_EXPR_OP_GT;
constexpr uint16_t OP_GE = PGACCEL_EXPR_OP_GE;
constexpr uint16_t OP_EQ = PGACCEL_EXPR_OP_EQ;
constexpr uint16_t OP_NE = PGACCEL_EXPR_OP_NE;
constexpr size_t COUNT_WG_SIZE = 256;
constexpr size_t COUNT_ROWS_PER_ITEM = 4;
constexpr size_t COUNT_GROUP_ROWS = COUNT_WG_SIZE * COUNT_ROWS_PER_ITEM;

// Validates the opcode is one of the comparison ops we support. Returns
// `true` if the kernel will recognise it.
bool is_supported_cmp(uint16_t opcode) {
  return opcode == OP_LT || opcode == OP_LE || opcode == OP_GT || opcode == OP_GE ||
         opcode == OP_EQ || opcode == OP_NE;
}

size_t count_num_groups(size_t row_count) {
  return row_count / COUNT_GROUP_ROWS + ((row_count % COUNT_GROUP_ROWS) != 0);
}

inline bool pg_is_nan(double v) {
  const uint64_t bits = sycl::bit_cast<uint64_t>(v);
  return (bits & 0x7ff0000000000000ULL) == 0x7ff0000000000000ULL &&
         (bits & 0x000fffffffffffffULL) != 0;
}

inline bool pg_is_nan_f32(float v) {
  const uint32_t bits = sycl::bit_cast<uint32_t>(v);
  return (bits & 0x7f800000U) == 0x7f800000U && (bits & 0x007fffffU) != 0;
}

// Device-callable PG-NaN-aware comparison. Inlined into kernel body so
// the SYCL emitter sees a single function path. NaN semantics match the
// host helpers from the previous implementation:
//   - LT:  NaN < anything = false; anything < NaN = true (NaN sorts highest)
//   - LE:  NaN <= NaN = true;  NaN <= other = false;  other <= NaN = true
//   - EQ:  NaN == NaN = true;  NaN == other = false
inline bool pg_cmp(uint16_t op, double a, double b) {
  const bool a_nan = pg_is_nan(a);
  const bool b_nan = pg_is_nan(b);
  switch (op) {
    case OP_LT:
      if (a_nan)
        return false;
      if (b_nan)
        return true;
      return a < b;
    case OP_LE:
      if (a_nan && b_nan)
        return true;
      if (a_nan)
        return false;
      if (b_nan)
        return true;
      return a <= b;
    case OP_GT:
      if (b_nan)
        return false;
      if (a_nan)
        return true;
      return a > b;
    case OP_GE:
      if (a_nan && b_nan)
        return true;
      if (b_nan)
        return false;
      if (a_nan)
        return true;
      return a >= b;
    case OP_EQ:
      if (a_nan && b_nan)
        return true;
      if (a_nan || b_nan)
        return false;
      return a == b;
    case OP_NE:
      if (a_nan && b_nan)
        return false;
      if (a_nan || b_nan)
        return true;
      return a != b;
    default:
      return false;
  }
}

inline bool pg_cmp_f32(uint16_t op, float a, float b) {
  const bool a_nan = pg_is_nan_f32(a);
  const bool b_nan = pg_is_nan_f32(b);
  switch (op) {
    case OP_LT:
      if (a_nan)
        return false;
      if (b_nan)
        return true;
      return a < b;
    case OP_LE:
      if (a_nan && b_nan)
        return true;
      if (a_nan)
        return false;
      if (b_nan)
        return true;
      return a <= b;
    case OP_GT:
      if (b_nan)
        return false;
      if (a_nan)
        return true;
      return a > b;
    case OP_GE:
      if (a_nan && b_nan)
        return true;
      if (b_nan)
        return false;
      if (a_nan)
        return true;
      return a >= b;
    case OP_EQ:
      if (a_nan && b_nan)
        return true;
      if (a_nan || b_nan)
        return false;
      return a == b;
    case OP_NE:
      if (a_nan && b_nan)
        return false;
      if (a_nan || b_nan)
        return true;
      return a != b;
    default:
      return false;
  }
}

template <typename T>
inline bool pg_cmp_integral(uint16_t op, T a, T b) {
  switch (op) {
    case OP_LT:
      return a < b;
    case OP_LE:
      return a <= b;
    case OP_GT:
      return a > b;
    case OP_GE:
      return a >= b;
    case OP_EQ:
      return a == b;
    case OP_NE:
      return a != b;
    default:
      return false;
  }
}

template <typename T>
inline bool pg_cmp_typed(uint16_t op, T a, T b) {
  if constexpr (std::is_same_v<T, float>) {
    return pg_cmp_f32(op, a, b);
  } else if constexpr (std::is_same_v<T, double>) {
    return pg_cmp(op, a, b);
  } else {
    return pg_cmp_integral(op, a, b);
  }
}

bool double_is_finite(double value) {
  uint64_t bits = 0;
  std::memcpy(&bits, &value, sizeof(bits));
  return (bits & 0x7ff0000000000000ULL) != 0x7ff0000000000000ULL;
}

bool host_bits_is_nan_f64(uint64_t bits) {
  volatile uint64_t raw = bits;
  const uint64_t exponent = raw & 0x7ff0000000000000ULL;
  const uint64_t fraction = raw & 0x000fffffffffffffULL;
  return exponent == 0x7ff0000000000000ULL && fraction != 0;
}

bool host_bits_is_nan_f32(uint32_t bits) {
  volatile uint32_t raw = bits;
  const uint32_t exponent = raw & 0x7f800000U;
  const uint32_t fraction = raw & 0x007fffffU;
  return exponent == 0x7f800000U && fraction != 0;
}

bool host_is_nan_f64(double value) {
  uint64_t bits = 0;
  std::memcpy(&bits, &value, sizeof(bits));
  return host_bits_is_nan_f64(bits);
}

bool host_is_nan_f32(float value) {
  uint32_t bits = 0;
  std::memcpy(&bits, &value, sizeof(bits));
  return host_bits_is_nan_f32(bits);
}

bool const_to_i32(double value, int32_t* out) {
  if (out == nullptr || !double_is_finite(value))
    return false;
  if (value < static_cast<double>(std::numeric_limits<int32_t>::min()) ||
      value > static_cast<double>(std::numeric_limits<int32_t>::max()))
    return false;
  const double truncated = std::trunc(value);
  if (truncated != value)
    return false;
  *out = static_cast<int32_t>(value);
  return true;
}

bool const_to_i64(double value, int64_t* out) {
  if (out == nullptr || !double_is_finite(value))
    return false;
  constexpr double kMinInt64 = -9223372036854775808.0;
  constexpr double kMaxInt64Exclusive = 9223372036854775808.0;
  if (value < kMinInt64 || value >= kMaxInt64Exclusive)
    return false;
  const double truncated = std::trunc(value);
  if (truncated != value)
    return false;
  *out = static_cast<int64_t>(value);
  return true;
}

bool const_to_f32_exact(double value, float* out) {
  if (out == nullptr)
    return false;
  uint64_t bits = 0;
  std::memcpy(&bits, &value, sizeof(bits));
  const float narrowed = static_cast<float>(value);
  if (host_bits_is_nan_f64(bits) || static_cast<double>(narrowed) == value) {
    *out = narrowed;
    return true;
  }
  return false;
}

pgaccel_val_tag batch_col_tag(const pgaccel_batch* batch, uint32_t col_idx) {
  if (batch == nullptr || batch->col_types == nullptr || col_idx >= batch->num_cols ||
      batch->col_data == nullptr || batch->col_data[col_idx] == nullptr)
    return PGACCEL_VAL_NULL;
  return batch->col_types[col_idx];
}

const uint8_t* batch_col_null_mask(const pgaccel_batch* batch, uint32_t col_idx) {
  if (batch == nullptr || batch->col_nulls == nullptr || col_idx >= batch->num_cols)
    return nullptr;
  return batch->col_nulls[col_idx];
}

// Host-side column stage: reads the typed column and writes an f64 view
// + null mask to caller-supplied buffers. Mirrors the original
// `read_col_f64` cell-by-cell, but produces contiguous output suitable
// for a single SYCL parallel_for. Returns the count of nulls (for stats).
size_t stage_col_f64(const pgaccel_batch* batch, size_t col_idx, double* out_col,
                     uint8_t* out_null) {
  const size_t n = batch->num_rows;
  size_t nulls = 0;

  if (col_idx >= batch->num_cols || batch->col_data[col_idx] == nullptr) {
    // Column missing: every row is null.
    std::memset(out_null, 1, n);
    std::memset(out_col, 0, n * sizeof(double));
    return n;
  }

  const void* src = batch->col_data[col_idx];
  const uint8_t* src_nulls = (batch->col_nulls != nullptr) ? batch->col_nulls[col_idx] : nullptr;
  const pgaccel_val_tag tag = batch->col_types[col_idx];

  for (size_t row = 0; row < n; ++row) {
    if (src_nulls != nullptr && src_nulls[row]) {
      out_null[row] = 1;
      out_col[row] = 0.0;
      ++nulls;
      continue;
    }
    out_null[row] = 0;
    switch (tag) {
      case PGACCEL_VAL_INT32:
        out_col[row] = static_cast<double>(static_cast<const int32_t*>(src)[row]);
        break;
      case PGACCEL_VAL_INT64:
        out_col[row] = static_cast<double>(static_cast<const int64_t*>(src)[row]);
        break;
      case PGACCEL_VAL_FLOAT32:
        out_col[row] = static_cast<double>(static_cast<const float*>(src)[row]);
        break;
      case PGACCEL_VAL_FLOAT64:
        out_col[row] = static_cast<const double*>(src)[row];
        break;
      case PGACCEL_VAL_BOOL:
        out_col[row] = static_cast<const bool*>(src)[row] ? 1.0 : 0.0;
        break;
      case PGACCEL_VAL_DATE:
        out_col[row] = static_cast<double>(static_cast<const int32_t*>(src)[row]);
        break;
      case PGACCEL_VAL_TIMESTAMP:
        out_col[row] = static_cast<double>(static_cast<const int64_t*>(src)[row]);
        break;
      default:
        // Unsupported tag: treat as null.
        out_null[row] = 1;
        out_col[row] = 0.0;
        ++nulls;
        break;
    }
  }
  return nulls;
}

template <typename T>
size_t stage_col_exact(const pgaccel_batch* batch, size_t col_idx, pgaccel_val_tag expected,
                       T* out_col, uint8_t* out_null) {
  const size_t n = batch->num_rows;
  if (col_idx >= batch->num_cols || batch->col_data == nullptr ||
      batch->col_data[col_idx] == nullptr || batch->col_types == nullptr ||
      batch->col_types[col_idx] != expected) {
    if (out_null != nullptr)
      std::memset(out_null, 1, n);
    std::memset(out_col, 0, n * sizeof(T));
    return n;
  }

  const T* src = static_cast<const T*>(batch->col_data[col_idx]);
  const uint8_t* src_nulls = (batch->col_nulls != nullptr) ? batch->col_nulls[col_idx] : nullptr;
  if (src_nulls == nullptr) {
    std::memcpy(out_col, src, n * sizeof(T));
    if (out_null != nullptr)
      std::memset(out_null, 0, n);
    return 0;
  }

  if (out_null == nullptr)
    return n;

  size_t nulls = 0;
  for (size_t row = 0; row < n; ++row) {
    if (src_nulls[row]) {
      out_col[row] = T{};
      out_null[row] = 1;
      ++nulls;
    } else {
      out_col[row] = src[row];
      out_null[row] = 0;
    }
  }
  return nulls;
}

// Device-only: column missing detection for IS NULL / IS NOT NULL.
// Stages just the null mask without converting values, since the
// template doesn't read column data.
void stage_null_mask(const pgaccel_batch* batch, size_t col_idx, size_t n, uint8_t* out_null) {
  if (col_idx >= batch->num_cols || batch->col_data[col_idx] == nullptr) {
    std::memset(out_null, 1, n);
    return;
  }
  const uint8_t* src_nulls = (batch->col_nulls != nullptr) ? batch->col_nulls[col_idx] : nullptr;
  if (src_nulls == nullptr) {
    std::memset(out_null, 0, n);
  } else {
    std::memcpy(out_null, src_nulls, n);
  }
}

// Common cleanup helper for the 1-column templates.
struct OneColScratch {
  double* d_col;
  uint8_t* d_null;
  int8_t* d_res;
  sycl::queue* q;
  ~OneColScratch() {
    if (d_col)
      sycl::free(d_col, *q);
    if (d_null)
      sycl::free(d_null, *q);
    if (d_res)
      sycl::free(d_res, *q);
  }
};

struct CountOneColScratch {
  double* d_col;
  uint8_t* d_null;
  sycl::queue* q;
  ~CountOneColScratch() {
    if (d_col)
      sycl::free(d_col, *q);
    if (d_null)
      sycl::free(d_null, *q);
  }
};

struct CountTwoColScratch {
  double* d_col1;
  uint8_t* d_null1;
  double* d_col2;
  uint8_t* d_null2;
  sycl::queue* q;
  ~CountTwoColScratch() {
    if (d_col1)
      sycl::free(d_col1, *q);
    if (d_null1)
      sycl::free(d_null1, *q);
    if (d_col2)
      sycl::free(d_col2, *q);
    if (d_null2)
      sycl::free(d_null2, *q);
  }
};

template <typename T>
struct CountOneColTypedScratch {
  T* d_col;
  uint8_t* d_null;
  sycl::queue* q;
  ~CountOneColTypedScratch() {
    if (d_col)
      sycl::free(d_col, *q);
    if (d_null)
      sycl::free(d_null, *q);
  }
};

template <typename T1, typename T2>
struct CountTwoColTypedScratch {
  T1* d_col1;
  uint8_t* d_null1;
  T2* d_col2;
  uint8_t* d_null2;
  sycl::queue* q;
  ~CountTwoColTypedScratch() {
    if (d_col1)
      sycl::free(d_col1, *q);
    if (d_null1)
      sycl::free(d_null1, *q);
    if (d_col2)
      sycl::free(d_col2, *q);
    if (d_null2)
      sycl::free(d_null2, *q);
  }
};

struct CountPartialsScratch {
  uint32_t* partials;
  sycl::queue* q;
  ~CountPartialsScratch() {
    if (partials)
      sycl::free(partials, *q);
  }
};

size_t sum_count_partials(const uint32_t* partials, size_t num_groups) {
  size_t total = 0;
  for (size_t i = 0; i < num_groups; ++i)
    total += partials[i];
  return total;
}

template <typename T>
struct KernelConst {
  T value;
};

template <>
struct KernelConst<float> {
  uint32_t bits;
};

template <>
struct KernelConst<double> {
  uint64_t bits;
};

template <typename T>
KernelConst<T> make_kernel_const(T value) {
  return KernelConst<T>{value};
}

template <>
KernelConst<float> make_kernel_const<float>(float value) {
  uint32_t bits = 0;
  std::memcpy(&bits, &value, sizeof(bits));
  return KernelConst<float>{bits};
}

template <>
KernelConst<double> make_kernel_const<double>(double value) {
  uint64_t bits = 0;
  std::memcpy(&bits, &value, sizeof(bits));
  return KernelConst<double>{bits};
}

template <typename T>
inline T load_kernel_const(KernelConst<T> value) {
  if constexpr (std::is_same_v<T, float>) {
    return sycl::bit_cast<float>(value.bits);
  } else if constexpr (std::is_same_v<T, double>) {
    return sycl::bit_cast<double>(value.bits);
  } else {
    return value.value;
  }
}

template <typename T>
inline double value_as_double(T value) {
  return static_cast<double>(value);
}

inline bool pg_cmp_f32_nan_const(uint16_t op, float value) {
  const bool value_nan = pg_is_nan_f32(value);
  switch (op) {
    case OP_LT:
      return !value_nan;
    case OP_LE:
      return true;
    case OP_GT:
      return false;
    case OP_GE:
      return value_nan;
    case OP_EQ:
      return value_nan;
    case OP_NE:
      return !value_nan;
    default:
      return false;
  }
}

pgaccel_status launch_cmp_const_count_f32_nan_const(const float* d_col, const uint8_t* d_null,
                                                    size_t n, uint16_t cmp_opcode,
                                                    size_t* true_count, const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op = cmp_opcode;
  const size_t count = n;
  const bool nulls_present = d_null != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
                      [=](sycl::nd_item<1> item) {
                        const size_t lid = item.get_local_id(0);
                        const size_t group_id = item.get_group(0);
                        const size_t group_start = group_id * COUNT_GROUP_ROWS;
                        uint32_t local_count = 0;
                        for (size_t offset = 0; offset < COUNT_GROUP_ROWS;
                             offset += COUNT_WG_SIZE) {
                          const size_t row = group_start + offset + lid;
                          if (row < count && (!nulls_present || !d_null[row]) &&
                              pg_cmp_f32_nan_const(op, d_col[row])) {
                            ++local_count;
                          }
                        }
                        local_mem[lid] = local_count;
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
                          if (lid < stride) {
                            local_mem[lid] += local_mem[lid + stride];
                          }
                          item.barrier(sycl::access::fence_space::local_space);
                        }

                        if (lid == 0) {
                          partials[group_id] = local_mem[0];
                        }
                      });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T>
pgaccel_status launch_cmp_const_count_usm_typed(const T* d_col, const uint8_t* d_null, size_t n,
                                                uint16_t cmp_opcode, T const_val,
                                                size_t* true_count, const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col == nullptr)
    return PGACCEL_ERROR;
  if constexpr (std::is_same_v<T, float>) {
    uint32_t const_bits = 0;
    std::memcpy(&const_bits, &const_val, sizeof(const_bits));
    if (host_bits_is_nan_f32(const_bits)) {
      return launch_cmp_const_count_f32_nan_const(d_col, d_null, n, cmp_opcode, true_count,
                                                  kernel_name);
    }
  }

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op = cmp_opcode;
  const KernelConst<T> cv = make_kernel_const<T>(const_val);
  const size_t count = n;
  const bool nulls_present = d_null != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
                      [=](sycl::nd_item<1> item) {
                        const size_t lid = item.get_local_id(0);
                        const size_t group_id = item.get_group(0);
                        const size_t group_start = group_id * COUNT_GROUP_ROWS;
                        uint32_t local_count = 0;
                        for (size_t offset = 0; offset < COUNT_GROUP_ROWS;
                             offset += COUNT_WG_SIZE) {
                          const size_t row = group_start + offset + lid;
                          if (row < count && (!nulls_present || !d_null[row]) &&
                              pg_cmp_typed(op, d_col[row], load_kernel_const<T>(cv))) {
                            ++local_count;
                          }
                        }
                        local_mem[lid] = local_count;
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
                          if (lid < stride) {
                            local_mem[lid] += local_mem[lid + stride];
                          }
                          item.barrier(sycl::access::fence_space::local_space);
                        }

                        if (lid == 0) {
                          partials[group_id] = local_mem[0];
                        }
                      });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T1, typename T2>
pgaccel_status launch_two_pred_and_count_usm_typed(const T1* d_col1, const uint8_t* d_null1,
                                                   const T2* d_col2, const uint8_t* d_null2,
                                                   size_t n, uint16_t cmp1_opcode, T1 const1_val,
                                                   uint16_t cmp2_opcode, T2 const2_val,
                                                   size_t* true_count, const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col1 == nullptr || d_col2 == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op1 = cmp1_opcode;
  const uint16_t op2 = cmp2_opcode;
  const KernelConst<T1> cv1 = make_kernel_const<T1>(const1_val);
  const KernelConst<T2> cv2 = make_kernel_const<T2>(const2_val);
  const size_t count = n;
  const bool nulls1_present = d_null1 != nullptr;
  const bool nulls2_present = d_null2 != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
                      [=](sycl::nd_item<1> item) {
                        const size_t lid = item.get_local_id(0);
                        const size_t group_id = item.get_group(0);
                        const size_t group_start = group_id * COUNT_GROUP_ROWS;
                        uint32_t local_count = 0;
                        for (size_t offset = 0; offset < COUNT_GROUP_ROWS;
                             offset += COUNT_WG_SIZE) {
                          const size_t row = group_start + offset + lid;
                          if (row < count && (!nulls1_present || !d_null1[row]) &&
                              pg_cmp_typed(op1, d_col1[row], load_kernel_const<T1>(cv1)) &&
                              (!nulls2_present || !d_null2[row]) &&
                              pg_cmp_typed(op2, d_col2[row], load_kernel_const<T2>(cv2))) {
                            ++local_count;
                          }
                        }
                        local_mem[lid] = local_count;
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
                          if (lid < stride) {
                            local_mem[lid] += local_mem[lid + stride];
                          }
                          item.barrier(sycl::access::fence_space::local_space);
                        }

                        if (lid == 0) {
                          partials[group_id] = local_mem[0];
                        }
                      });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T>
pgaccel_status launch_cmp_const_count_usm_as_double(const T* d_col, const uint8_t* d_null, size_t n,
                                                    uint16_t cmp_opcode, double const_val,
                                                    size_t* true_count, const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op = cmp_opcode;
  const KernelConst<double> cv = make_kernel_const<double>(const_val);
  const size_t count = n;
  const bool nulls_present = d_null != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(
           sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
           [=](sycl::nd_item<1> item) {
             const size_t lid = item.get_local_id(0);
             const size_t group_id = item.get_group(0);
             const size_t group_start = group_id * COUNT_GROUP_ROWS;
             uint32_t local_count = 0;
             for (size_t offset = 0; offset < COUNT_GROUP_ROWS; offset += COUNT_WG_SIZE) {
               const size_t row = group_start + offset + lid;
               if (row < count && (!nulls_present || !d_null[row]) &&
                   pg_cmp(op, value_as_double(d_col[row]), load_kernel_const<double>(cv))) {
                 ++local_count;
               }
             }
             local_mem[lid] = local_count;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
               if (lid < stride) {
                 local_mem[lid] += local_mem[lid + stride];
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (lid == 0) {
               partials[group_id] = local_mem[0];
             }
           });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T1, typename T2>
pgaccel_status
launch_two_pred_and_count_usm_as_double(const T1* d_col1, const uint8_t* d_null1, const T2* d_col2,
                                        const uint8_t* d_null2, size_t n, uint16_t cmp1_opcode,
                                        double const1_val, uint16_t cmp2_opcode, double const2_val,
                                        size_t* true_count, const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col1 == nullptr || d_col2 == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op1 = cmp1_opcode;
  const uint16_t op2 = cmp2_opcode;
  const KernelConst<double> cv1 = make_kernel_const<double>(const1_val);
  const KernelConst<double> cv2 = make_kernel_const<double>(const2_val);
  const size_t count = n;
  const bool nulls1_present = d_null1 != nullptr;
  const bool nulls2_present = d_null2 != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(
           sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
           [=](sycl::nd_item<1> item) {
             const size_t lid = item.get_local_id(0);
             const size_t group_id = item.get_group(0);
             const size_t group_start = group_id * COUNT_GROUP_ROWS;
             uint32_t local_count = 0;
             for (size_t offset = 0; offset < COUNT_GROUP_ROWS; offset += COUNT_WG_SIZE) {
               const size_t row = group_start + offset + lid;
               if (row < count && (!nulls1_present || !d_null1[row]) &&
                   pg_cmp(op1, value_as_double(d_col1[row]), load_kernel_const<double>(cv1)) &&
                   (!nulls2_present || !d_null2[row]) &&
                   pg_cmp(op2, value_as_double(d_col2[row]), load_kernel_const<double>(cv2))) {
                 ++local_count;
               }
             }
             local_mem[lid] = local_count;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
               if (lid < stride) {
                 local_mem[lid] += local_mem[lid + stride];
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (lid == 0) {
               partials[group_id] = local_mem[0];
             }
           });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

pgaccel_status launch_cmp_const_mask_f32_nan_const(const float* d_col, const uint8_t* d_null,
                                                   size_t n, uint16_t cmp_opcode,
                                                   uint8_t* selection, size_t* true_count,
                                                   const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col == nullptr || selection == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op = cmp_opcode;
  const size_t count = n;
  const bool nulls_present = d_null != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
                      [=](sycl::nd_item<1> item) {
                        const size_t lid = item.get_local_id(0);
                        const size_t group_id = item.get_group(0);
                        const size_t group_start = group_id * COUNT_GROUP_ROWS;
                        uint32_t local_count = 0;
                        for (size_t offset = 0; offset < COUNT_GROUP_ROWS;
                             offset += COUNT_WG_SIZE) {
                          const size_t row = group_start + offset + lid;
                          if (row < count) {
                            const bool pass = (!nulls_present || !d_null[row]) &&
                                              pg_cmp_f32_nan_const(op, d_col[row]);
                            selection[row] = pass ? 1 : 0;
                            local_count += pass ? 1 : 0;
                          }
                        }
                        local_mem[lid] = local_count;
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
                          if (lid < stride) {
                            local_mem[lid] += local_mem[lid + stride];
                          }
                          item.barrier(sycl::access::fence_space::local_space);
                        }

                        if (lid == 0) {
                          partials[group_id] = local_mem[0];
                        }
                      });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T>
pgaccel_status launch_cmp_const_mask_usm_typed(const T* d_col, const uint8_t* d_null, size_t n,
                                               uint16_t cmp_opcode, T const_val, uint8_t* selection,
                                               size_t* true_count, const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col == nullptr || selection == nullptr)
    return PGACCEL_ERROR;
  if constexpr (std::is_same_v<T, float>) {
    uint32_t const_bits = 0;
    std::memcpy(&const_bits, &const_val, sizeof(const_bits));
    if (host_bits_is_nan_f32(const_bits)) {
      return launch_cmp_const_mask_f32_nan_const(d_col, d_null, n, cmp_opcode, selection,
                                                 true_count, kernel_name);
    }
  }

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op = cmp_opcode;
  const KernelConst<T> cv = make_kernel_const<T>(const_val);
  const size_t count = n;
  const bool nulls_present = d_null != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(
           sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
           [=](sycl::nd_item<1> item) {
             const size_t lid = item.get_local_id(0);
             const size_t group_id = item.get_group(0);
             const size_t group_start = group_id * COUNT_GROUP_ROWS;
             uint32_t local_count = 0;
             for (size_t offset = 0; offset < COUNT_GROUP_ROWS; offset += COUNT_WG_SIZE) {
               const size_t row = group_start + offset + lid;
               if (row < count) {
                 const bool pass = (!nulls_present || !d_null[row]) &&
                                   pg_cmp_typed(op, d_col[row], load_kernel_const<T>(cv));
                 selection[row] = pass ? 1 : 0;
                 local_count += pass ? 1 : 0;
               }
             }
             local_mem[lid] = local_count;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
               if (lid < stride) {
                 local_mem[lid] += local_mem[lid + stride];
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (lid == 0) {
               partials[group_id] = local_mem[0];
             }
           });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T1, typename T2>
pgaccel_status launch_two_pred_and_mask_usm_typed(const T1* d_col1, const uint8_t* d_null1,
                                                  const T2* d_col2, const uint8_t* d_null2,
                                                  size_t n, uint16_t cmp1_opcode, T1 const1_val,
                                                  uint16_t cmp2_opcode, T2 const2_val,
                                                  uint8_t* selection, size_t* true_count,
                                                  const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col1 == nullptr || d_col2 == nullptr || selection == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op1 = cmp1_opcode;
  const uint16_t op2 = cmp2_opcode;
  const KernelConst<T1> cv1 = make_kernel_const<T1>(const1_val);
  const KernelConst<T2> cv2 = make_kernel_const<T2>(const2_val);
  const size_t count = n;
  const bool nulls1_present = d_null1 != nullptr;
  const bool nulls2_present = d_null2 != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(
           sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
           [=](sycl::nd_item<1> item) {
             const size_t lid = item.get_local_id(0);
             const size_t group_id = item.get_group(0);
             const size_t group_start = group_id * COUNT_GROUP_ROWS;
             uint32_t local_count = 0;
             for (size_t offset = 0; offset < COUNT_GROUP_ROWS; offset += COUNT_WG_SIZE) {
               const size_t row = group_start + offset + lid;
               if (row < count) {
                 const bool pass = (!nulls1_present || !d_null1[row]) &&
                                   pg_cmp_typed(op1, d_col1[row], load_kernel_const<T1>(cv1)) &&
                                   (!nulls2_present || !d_null2[row]) &&
                                   pg_cmp_typed(op2, d_col2[row], load_kernel_const<T2>(cv2));
                 selection[row] = pass ? 1 : 0;
                 local_count += pass ? 1 : 0;
               }
             }
             local_mem[lid] = local_count;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
               if (lid < stride) {
                 local_mem[lid] += local_mem[lid + stride];
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (lid == 0) {
               partials[group_id] = local_mem[0];
             }
           });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T>
pgaccel_status launch_cmp_const_mask_usm_as_double(const T* d_col, const uint8_t* d_null, size_t n,
                                                   uint16_t cmp_opcode, double const_val,
                                                   uint8_t* selection, size_t* true_count,
                                                   const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col == nullptr || selection == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op = cmp_opcode;
  const KernelConst<double> cv = make_kernel_const<double>(const_val);
  const size_t count = n;
  const bool nulls_present = d_null != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
                      [=](sycl::nd_item<1> item) {
                        const size_t lid = item.get_local_id(0);
                        const size_t group_id = item.get_group(0);
                        const size_t group_start = group_id * COUNT_GROUP_ROWS;
                        uint32_t local_count = 0;
                        for (size_t offset = 0; offset < COUNT_GROUP_ROWS;
                             offset += COUNT_WG_SIZE) {
                          const size_t row = group_start + offset + lid;
                          if (row < count) {
                            const bool pass = (!nulls_present || !d_null[row]) &&
                                              pg_cmp(op, value_as_double(d_col[row]),
                                                     load_kernel_const<double>(cv));
                            selection[row] = pass ? 1 : 0;
                            local_count += pass ? 1 : 0;
                          }
                        }
                        local_mem[lid] = local_count;
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
                          if (lid < stride) {
                            local_mem[lid] += local_mem[lid + stride];
                          }
                          item.barrier(sycl::access::fence_space::local_space);
                        }

                        if (lid == 0) {
                          partials[group_id] = local_mem[0];
                        }
                      });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T1, typename T2>
pgaccel_status launch_two_pred_and_mask_usm_as_double(const T1* d_col1, const uint8_t* d_null1,
                                                      const T2* d_col2, const uint8_t* d_null2,
                                                      size_t n, uint16_t cmp1_opcode,
                                                      double const1_val, uint16_t cmp2_opcode,
                                                      double const2_val, uint8_t* selection,
                                                      size_t* true_count, const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col1 == nullptr || d_col2 == nullptr || selection == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  CountPartialsScratch s{
      sycl::malloc_shared<uint32_t>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  uint32_t* partials = s.partials;
  const uint16_t op1 = cmp1_opcode;
  const uint16_t op2 = cmp2_opcode;
  const KernelConst<double> cv1 = make_kernel_const<double>(const1_val);
  const KernelConst<double> cv2 = make_kernel_const<double>(const2_val);
  const size_t count = n;
  const bool nulls1_present = d_null1 != nullptr;
  const bool nulls2_present = d_null2 != nullptr;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(
           sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
           [=](sycl::nd_item<1> item) {
             const size_t lid = item.get_local_id(0);
             const size_t group_id = item.get_group(0);
             const size_t group_start = group_id * COUNT_GROUP_ROWS;
             uint32_t local_count = 0;
             for (size_t offset = 0; offset < COUNT_GROUP_ROWS; offset += COUNT_WG_SIZE) {
               const size_t row = group_start + offset + lid;
               if (row < count) {
                 const bool pass =
                     (!nulls1_present || !d_null1[row]) &&
                     pg_cmp(op1, value_as_double(d_col1[row]), load_kernel_const<double>(cv1)) &&
                     (!nulls2_present || !d_null2[row]) &&
                     pg_cmp(op2, value_as_double(d_col2[row]), load_kernel_const<double>(cv2));
                 selection[row] = pass ? 1 : 0;
                 local_count += pass ? 1 : 0;
               }
             }
             local_mem[lid] = local_count;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
               if (lid < stride) {
                 local_mem[lid] += local_mem[lid + stride];
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (lid == 0) {
               partials[group_id] = local_mem[0];
             }
           });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  *true_count = sum_count_partials(s.partials, num_groups);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T>
pgaccel_status launch_cmp_const_count_typed(const pgaccel_batch* batch, uint32_t col_idx,
                                            uint16_t cmp_opcode, T const_val,
                                            pgaccel_val_tag expected_tag, size_t* true_count,
                                            const char* kernel_name) {
  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const bool has_nulls = batch_col_null_mask(batch, col_idx) != nullptr;
  CountOneColTypedScratch<T> s{
      sycl::malloc_shared<T>(n, *q),
      has_nulls ? sycl::malloc_shared<uint8_t>(n, *q) : nullptr,
      q,
  };
  if (s.d_col == nullptr || (has_nulls && s.d_null == nullptr))
    return PGACCEL_OOM;

  stage_col_exact(batch, col_idx, expected_tag, s.d_col, s.d_null);
  return launch_cmp_const_count_usm_typed<T>(s.d_col, s.d_null, n, cmp_opcode, const_val,
                                             true_count, kernel_name);
}

template <typename T1, typename T2>
pgaccel_status
launch_two_pred_and_count_typed(const pgaccel_batch* batch, uint32_t col1_idx, uint16_t cmp1_opcode,
                                T1 const1_val, pgaccel_val_tag tag1, uint32_t col2_idx,
                                uint16_t cmp2_opcode, T2 const2_val, pgaccel_val_tag tag2,
                                size_t* true_count, const char* kernel_name) {
  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const bool has_nulls1 = batch_col_null_mask(batch, col1_idx) != nullptr;
  const bool has_nulls2 = batch_col_null_mask(batch, col2_idx) != nullptr;
  CountTwoColTypedScratch<T1, T2> s{
      sycl::malloc_shared<T1>(n, *q),
      has_nulls1 ? sycl::malloc_shared<uint8_t>(n, *q) : nullptr,
      sycl::malloc_shared<T2>(n, *q),
      has_nulls2 ? sycl::malloc_shared<uint8_t>(n, *q) : nullptr,
      q,
  };
  if (s.d_col1 == nullptr || (has_nulls1 && s.d_null1 == nullptr) || s.d_col2 == nullptr ||
      (has_nulls2 && s.d_null2 == nullptr))
    return PGACCEL_OOM;

  stage_col_exact(batch, col1_idx, tag1, s.d_col1, s.d_null1);
  stage_col_exact(batch, col2_idx, tag2, s.d_col2, s.d_null2);
  return launch_two_pred_and_count_usm_typed<T1, T2>(s.d_col1, s.d_null1, s.d_col2, s.d_null2, n,
                                                     cmp1_opcode, const1_val, cmp2_opcode,
                                                     const2_val, true_count, kernel_name);
}

template <typename T>
bool const_to_typed(double value, T* out) {
  if constexpr (std::is_same_v<T, float>) {
    return const_to_f32_exact(value, out);
  } else if constexpr (std::is_same_v<T, double>) {
    if (out == nullptr)
      return false;
    *out = value;
    return true;
  } else if constexpr (std::is_same_v<T, int32_t>) {
    return const_to_i32(value, out);
  } else if constexpr (std::is_same_v<T, int64_t>) {
    return const_to_i64(value, out);
  } else {
    return false;
  }
}

struct F32ReducePartial {
  float sum;
  float min;
  float max;
  int64_t count;
  uint64_t selected;
};

struct F32ReducePartialsScratch {
  F32ReducePartial* partials;
  sycl::queue* q;
  ~F32ReducePartialsScratch() {
    if (partials)
      sycl::free(partials, *q);
  }
};

inline F32ReducePartial f32_reduce_identity() {
  return F32ReducePartial{0.0f, 0.0f, 0.0f, 0, 0};
}

inline F32ReducePartial f32_reduce_one(float value, uint64_t selected) {
  return F32ReducePartial{value, value, value, 1, selected};
}

inline bool f32_reduce_pg_less(float a, float b) {
  const bool a_nan = pg_is_nan_f32(a);
  const bool b_nan = pg_is_nan_f32(b);
  if (a_nan)
    return false;
  if (b_nan)
    return true;
  return a < b;
}

inline bool f32_reduce_pg_greater(float a, float b) {
  const bool a_nan = pg_is_nan_f32(a);
  const bool b_nan = pg_is_nan_f32(b);
  if (b_nan)
    return false;
  if (a_nan)
    return true;
  return a > b;
}

inline F32ReducePartial f32_reduce_combine(F32ReducePartial a, F32ReducePartial b) {
  F32ReducePartial r;
  r.selected = a.selected + b.selected;
  if (a.count == 0) {
    r.sum = b.sum;
    r.min = b.min;
    r.max = b.max;
    r.count = b.count;
    return r;
  }
  if (b.count == 0) {
    r.sum = a.sum;
    r.min = a.min;
    r.max = a.max;
    r.count = a.count;
    return r;
  }
  r.sum = a.sum + b.sum;
  r.min = f32_reduce_pg_less(b.min, a.min) ? b.min : a.min;
  r.max = f32_reduce_pg_greater(b.max, a.max) ? b.max : a.max;
  r.count = a.count + b.count;
  return r;
}

inline void finish_f32_reduce(F32ReducePartial final, float* out_sum, float* out_min,
                              float* out_max, int64_t* out_count, size_t* true_count) {
  if (final.count == 0) {
    *out_sum = 0.0f;
    *out_min = 0.0f;
    *out_max = 0.0f;
  } else {
    *out_sum = final.sum;
    *out_min = final.min;
    *out_max = final.max;
  }
  *out_count = final.count;
  *true_count = static_cast<size_t>(final.selected);
}

template <typename T>
pgaccel_status launch_cmp_const_reduce_f32_usm_typed(
    const T* d_pred, const uint8_t* d_pred_null, const float* d_value, const uint8_t* d_value_null,
    size_t n, uint16_t cmp_opcode, T const_val, float* out_sum, float* out_min, float* out_max,
    int64_t* out_count, size_t* true_count, const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_pred == nullptr || d_value == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  F32ReducePartialsScratch s{
      sycl::malloc_shared<F32ReducePartial>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  F32ReducePartial* partials = s.partials;
  const uint16_t op = cmp_opcode;
  const KernelConst<T> cv = make_kernel_const<T>(const_val);
  const size_t count = n;
  const bool pred_nulls_present = d_pred_null != nullptr;
  const bool value_nulls_present = d_value_null != nullptr;
  const F32ReducePartial identity = f32_reduce_identity();

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<F32ReducePartial, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(
           sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
           [=](sycl::nd_item<1> item) {
             const size_t lid = item.get_local_id(0);
             const size_t group_id = item.get_group(0);
             const size_t group_start = group_id * COUNT_GROUP_ROWS;
             F32ReducePartial local = identity;
             for (size_t offset = 0; offset < COUNT_GROUP_ROWS; offset += COUNT_WG_SIZE) {
               const size_t row = group_start + offset + lid;
               if (row < count) {
                 const bool pass = (!pred_nulls_present || !d_pred_null[row]) &&
                                   pg_cmp_typed(op, d_pred[row], load_kernel_const<T>(cv));
                 if (pass) {
                   const bool consume = !value_nulls_present || d_value_null[row] == 0;
                   F32ReducePartial row_partial{0.0f, 0.0f, 0.0f, 0, 1};
                   if (consume) {
                     row_partial = f32_reduce_one(d_value[row], 1);
                   }
                   local = f32_reduce_combine(local, row_partial);
                 }
               }
             }
             local_mem[lid] = local;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
               if (lid < stride) {
                 local_mem[lid] = f32_reduce_combine(local_mem[lid], local_mem[lid + stride]);
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (lid == 0) {
               partials[group_id] = local_mem[0];
             }
           });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  F32ReducePartial final = identity;
  for (size_t i = 0; i < num_groups; ++i) {
    final = f32_reduce_combine(final, s.partials[i]);
  }
  finish_f32_reduce(final, out_sum, out_min, out_max, out_count, true_count);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T1, typename T2>
pgaccel_status launch_two_pred_and_reduce_f32_usm_typed(
    const T1* d_col1, const uint8_t* d_null1, const T2* d_col2, const uint8_t* d_null2,
    const float* d_value, const uint8_t* d_value_null, size_t n, uint16_t cmp1_opcode,
    T1 const1_val, uint16_t cmp2_opcode, T2 const2_val, float* out_sum, float* out_min,
    float* out_max, int64_t* out_count, size_t* true_count, const char* kernel_name) {
  if (n == 0)
    return PGACCEL_OK;
  if (d_col1 == nullptr || d_col2 == nullptr || d_value == nullptr)
    return PGACCEL_ERROR;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t num_groups = count_num_groups(n);
  F32ReducePartialsScratch s{
      sycl::malloc_shared<F32ReducePartial>(num_groups, *q),
      q,
  };
  if (s.partials == nullptr)
    return PGACCEL_OOM;

  F32ReducePartial* partials = s.partials;
  const uint16_t op1 = cmp1_opcode;
  const uint16_t op2 = cmp2_opcode;
  const KernelConst<T1> cv1 = make_kernel_const<T1>(const1_val);
  const KernelConst<T2> cv2 = make_kernel_const<T2>(const2_val);
  const size_t count = n;
  const bool nulls1_present = d_null1 != nullptr;
  const bool nulls2_present = d_null2 != nullptr;
  const bool value_nulls_present = d_value_null != nullptr;
  const F32ReducePartial identity = f32_reduce_identity();

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<F32ReducePartial, 1> local_mem(COUNT_WG_SIZE, h);
       h.parallel_for(
           sycl::nd_range<1>(num_groups * COUNT_WG_SIZE, COUNT_WG_SIZE),
           [=](sycl::nd_item<1> item) {
             const size_t lid = item.get_local_id(0);
             const size_t group_id = item.get_group(0);
             const size_t group_start = group_id * COUNT_GROUP_ROWS;
             F32ReducePartial local = identity;
             for (size_t offset = 0; offset < COUNT_GROUP_ROWS; offset += COUNT_WG_SIZE) {
               const size_t row = group_start + offset + lid;
               if (row < count) {
                 const bool pass = (!nulls1_present || !d_null1[row]) &&
                                   pg_cmp_typed(op1, d_col1[row], load_kernel_const<T1>(cv1)) &&
                                   (!nulls2_present || !d_null2[row]) &&
                                   pg_cmp_typed(op2, d_col2[row], load_kernel_const<T2>(cv2));
                 if (pass) {
                   const bool consume = !value_nulls_present || d_value_null[row] == 0;
                   F32ReducePartial row_partial{0.0f, 0.0f, 0.0f, 0, 1};
                   if (consume) {
                     row_partial = f32_reduce_one(d_value[row], 1);
                   }
                   local = f32_reduce_combine(local, row_partial);
                 }
               }
             }
             local_mem[lid] = local;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = COUNT_WG_SIZE / 2; stride > 0; stride >>= 1) {
               if (lid < stride) {
                 local_mem[lid] = f32_reduce_combine(local_mem[lid], local_mem[lid + stride]);
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (lid == 0) {
               partials[group_id] = local_mem[0];
             }
           });
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s failed: %s\n", kernel_name, e.what());
    return PGACCEL_ERROR;
  }

  F32ReducePartial final = identity;
  for (size_t i = 0; i < num_groups; ++i) {
    final = f32_reduce_combine(final, s.partials[i]);
  }
  finish_f32_reduce(final, out_sum, out_min, out_max, out_count, true_count);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

template <typename T>
pgaccel_status dispatch_cmp_const_reduce_f32_usm(pgaccel_expr_usm_col pred_col, uint16_t cmp_opcode,
                                                 double const_val, pgaccel_expr_usm_col value_col,
                                                 size_t row_count, float* out_sum, float* out_min,
                                                 float* out_max, int64_t* out_count,
                                                 size_t* true_count) {
  const T* pred_values = static_cast<const T*>(pred_col.values);
  const float* value_values = static_cast<const float*>(value_col.values);
  T typed_const{};
  if (!const_to_typed<T>(const_val, &typed_const))
    return PGACCEL_UNSUPPORTED;
  return launch_cmp_const_reduce_f32_usm_typed<T>(
      pred_values, pred_col.nulls, value_values, value_col.nulls, row_count, cmp_opcode,
      typed_const, out_sum, out_min, out_max, out_count, true_count,
      "expr_template_cmp_const_reduce_f32_usm");
}

template <typename T1, typename T2>
pgaccel_status dispatch_two_pred_and_reduce_f32_usm_pair(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, pgaccel_expr_usm_col value_col, size_t row_count,
    float* out_sum, float* out_min, float* out_max, int64_t* out_count, size_t* true_count) {
  const T1* values1 = static_cast<const T1*>(col1.values);
  const T2* values2 = static_cast<const T2*>(col2.values);
  const float* value_values = static_cast<const float*>(value_col.values);
  T1 typed_const1{};
  T2 typed_const2{};
  if (!const_to_typed<T1>(const1_val, &typed_const1) ||
      !const_to_typed<T2>(const2_val, &typed_const2))
    return PGACCEL_UNSUPPORTED;
  return launch_two_pred_and_reduce_f32_usm_typed<T1, T2>(
      values1, col1.nulls, values2, col2.nulls, value_values, value_col.nulls, row_count,
      cmp1_opcode, typed_const1, cmp2_opcode, typed_const2, out_sum, out_min, out_max, out_count,
      true_count, "expr_template_two_pred_and_reduce_f32_usm");
}

template <typename T1>
pgaccel_status dispatch_two_pred_and_reduce_f32_usm_col2(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, pgaccel_expr_usm_col value_col, size_t row_count,
    float* out_sum, float* out_min, float* out_max, int64_t* out_count, size_t* true_count) {
  switch (col2.type) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return dispatch_two_pred_and_reduce_f32_usm_pair<T1, int32_t>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, value_col, row_count,
          out_sum, out_min, out_max, out_count, true_count);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return dispatch_two_pred_and_reduce_f32_usm_pair<T1, int64_t>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, value_col, row_count,
          out_sum, out_min, out_max, out_count, true_count);
    case PGACCEL_VAL_FLOAT32:
      return dispatch_two_pred_and_reduce_f32_usm_pair<T1, float>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, value_col, row_count,
          out_sum, out_min, out_max, out_count, true_count);
    case PGACCEL_VAL_FLOAT64:
      return dispatch_two_pred_and_reduce_f32_usm_pair<T1, double>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, value_col, row_count,
          out_sum, out_min, out_max, out_count, true_count);
    default:
      return PGACCEL_UNSUPPORTED;
  }
}

pgaccel_status dispatch_two_pred_and_reduce_f32_usm(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, pgaccel_expr_usm_col value_col, size_t row_count,
    float* out_sum, float* out_min, float* out_max, int64_t* out_count, size_t* true_count) {
  switch (col1.type) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return dispatch_two_pred_and_reduce_f32_usm_col2<int32_t>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, value_col, row_count,
          out_sum, out_min, out_max, out_count, true_count);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return dispatch_two_pred_and_reduce_f32_usm_col2<int64_t>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, value_col, row_count,
          out_sum, out_min, out_max, out_count, true_count);
    case PGACCEL_VAL_FLOAT32:
      return dispatch_two_pred_and_reduce_f32_usm_col2<float>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, value_col, row_count,
          out_sum, out_min, out_max, out_count, true_count);
    case PGACCEL_VAL_FLOAT64:
      return dispatch_two_pred_and_reduce_f32_usm_col2<double>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, value_col, row_count,
          out_sum, out_min, out_max, out_count, true_count);
    default:
      return PGACCEL_UNSUPPORTED;
  }
}

template <typename T>
pgaccel_status dispatch_cmp_const_count_usm(pgaccel_expr_usm_col col, size_t row_count,
                                            uint16_t cmp_opcode, double const_val,
                                            size_t* true_count) {
  const T* values = static_cast<const T*>(col.values);
  if constexpr (std::is_same_v<T, float>) {
    uint64_t const_bits = 0;
    std::memcpy(&const_bits, &const_val, sizeof(const_bits));
    if (host_bits_is_nan_f64(const_bits)) {
      return launch_cmp_const_count_f32_nan_const(values, col.nulls, row_count, cmp_opcode,
                                                  true_count, "expr_template_cmp_const_count_usm");
    }
  }
  T typed_const{};
  if (const_to_typed<T>(const_val, &typed_const)) {
    return launch_cmp_const_count_usm_typed<T>(values, col.nulls, row_count, cmp_opcode,
                                               typed_const, true_count,
                                               "expr_template_cmp_const_count_usm");
  }
  return launch_cmp_const_count_usm_as_double<T>(values, col.nulls, row_count, cmp_opcode,
                                                 const_val, true_count,
                                                 "expr_template_cmp_const_count_usm");
}

template <typename T>
pgaccel_status dispatch_cmp_const_mask_usm(pgaccel_expr_usm_col col, size_t row_count,
                                           uint16_t cmp_opcode, double const_val,
                                           uint8_t* selection, size_t* true_count) {
  const T* values = static_cast<const T*>(col.values);
  if constexpr (std::is_same_v<T, float>) {
    uint64_t const_bits = 0;
    std::memcpy(&const_bits, &const_val, sizeof(const_bits));
    if (host_bits_is_nan_f64(const_bits)) {
      return launch_cmp_const_mask_f32_nan_const(values, col.nulls, row_count, cmp_opcode,
                                                 selection, true_count,
                                                 "expr_template_cmp_const_mask_usm");
    }
  }
  T typed_const{};
  if (const_to_typed<T>(const_val, &typed_const)) {
    return launch_cmp_const_mask_usm_typed<T>(values, col.nulls, row_count, cmp_opcode, typed_const,
                                              selection, true_count,
                                              "expr_template_cmp_const_mask_usm");
  }
  return launch_cmp_const_mask_usm_as_double<T>(values, col.nulls, row_count, cmp_opcode, const_val,
                                                selection, true_count,
                                                "expr_template_cmp_const_mask_usm");
}

template <typename T1, typename T2>
pgaccel_status dispatch_two_pred_and_count_usm_pair(pgaccel_expr_usm_col col1, uint16_t cmp1_opcode,
                                                    double const1_val, pgaccel_expr_usm_col col2,
                                                    uint16_t cmp2_opcode, double const2_val,
                                                    size_t row_count, size_t* true_count) {
  const T1* values1 = static_cast<const T1*>(col1.values);
  const T2* values2 = static_cast<const T2*>(col2.values);
  T1 typed_const1{};
  T2 typed_const2{};
  if (const_to_typed<T1>(const1_val, &typed_const1) &&
      const_to_typed<T2>(const2_val, &typed_const2)) {
    return launch_two_pred_and_count_usm_typed<T1, T2>(
        values1, col1.nulls, values2, col2.nulls, row_count, cmp1_opcode, typed_const1, cmp2_opcode,
        typed_const2, true_count, "expr_template_two_pred_and_count_usm");
  }
  return launch_two_pred_and_count_usm_as_double<T1, T2>(
      values1, col1.nulls, values2, col2.nulls, row_count, cmp1_opcode, const1_val, cmp2_opcode,
      const2_val, true_count, "expr_template_two_pred_and_count_usm");
}

template <typename T1, typename T2>
pgaccel_status dispatch_two_pred_and_mask_usm_pair(pgaccel_expr_usm_col col1, uint16_t cmp1_opcode,
                                                   double const1_val, pgaccel_expr_usm_col col2,
                                                   uint16_t cmp2_opcode, double const2_val,
                                                   size_t row_count, uint8_t* selection,
                                                   size_t* true_count) {
  const T1* values1 = static_cast<const T1*>(col1.values);
  const T2* values2 = static_cast<const T2*>(col2.values);
  T1 typed_const1{};
  T2 typed_const2{};
  if (const_to_typed<T1>(const1_val, &typed_const1) &&
      const_to_typed<T2>(const2_val, &typed_const2)) {
    return launch_two_pred_and_mask_usm_typed<T1, T2>(
        values1, col1.nulls, values2, col2.nulls, row_count, cmp1_opcode, typed_const1, cmp2_opcode,
        typed_const2, selection, true_count, "expr_template_two_pred_and_mask_usm");
  }
  return launch_two_pred_and_mask_usm_as_double<T1, T2>(
      values1, col1.nulls, values2, col2.nulls, row_count, cmp1_opcode, const1_val, cmp2_opcode,
      const2_val, selection, true_count, "expr_template_two_pred_and_mask_usm");
}

template <typename T1>
pgaccel_status dispatch_two_pred_and_count_usm_col2(pgaccel_expr_usm_col col1, uint16_t cmp1_opcode,
                                                    double const1_val, pgaccel_expr_usm_col col2,
                                                    uint16_t cmp2_opcode, double const2_val,
                                                    size_t row_count, size_t* true_count) {
  switch (col2.type) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return dispatch_two_pred_and_count_usm_pair<T1, int32_t>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, row_count, true_count);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return dispatch_two_pred_and_count_usm_pair<T1, int64_t>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, row_count, true_count);
    case PGACCEL_VAL_FLOAT32:
      return dispatch_two_pred_and_count_usm_pair<T1, float>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, row_count, true_count);
    case PGACCEL_VAL_FLOAT64:
      return dispatch_two_pred_and_count_usm_pair<T1, double>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, row_count, true_count);
    default:
      return PGACCEL_UNSUPPORTED;
  }
}

template <typename T1>
pgaccel_status dispatch_two_pred_and_mask_usm_col2(pgaccel_expr_usm_col col1, uint16_t cmp1_opcode,
                                                   double const1_val, pgaccel_expr_usm_col col2,
                                                   uint16_t cmp2_opcode, double const2_val,
                                                   size_t row_count, uint8_t* selection,
                                                   size_t* true_count) {
  switch (col2.type) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return dispatch_two_pred_and_mask_usm_pair<T1, int32_t>(col1, cmp1_opcode, const1_val, col2,
                                                              cmp2_opcode, const2_val, row_count,
                                                              selection, true_count);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return dispatch_two_pred_and_mask_usm_pair<T1, int64_t>(col1, cmp1_opcode, const1_val, col2,
                                                              cmp2_opcode, const2_val, row_count,
                                                              selection, true_count);
    case PGACCEL_VAL_FLOAT32:
      return dispatch_two_pred_and_mask_usm_pair<T1, float>(col1, cmp1_opcode, const1_val, col2,
                                                            cmp2_opcode, const2_val, row_count,
                                                            selection, true_count);
    case PGACCEL_VAL_FLOAT64:
      return dispatch_two_pred_and_mask_usm_pair<T1, double>(col1, cmp1_opcode, const1_val, col2,
                                                             cmp2_opcode, const2_val, row_count,
                                                             selection, true_count);
    default:
      return PGACCEL_UNSUPPORTED;
  }
}

pgaccel_status dispatch_two_pred_and_count_usm(pgaccel_expr_usm_col col1, uint16_t cmp1_opcode,
                                               double const1_val, pgaccel_expr_usm_col col2,
                                               uint16_t cmp2_opcode, double const2_val,
                                               size_t row_count, size_t* true_count) {
  switch (col1.type) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return dispatch_two_pred_and_count_usm_col2<int32_t>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, row_count, true_count);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return dispatch_two_pred_and_count_usm_col2<int64_t>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, row_count, true_count);
    case PGACCEL_VAL_FLOAT32:
      return dispatch_two_pred_and_count_usm_col2<float>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, row_count, true_count);
    case PGACCEL_VAL_FLOAT64:
      return dispatch_two_pred_and_count_usm_col2<double>(
          col1, cmp1_opcode, const1_val, col2, cmp2_opcode, const2_val, row_count, true_count);
    default:
      return PGACCEL_UNSUPPORTED;
  }
}

pgaccel_status dispatch_two_pred_and_mask_usm(pgaccel_expr_usm_col col1, uint16_t cmp1_opcode,
                                              double const1_val, pgaccel_expr_usm_col col2,
                                              uint16_t cmp2_opcode, double const2_val,
                                              size_t row_count, uint8_t* selection,
                                              size_t* true_count) {
  switch (col1.type) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return dispatch_two_pred_and_mask_usm_col2<int32_t>(col1, cmp1_opcode, const1_val, col2,
                                                          cmp2_opcode, const2_val, row_count,
                                                          selection, true_count);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return dispatch_two_pred_and_mask_usm_col2<int64_t>(col1, cmp1_opcode, const1_val, col2,
                                                          cmp2_opcode, const2_val, row_count,
                                                          selection, true_count);
    case PGACCEL_VAL_FLOAT32:
      return dispatch_two_pred_and_mask_usm_col2<float>(col1, cmp1_opcode, const1_val, col2,
                                                        cmp2_opcode, const2_val, row_count,
                                                        selection, true_count);
    case PGACCEL_VAL_FLOAT64:
      return dispatch_two_pred_and_mask_usm_col2<double>(col1, cmp1_opcode, const1_val, col2,
                                                         cmp2_opcode, const2_val, row_count,
                                                         selection, true_count);
    default:
      return PGACCEL_UNSUPPORTED;
  }
}

}  // namespace

extern "C" pgaccel_status pgaccel_expr_shared_alloc(size_t bytes, void** out) try {
  if (out == nullptr)
    return PGACCEL_ERROR;
  *out = nullptr;
  if (bytes == 0)
    return PGACCEL_OK;

  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  void* ptr = nullptr;
  try {
    ptr = sycl::malloc_shared(bytes, *q);
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: shared allocation failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: shared allocation failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
  if (ptr == nullptr)
    return PGACCEL_OOM;
  *out = ptr;
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_shared_alloc", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_shared_alloc", nullptr);
}

extern "C" void pgaccel_expr_shared_free(void* ptr) {
  if (ptr == nullptr)
    return;
  if (pgaccel_init() != PGACCEL_OK)
    return;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return;
  try {
    sycl::free(ptr, *q);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: %s\n", __func__, e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: unknown C++ exception\n", __func__);
  }
}

extern "C" pgaccel_status pgaccel_expr_device_alloc(size_t bytes, void** out) try {
  if (out == nullptr)
    return PGACCEL_ERROR;
  *out = nullptr;
  if (bytes == 0)
    return PGACCEL_OK;

  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  void* ptr = nullptr;
  try {
    ptr = sycl::malloc_device(bytes, *q);
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: device allocation failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: device allocation failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
  if (ptr == nullptr)
    return PGACCEL_OOM;
  *out = ptr;
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_device_alloc", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_device_alloc", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_device_alloc_copy(const void* src, size_t bytes,
                                                         void** out) try {
  if (out == nullptr)
    return PGACCEL_ERROR;
  *out = nullptr;
  if (bytes == 0)
    return PGACCEL_OK;
  if (src == nullptr)
    return PGACCEL_ERROR;

#if defined(__APPLE__)
  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  void* ptr = nullptr;
  try {
    // On Apple Silicon, shared USM is the stable resident representation for
    // host-built columns: kernels read it directly from unified memory and the
    // loader avoids Metal blit/copy-kernel paths that can crash forked
    // PostgreSQL backends in Apple's telemetry/logging helper thread.
    ptr = sycl::malloc_shared(bytes, *q);
    if (ptr == nullptr)
      return PGACCEL_OOM;
    std::memcpy(ptr, src, bytes);
    *out = ptr;
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: resident shared copy allocation failed: %s\n", e.what());
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident shared copy allocation failed: %s\n", e.what());
  }
  if (ptr != nullptr) {
    try {
      sycl::free(ptr, *q);
    } catch (...) {}
  }
  return PGACCEL_ERROR;
#else
  void* ptr = nullptr;
  pgaccel_status status = pgaccel_expr_device_alloc(bytes, &ptr);
  if (status != PGACCEL_OK)
    return status;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  try {
    q->memcpy(ptr, src, bytes).wait_and_throw();
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy failed: %s\n", e.what());
    sycl::free(ptr, *q);
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy failed: %s\n", e.what());
    sycl::free(ptr, *q);
    return PGACCEL_ERROR;
  }
  *out = ptr;
  return PGACCEL_OK;
#endif
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_device_alloc_copy", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_device_alloc_copy", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_device_copy_from_host(void* dst, const void* src,
                                                             size_t bytes) try {
  if (bytes == 0)
    return PGACCEL_OK;
  if (dst == nullptr || src == nullptr)
    return PGACCEL_ERROR;
  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  try {
    q->memcpy(dst, src, bytes).wait_and_throw();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy from host failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy from host failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_device_copy_from_host", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_device_copy_from_host", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_device_copy_to_host(void* dst, const void* src,
                                                           size_t bytes) try {
  if (bytes == 0)
    return PGACCEL_OK;
  if (dst == nullptr || src == nullptr)
    return PGACCEL_ERROR;
  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  try {
    q->memcpy(dst, src, bytes).wait_and_throw();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy to host failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy to host failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_device_copy_to_host", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_device_copy_to_host", nullptr);
}

extern "C" void pgaccel_expr_device_free(void* ptr) {
  if (ptr == nullptr)
    return;
  if (pgaccel_init() != PGACCEL_OK)
    return;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return;
  try {
    sycl::free(ptr, *q);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: %s\n", __func__, e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: unknown C++ exception\n", __func__);
  }
}

extern "C" pgaccel_status
pgaccel_expr_template_cmp_const_count_usm(pgaccel_expr_usm_col col, size_t row_count,
                                          uint16_t cmp_opcode, double const_val, size_t* true_count,
                                          size_t* uncertain_count) try {
  if (true_count != nullptr)
    *true_count = 0;
  if (uncertain_count != nullptr)
    *uncertain_count = 0;
  if (true_count == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp_opcode))
    return PGACCEL_UNSUPPORTED;
  if (row_count == 0 || col.type == PGACCEL_VAL_NULL)
    return PGACCEL_OK;
  if (col.values == nullptr)
    return PGACCEL_ERROR;

  switch (col.type) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return dispatch_cmp_const_count_usm<int32_t>(col, row_count, cmp_opcode, const_val,
                                                   true_count);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return dispatch_cmp_const_count_usm<int64_t>(col, row_count, cmp_opcode, const_val,
                                                   true_count);
    case PGACCEL_VAL_FLOAT32:
      return dispatch_cmp_const_count_usm<float>(col, row_count, cmp_opcode, const_val, true_count);
    case PGACCEL_VAL_FLOAT64:
      return dispatch_cmp_const_count_usm<double>(col, row_count, cmp_opcode, const_val,
                                                  true_count);
    default:
      return PGACCEL_UNSUPPORTED;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const_count_usm", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const_count_usm", nullptr);
}

extern "C" pgaccel_status
pgaccel_expr_template_cmp_const_mask_usm(pgaccel_expr_usm_col col, size_t row_count,
                                         uint16_t cmp_opcode, double const_val, uint8_t* selection,
                                         size_t* true_count, size_t* uncertain_count) try {
  if (true_count != nullptr)
    *true_count = 0;
  if (uncertain_count != nullptr)
    *uncertain_count = 0;
  if (true_count == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp_opcode))
    return PGACCEL_UNSUPPORTED;
  if (row_count == 0 || col.type == PGACCEL_VAL_NULL)
    return PGACCEL_OK;
  if (col.values == nullptr || selection == nullptr)
    return PGACCEL_ERROR;

  switch (col.type) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return dispatch_cmp_const_mask_usm<int32_t>(col, row_count, cmp_opcode, const_val, selection,
                                                  true_count);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return dispatch_cmp_const_mask_usm<int64_t>(col, row_count, cmp_opcode, const_val, selection,
                                                  true_count);
    case PGACCEL_VAL_FLOAT32:
      return dispatch_cmp_const_mask_usm<float>(col, row_count, cmp_opcode, const_val, selection,
                                                true_count);
    case PGACCEL_VAL_FLOAT64:
      return dispatch_cmp_const_mask_usm<double>(col, row_count, cmp_opcode, const_val, selection,
                                                 true_count);
    default:
      return PGACCEL_UNSUPPORTED;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const_mask_usm", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const_mask_usm", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_template_cmp_const_reduce_f32_usm(
    pgaccel_expr_usm_col pred_col, uint16_t cmp_opcode, double const_val,
    pgaccel_expr_usm_col value_col, size_t row_count, float* out_sum, float* out_min,
    float* out_max, int64_t* out_value_count, size_t* true_count, size_t* uncertain_count) try {
  if (out_sum != nullptr)
    *out_sum = 0.0f;
  if (out_min != nullptr)
    *out_min = 0.0f;
  if (out_max != nullptr)
    *out_max = 0.0f;
  if (out_value_count != nullptr)
    *out_value_count = 0;
  if (true_count != nullptr)
    *true_count = 0;
  if (uncertain_count != nullptr)
    *uncertain_count = 0;
  if (out_sum == nullptr || out_min == nullptr || out_max == nullptr ||
      out_value_count == nullptr || true_count == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp_opcode))
    return PGACCEL_UNSUPPORTED;
  if (row_count == 0 || pred_col.type == PGACCEL_VAL_NULL)
    return PGACCEL_OK;
  if (value_col.type != PGACCEL_VAL_FLOAT32)
    return PGACCEL_UNSUPPORTED;
  if (pred_col.values == nullptr || value_col.values == nullptr)
    return PGACCEL_ERROR;

  switch (pred_col.type) {
    case PGACCEL_VAL_INT32:
    case PGACCEL_VAL_DATE:
      return dispatch_cmp_const_reduce_f32_usm<int32_t>(pred_col, cmp_opcode, const_val, value_col,
                                                        row_count, out_sum, out_min, out_max,
                                                        out_value_count, true_count);
    case PGACCEL_VAL_INT64:
    case PGACCEL_VAL_TIMESTAMP:
      return dispatch_cmp_const_reduce_f32_usm<int64_t>(pred_col, cmp_opcode, const_val, value_col,
                                                        row_count, out_sum, out_min, out_max,
                                                        out_value_count, true_count);
    case PGACCEL_VAL_FLOAT32:
      return dispatch_cmp_const_reduce_f32_usm<float>(pred_col, cmp_opcode, const_val, value_col,
                                                      row_count, out_sum, out_min, out_max,
                                                      out_value_count, true_count);
    case PGACCEL_VAL_FLOAT64:
      return dispatch_cmp_const_reduce_f32_usm<double>(pred_col, cmp_opcode, const_val, value_col,
                                                       row_count, out_sum, out_min, out_max,
                                                       out_value_count, true_count);
    default:
      return PGACCEL_UNSUPPORTED;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const_reduce_f32_usm", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const_reduce_f32_usm", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_template_two_pred_and_count_usm(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, size_t row_count, size_t* true_count,
    size_t* uncertain_count) try {
  if (true_count != nullptr)
    *true_count = 0;
  if (uncertain_count != nullptr)
    *uncertain_count = 0;
  if (true_count == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp1_opcode) || !is_supported_cmp(cmp2_opcode))
    return PGACCEL_UNSUPPORTED;
  if (row_count == 0 || col1.type == PGACCEL_VAL_NULL || col2.type == PGACCEL_VAL_NULL)
    return PGACCEL_OK;
  if (col1.values == nullptr || col2.values == nullptr)
    return PGACCEL_ERROR;

  return dispatch_two_pred_and_count_usm(col1, cmp1_opcode, const1_val, col2, cmp2_opcode,
                                         const2_val, row_count, true_count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and_count_usm", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and_count_usm", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_template_two_pred_and_mask_usm(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, size_t row_count, uint8_t* selection,
    size_t* true_count, size_t* uncertain_count) try {
  if (true_count != nullptr)
    *true_count = 0;
  if (uncertain_count != nullptr)
    *uncertain_count = 0;
  if (true_count == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp1_opcode) || !is_supported_cmp(cmp2_opcode))
    return PGACCEL_UNSUPPORTED;
  if (row_count == 0 || col1.type == PGACCEL_VAL_NULL || col2.type == PGACCEL_VAL_NULL)
    return PGACCEL_OK;
  if (col1.values == nullptr || col2.values == nullptr || selection == nullptr)
    return PGACCEL_ERROR;

  return dispatch_two_pred_and_mask_usm(col1, cmp1_opcode, const1_val, col2, cmp2_opcode,
                                        const2_val, row_count, selection, true_count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and_mask_usm", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and_mask_usm", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_template_two_pred_and_reduce_f32_usm(
    pgaccel_expr_usm_col col1, uint16_t cmp1_opcode, double const1_val, pgaccel_expr_usm_col col2,
    uint16_t cmp2_opcode, double const2_val, pgaccel_expr_usm_col value_col, size_t row_count,
    float* out_sum, float* out_min, float* out_max, int64_t* out_value_count, size_t* true_count,
    size_t* uncertain_count) try {
  if (out_sum != nullptr)
    *out_sum = 0.0f;
  if (out_min != nullptr)
    *out_min = 0.0f;
  if (out_max != nullptr)
    *out_max = 0.0f;
  if (out_value_count != nullptr)
    *out_value_count = 0;
  if (true_count != nullptr)
    *true_count = 0;
  if (uncertain_count != nullptr)
    *uncertain_count = 0;
  if (out_sum == nullptr || out_min == nullptr || out_max == nullptr ||
      out_value_count == nullptr || true_count == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp1_opcode) || !is_supported_cmp(cmp2_opcode))
    return PGACCEL_UNSUPPORTED;
  if (row_count == 0 || col1.type == PGACCEL_VAL_NULL || col2.type == PGACCEL_VAL_NULL)
    return PGACCEL_OK;
  if (value_col.type != PGACCEL_VAL_FLOAT32)
    return PGACCEL_UNSUPPORTED;
  if (col1.values == nullptr || col2.values == nullptr || value_col.values == nullptr)
    return PGACCEL_ERROR;

  return dispatch_two_pred_and_reduce_f32_usm(col1, cmp1_opcode, const1_val, col2, cmp2_opcode,
                                              const2_val, value_col, row_count, out_sum, out_min,
                                              out_max, out_value_count, true_count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and_reduce_f32_usm", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and_reduce_f32_usm", nullptr);
}

// ===========================================================================
// Template 1: col <cmp> const
// ===========================================================================

extern "C" pgaccel_status pgaccel_expr_template_cmp_const(const pgaccel_batch* batch,
                                                          uint32_t col_idx, uint16_t cmp_opcode,
                                                          double const_val, int8_t* results) try {
  if (batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp_opcode))
    return PGACCEL_UNSUPPORTED;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  OneColScratch s{
      sycl::malloc_shared<double>(n, *q),
      sycl::malloc_shared<uint8_t>(n, *q),
      sycl::malloc_shared<int8_t>(n, *q),
      q,
  };
  if (s.d_col == nullptr || s.d_null == nullptr || s.d_res == nullptr)
    return PGACCEL_OOM;

  stage_col_f64(batch, col_idx, s.d_col, s.d_null);

  const double cv = const_val;
  const uint16_t op = cmp_opcode;
  double* d_col = s.d_col;
  uint8_t* d_null = s.d_null;
  int8_t* d_res = s.d_res;

  q->parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
     const size_t i = id[0];
     if (d_null[i]) {
       d_res[i] = PGACCEL_EXPR_FALSE;
     } else {
       d_res[i] = pg_cmp(op, d_col[i], cv) ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
     }
   }).wait_and_throw();

  std::memcpy(results, s.d_res, n * sizeof(int8_t));
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const", nullptr);
}

extern "C" pgaccel_status
pgaccel_expr_template_cmp_const_count(const pgaccel_batch* batch, uint32_t col_idx,
                                      uint16_t cmp_opcode, double const_val, size_t* true_count,
                                      size_t* uncertain_count) try {
  if (true_count != nullptr)
    *true_count = 0;
  if (uncertain_count != nullptr)
    *uncertain_count = 0;
  if (batch == nullptr || true_count == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp_opcode))
    return PGACCEL_UNSUPPORTED;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  const pgaccel_val_tag tag = batch_col_tag(batch, col_idx);
  float cv_f32 = 0.0f;
  if (tag == PGACCEL_VAL_FLOAT32 && const_to_f32_exact(const_val, &cv_f32)) {
    return launch_cmp_const_count_typed<float>(batch, col_idx, cmp_opcode, cv_f32,
                                               PGACCEL_VAL_FLOAT32, true_count,
                                               "expr_template_cmp_const_count_f32");
  }
  int32_t cv_i32 = 0;
  if ((tag == PGACCEL_VAL_INT32 || tag == PGACCEL_VAL_DATE) && const_to_i32(const_val, &cv_i32)) {
    return launch_cmp_const_count_typed<int32_t>(batch, col_idx, cmp_opcode, cv_i32, tag,
                                                 true_count, "expr_template_cmp_const_count_i32");
  }
  int64_t cv_i64 = 0;
  if ((tag == PGACCEL_VAL_INT64 || tag == PGACCEL_VAL_TIMESTAMP) &&
      const_to_i64(const_val, &cv_i64)) {
    return launch_cmp_const_count_typed<int64_t>(batch, col_idx, cmp_opcode, cv_i64, tag,
                                                 true_count, "expr_template_cmp_const_count_i64");
  }

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  CountOneColScratch s{
      sycl::malloc_shared<double>(n, *q),
      sycl::malloc_shared<uint8_t>(n, *q),
      q,
  };
  if (s.d_col == nullptr || s.d_null == nullptr)
    return PGACCEL_OOM;

  stage_col_f64(batch, col_idx, s.d_col, s.d_null);
  return launch_cmp_const_count_usm_typed<double>(s.d_col, s.d_null, n, cmp_opcode, const_val,
                                                  true_count, "expr_template_cmp_const_count");
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const_count", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_cmp_const_count", nullptr);
}

// ===========================================================================
// Template 2: col BETWEEN lo AND hi  (inclusive)
// ===========================================================================

extern "C" pgaccel_status pgaccel_expr_template_between(const pgaccel_batch* batch,
                                                        uint32_t col_idx, double lo, double hi,
                                                        int8_t* results) try {
  if (batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  OneColScratch s{
      sycl::malloc_shared<double>(n, *q),
      sycl::malloc_shared<uint8_t>(n, *q),
      sycl::malloc_shared<int8_t>(n, *q),
      q,
  };
  if (s.d_col == nullptr || s.d_null == nullptr || s.d_res == nullptr)
    return PGACCEL_OOM;

  stage_col_f64(batch, col_idx, s.d_col, s.d_null);

  const double clo = lo;
  const double chi = hi;
  double* d_col = s.d_col;
  uint8_t* d_null = s.d_null;
  int8_t* d_res = s.d_res;

  q->parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
     const size_t i = id[0];
     if (d_null[i]) {
       d_res[i] = PGACCEL_EXPR_FALSE;
     } else {
       const double v = d_col[i];
       const bool ok = pg_cmp(OP_GE, v, clo) && pg_cmp(OP_LE, v, chi);
       d_res[i] = ok ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
     }
   }).wait_and_throw();

  std::memcpy(results, s.d_res, n * sizeof(int8_t));
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_between", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_between", nullptr);
}

// ===========================================================================
// Template 3: col IN (v0, v1, ..., vN)  — up to 16 values
// ===========================================================================

extern "C" pgaccel_status pgaccel_expr_template_in_list(const pgaccel_batch* batch,
                                                        uint32_t col_idx, const double* values,
                                                        size_t value_count, int8_t* results) try {
  if (batch == nullptr || results == nullptr || values == nullptr)
    return PGACCEL_ERROR;
  if (value_count > 16)
    return PGACCEL_UNSUPPORTED;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  OneColScratch s{
      sycl::malloc_shared<double>(n, *q),
      sycl::malloc_shared<uint8_t>(n, *q),
      sycl::malloc_shared<int8_t>(n, *q),
      q,
  };
  // Need a separate device-accessible copy of the IN-list values.
  double* d_vals = sycl::malloc_shared<double>(value_count, *q);
  if (s.d_col == nullptr || s.d_null == nullptr || s.d_res == nullptr || d_vals == nullptr) {
    if (d_vals)
      sycl::free(d_vals, *q);
    return PGACCEL_OOM;
  }
  std::memcpy(d_vals, values, value_count * sizeof(double));
  stage_col_f64(batch, col_idx, s.d_col, s.d_null);

  const size_t vc = value_count;
  double* d_col = s.d_col;
  uint8_t* d_null = s.d_null;
  int8_t* d_res = s.d_res;

  q->parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
     const size_t i = id[0];
     if (d_null[i]) {
       d_res[i] = PGACCEL_EXPR_FALSE;
       return;
     }
     const double v = d_col[i];
     bool found = false;
     for (size_t k = 0; k < vc; ++k) {
       if (pg_cmp(OP_EQ, v, d_vals[k])) {
         found = true;
         break;
       }
     }
     d_res[i] = found ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
   }).wait_and_throw();

  std::memcpy(results, s.d_res, n * sizeof(int8_t));
  sycl::free(d_vals, *q);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_in_list", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_in_list", nullptr);
}

// ===========================================================================
// Template 4: col IS NULL / IS NOT NULL
// ===========================================================================

extern "C" pgaccel_status pgaccel_expr_template_is_null(const pgaccel_batch* batch,
                                                        uint32_t col_idx, bool check_not_null,
                                                        int8_t* results) try {
  if (batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  uint8_t* d_null = sycl::malloc_shared<uint8_t>(n, *q);
  int8_t* d_res = sycl::malloc_shared<int8_t>(n, *q);
  if (d_null == nullptr || d_res == nullptr) {
    if (d_null)
      sycl::free(d_null, *q);
    if (d_res)
      sycl::free(d_res, *q);
    return PGACCEL_OOM;
  }

  stage_null_mask(batch, col_idx, n, d_null);
  const int8_t hit = check_not_null ? PGACCEL_EXPR_FALSE : PGACCEL_EXPR_TRUE;
  const int8_t miss = check_not_null ? PGACCEL_EXPR_TRUE : PGACCEL_EXPR_FALSE;
  uint8_t* dn = d_null;
  int8_t* dr = d_res;

  q->parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
     const size_t i = id[0];
     dr[i] = dn[i] ? hit : miss;
   }).wait_and_throw();

  std::memcpy(results, d_res, n * sizeof(int8_t));
  sycl::free(d_null, *q);
  sycl::free(d_res, *q);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_is_null", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_is_null", nullptr);
}

// ===========================================================================
// Template 5: col1 <cmp1> const1 AND col2 <cmp2> const2
// ===========================================================================

namespace {

// Pre-encoded f64 → u64 bit pattern for kernel-side reconstruction.
//
// AdaptiveCpp Metal SSCP emits one `[[id(N)]]` argbuffer slot per captured
// field, but its slot accounting is broken for `acpp_f64` (8-byte scalar)
// fields — each f64 takes 1 id slot in the emitter's count but Metal's
// argbuffer layout actually requires 2, so any field after an f64 collides
// with the previous slot (`'id' set location to N, but minimum is N+1`).
//
// Workaround: never put a literal `double` in the captured struct. Instead
// transmit the f64 as a `uint64_t` bit pattern (1 id slot, no alignment
// surprise) and reconstruct on the kernel side via `sycl::bit_cast`. The
// captures become entirely 1-slot fields (pointers + 1-slot scalars),
// keeping the emitter's id accounting in step with Metal's expectation.
struct TwoPredAndParams {
  const double* col1;
  const uint8_t* null1;
  const double* col2;
  const uint8_t* null2;
  int8_t* res;
  uint64_t cv1_bits;
  uint64_t cv2_bits;
  uint16_t op1;
  uint16_t op2;
  size_t n;
};

}  // namespace

extern "C" pgaccel_status
pgaccel_expr_template_two_pred_and(const pgaccel_batch* batch, uint32_t col1_idx,
                                   uint16_t cmp1_opcode, double const1_val, uint32_t col2_idx,
                                   uint16_t cmp2_opcode, double const2_val, int8_t* results) try {
  if (batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp1_opcode) || !is_supported_cmp(cmp2_opcode))
    return PGACCEL_UNSUPPORTED;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  double* d_col1 = sycl::malloc_shared<double>(n, *q);
  uint8_t* d_null1 = sycl::malloc_shared<uint8_t>(n, *q);
  double* d_col2 = sycl::malloc_shared<double>(n, *q);
  uint8_t* d_null2 = sycl::malloc_shared<uint8_t>(n, *q);
  int8_t* d_res = sycl::malloc_shared<int8_t>(n, *q);
  if (d_col1 == nullptr || d_null1 == nullptr || d_col2 == nullptr || d_null2 == nullptr ||
      d_res == nullptr) {
    if (d_col1)
      sycl::free(d_col1, *q);
    if (d_null1)
      sycl::free(d_null1, *q);
    if (d_col2)
      sycl::free(d_col2, *q);
    if (d_null2)
      sycl::free(d_null2, *q);
    if (d_res)
      sycl::free(d_res, *q);
    return PGACCEL_OOM;
  }

  stage_col_f64(batch, col1_idx, d_col1, d_null1);
  stage_col_f64(batch, col2_idx, d_col2, d_null2);

  uint64_t cv1_bits;
  uint64_t cv2_bits;
  std::memcpy(&cv1_bits, &const1_val, sizeof(cv1_bits));
  std::memcpy(&cv2_bits, &const2_val, sizeof(cv2_bits));

  const TwoPredAndParams p{
      d_col1, d_null1, d_col2, d_null2, d_res, cv1_bits, cv2_bits, cmp1_opcode, cmp2_opcode, n,
  };

  q->parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
     const size_t i = id[0];
     // Reconstruct f64 constants from u64 bit pattern. `sycl::bit_cast`
     // is a SYCL 2020 builtin device-callable equivalent to memcpy-bitcast.
     const double cv1 = sycl::bit_cast<double>(p.cv1_bits);
     const double cv2 = sycl::bit_cast<double>(p.cv2_bits);
     if (p.null1[i] || !pg_cmp(p.op1, p.col1[i], cv1)) {
       p.res[i] = PGACCEL_EXPR_FALSE;
       return;
     }
     if (p.null2[i] || !pg_cmp(p.op2, p.col2[i], cv2)) {
       p.res[i] = PGACCEL_EXPR_FALSE;
       return;
     }
     p.res[i] = PGACCEL_EXPR_TRUE;
   }).wait_and_throw();

  std::memcpy(results, d_res, n * sizeof(int8_t));
  sycl::free(d_col1, *q);
  sycl::free(d_null1, *q);
  sycl::free(d_col2, *q);
  sycl::free(d_null2, *q);
  sycl::free(d_res, *q);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and", nullptr);
}

extern "C" pgaccel_status
pgaccel_expr_template_two_pred_and_count(const pgaccel_batch* batch, uint32_t col1_idx,
                                         uint16_t cmp1_opcode, double const1_val, uint32_t col2_idx,
                                         uint16_t cmp2_opcode, double const2_val,
                                         size_t* true_count, size_t* uncertain_count) try {
  if (true_count != nullptr)
    *true_count = 0;
  if (uncertain_count != nullptr)
    *uncertain_count = 0;
  if (batch == nullptr || true_count == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp1_opcode) || !is_supported_cmp(cmp2_opcode))
    return PGACCEL_UNSUPPORTED;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  const pgaccel_val_tag tag1 = batch_col_tag(batch, col1_idx);
  const pgaccel_val_tag tag2 = batch_col_tag(batch, col2_idx);
  float cv1_f32 = 0.0f;
  float cv2_f32 = 0.0f;
  const bool c1_f32 = const_to_f32_exact(const1_val, &cv1_f32);
  const bool c2_f32 = const_to_f32_exact(const2_val, &cv2_f32);
  int32_t cv1_i32 = 0;
  int32_t cv2_i32 = 0;
  const bool c1_i32 = const_to_i32(const1_val, &cv1_i32);
  const bool c2_i32 = const_to_i32(const2_val, &cv2_i32);
  if (tag1 == PGACCEL_VAL_FLOAT32 && tag2 == PGACCEL_VAL_FLOAT32 && c1_f32 && c2_f32) {
    return launch_two_pred_and_count_typed<float, float>(
        batch, col1_idx, cmp1_opcode, cv1_f32, PGACCEL_VAL_FLOAT32, col2_idx, cmp2_opcode, cv2_f32,
        PGACCEL_VAL_FLOAT32, true_count, "expr_template_two_pred_and_count_f32_f32");
  }
  if (tag1 == PGACCEL_VAL_FLOAT32 && tag2 == PGACCEL_VAL_INT32 && c1_f32 && c2_i32) {
    return launch_two_pred_and_count_typed<float, int32_t>(
        batch, col1_idx, cmp1_opcode, cv1_f32, PGACCEL_VAL_FLOAT32, col2_idx, cmp2_opcode, cv2_i32,
        PGACCEL_VAL_INT32, true_count, "expr_template_two_pred_and_count_f32_i32");
  }
  if (tag1 == PGACCEL_VAL_INT32 && tag2 == PGACCEL_VAL_FLOAT32 && c1_i32 && c2_f32) {
    return launch_two_pred_and_count_typed<int32_t, float>(
        batch, col1_idx, cmp1_opcode, cv1_i32, PGACCEL_VAL_INT32, col2_idx, cmp2_opcode, cv2_f32,
        PGACCEL_VAL_FLOAT32, true_count, "expr_template_two_pred_and_count_i32_f32");
  }
  if (tag1 == PGACCEL_VAL_INT32 && tag2 == PGACCEL_VAL_INT32 && c1_i32 && c2_i32) {
    return launch_two_pred_and_count_typed<int32_t, int32_t>(
        batch, col1_idx, cmp1_opcode, cv1_i32, PGACCEL_VAL_INT32, col2_idx, cmp2_opcode, cv2_i32,
        PGACCEL_VAL_INT32, true_count, "expr_template_two_pred_and_count_i32_i32");
  }

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  CountTwoColScratch s{
      sycl::malloc_shared<double>(n, *q),
      sycl::malloc_shared<uint8_t>(n, *q),
      sycl::malloc_shared<double>(n, *q),
      sycl::malloc_shared<uint8_t>(n, *q),
      q,
  };
  if (s.d_col1 == nullptr || s.d_null1 == nullptr || s.d_col2 == nullptr || s.d_null2 == nullptr)
    return PGACCEL_OOM;

  stage_col_f64(batch, col1_idx, s.d_col1, s.d_null1);
  stage_col_f64(batch, col2_idx, s.d_col2, s.d_null2);
  return launch_two_pred_and_count_usm_typed<double, double>(
      s.d_col1, s.d_null1, s.d_col2, s.d_null2, n, cmp1_opcode, const1_val, cmp2_opcode, const2_val,
      true_count, "expr_template_two_pred_and_count");
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and_count", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_template_two_pred_and_count", nullptr);
}
