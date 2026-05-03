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

#include <cstdlib>
#include <cstring>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"

extern sycl::queue* g_queue;

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

// Validates the opcode is one of the comparison ops we support. Returns
// `true` if the kernel will recognise it.
bool is_supported_cmp(uint16_t opcode) {
  return opcode == OP_LT || opcode == OP_LE || opcode == OP_GT || opcode == OP_GE ||
         opcode == OP_EQ || opcode == OP_NE;
}

// Device-callable PG-NaN-aware comparison. Inlined into kernel body so
// the SYCL emitter sees a single function path. NaN semantics match the
// host helpers from the previous implementation:
//   - LT:  NaN < anything = false; anything < NaN = true (NaN sorts highest)
//   - LE:  NaN <= NaN = true;  NaN <= other = false;  other <= NaN = true
//   - EQ:  NaN == NaN = true;  NaN == other = false
inline bool pg_cmp(uint16_t op, double a, double b) {
  const bool a_nan = a != a;
  const bool b_nan = b != b;
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

}  // namespace

// ===========================================================================
// Template 1: col <cmp> const
// ===========================================================================

extern "C" pgaccel_status pgaccel_expr_template_cmp_const(const pgaccel_batch* batch,
                                                          uint32_t col_idx, uint16_t cmp_opcode,
                                                          double const_val, int8_t* results) {
  if (batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp_opcode))
    return PGACCEL_UNSUPPORTED;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  pgaccel_init();
  sycl::queue* q = g_queue;
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
   }).wait();

  std::memcpy(results, s.d_res, n * sizeof(int8_t));
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

// ===========================================================================
// Template 2: col BETWEEN lo AND hi  (inclusive)
// ===========================================================================

extern "C" pgaccel_status pgaccel_expr_template_between(const pgaccel_batch* batch,
                                                        uint32_t col_idx, double lo, double hi,
                                                        int8_t* results) {
  if (batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  pgaccel_init();
  sycl::queue* q = g_queue;
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
   }).wait();

  std::memcpy(results, s.d_res, n * sizeof(int8_t));
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

// ===========================================================================
// Template 3: col IN (v0, v1, ..., vN)  — up to 16 values
// ===========================================================================

extern "C" pgaccel_status pgaccel_expr_template_in_list(const pgaccel_batch* batch,
                                                        uint32_t col_idx, const double* values,
                                                        size_t value_count, int8_t* results) {
  if (batch == nullptr || results == nullptr || values == nullptr)
    return PGACCEL_ERROR;
  if (value_count > 16)
    return PGACCEL_UNSUPPORTED;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  pgaccel_init();
  sycl::queue* q = g_queue;
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
   }).wait();

  std::memcpy(results, s.d_res, n * sizeof(int8_t));
  sycl::free(d_vals, *q);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

// ===========================================================================
// Template 4: col IS NULL / IS NOT NULL
// ===========================================================================

extern "C" pgaccel_status pgaccel_expr_template_is_null(const pgaccel_batch* batch,
                                                        uint32_t col_idx, bool check_not_null,
                                                        int8_t* results) {
  if (batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  pgaccel_init();
  sycl::queue* q = g_queue;
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
   }).wait();

  std::memcpy(results, d_res, n * sizeof(int8_t));
  sycl::free(d_null, *q);
  sycl::free(d_res, *q);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
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
                                   uint16_t cmp2_opcode, double const2_val, int8_t* results) {
  if (batch == nullptr || results == nullptr)
    return PGACCEL_ERROR;
  if (!is_supported_cmp(cmp1_opcode) || !is_supported_cmp(cmp2_opcode))
    return PGACCEL_UNSUPPORTED;

  const size_t n = batch->num_rows;
  if (n == 0)
    return PGACCEL_OK;

  pgaccel_init();
  sycl::queue* q = g_queue;
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
   }).wait();

  std::memcpy(results, d_res, n * sizeof(int8_t));
  sycl::free(d_col1, *q);
  sycl::free(d_null1, *q);
  sycl::free(d_col2, *q);
  sycl::free(d_null2, *q);
  sycl::free(d_res, *q);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}
