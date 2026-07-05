// olap_ssbm.cpp -- GPU OLAP kernels for Star Schema Benchmark lanes.
//
// SSBM Q1.x is the first OLAP target: filter resident lineorder fact columns
// and reduce SUM(lo_extendedprice * lo_discount) without materializing rows.

#include <sycl/sycl.hpp>

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <new>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"

extern sycl::queue* g_queue;
void pgaccel_record_gpu_exec();

namespace {

constexpr size_t ROWS_PER_ITEM = 64;
constexpr size_t REDUCE_FANIN = 256;
constexpr size_t DENSE_GROUP_LOW_LOCAL_SIZE = 256;
constexpr size_t DENSE_GROUP_HIGH_LOCAL_SIZE = 64;
constexpr size_t DENSE_GROUP_SORT_MIN_GROUPS = 4096;
constexpr size_t DENSE_GROUP_SORT_MIN_ROWS = 65536;
constexpr size_t DENSE_GROUP_TILE_LOCAL_SIZE = 64;
constexpr size_t DENSE_GROUP_TILE_GROUPS = 16;
constexpr size_t DENSE_GROUP_ONE_PASS_LOCAL_SIZE = 32;
constexpr size_t DENSE_GROUP_SIMPLE_WIDE_LOCAL_SIZE = 8;
constexpr size_t DENSE_GROUP_ONE_PASS_TILE_GROUPS = 128;
constexpr size_t DENSE_GROUP_ONE_PASS_MAX_GROUPS = 256;
constexpr size_t DENSE_GROUP_ONE_PASS_BLOCK_ROWS = 8192;
constexpr size_t DENSE_GROUP_BLOCKED_MIN_ROWS = 262144;
constexpr size_t DENSE_GROUP_BLOCK_ROWS = 4096;
constexpr size_t DENSE_GROUP_MINMAX_BLOCK_ROWS = 16384;
constexpr size_t DENSE_GROUP_MINMAX_TILE_GROUPS = DENSE_GROUP_TILE_GROUPS;
constexpr size_t DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE = 256;
constexpr uint32_t DENSE_GROUP_AGG_SUM = 1u << 0;
constexpr uint32_t DENSE_GROUP_AGG_MIN = 1u << 1;
constexpr uint32_t DENSE_GROUP_AGG_MAX = 1u << 2;
constexpr uint32_t DENSE_GROUP_AGG_COUNT = 1u << 3;
constexpr uint32_t DENSE_GROUP_AGG_SUMSQ = 1u << 4;
constexpr uint32_t DENSE_GROUP_AGG_ALL =
    DENSE_GROUP_AGG_SUM | DENSE_GROUP_AGG_MIN | DENSE_GROUP_AGG_MAX | DENSE_GROUP_AGG_COUNT;
constexpr uint32_t RESIDENT_F64_REDUCE_AGG_ALL = DENSE_GROUP_AGG_ALL | DENSE_GROUP_AGG_SUMSQ;
constexpr int32_t DENSE_GROUP_FILTER_ROWS = 0;
constexpr int32_t DENSE_GROUP_FILTER_MEASURE_ONLY = 1;
constexpr int32_t DENSE_GROUP_MEASURE_PRED_BOOL_ONLY = 0;
constexpr int32_t DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_BETWEEN = 1;
constexpr int32_t DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_RANGES = 2;
constexpr int32_t DENSE_GROUP_MEASURE_PRED_SOURCE_VALUE = 0;
constexpr int32_t DENSE_GROUP_MEASURE_PRED_SOURCE_RHS = 1;
constexpr int32_t DENSE_GROUP_MUL_FILTER_NONE = 0;
constexpr int32_t DENSE_GROUP_MUL_FILTER_AGGREGATE = 1;
constexpr int32_t DENSE_GROUP_MUL_FILTER_MEASURE_ONLY = 2;
constexpr uint16_t OP_EQ = 40;
constexpr uint16_t OP_NE = 41;
constexpr uint16_t OP_LT = 42;
constexpr uint16_t OP_LE = 43;
constexpr uint16_t OP_GT = 44;
constexpr uint16_t OP_GE = 45;
constexpr uint16_t OP_ALWAYS_TRUE = 46;

size_t partial_item_count(size_t row_count) {
  return row_count / ROWS_PER_ITEM + ((row_count % ROWS_PER_ITEM) != 0);
}

size_t fanin_group_count(size_t count) {
  return count / REDUCE_FANIN + ((count % REDUCE_FANIN) != 0);
}

inline bool date_key_matches(int32_t key, int32_t min_key, int32_t max_key,
                             const int32_t* filter_keys, size_t filter_count) {
  if (filter_count == 0)
    return key >= min_key && key <= max_key;
  for (size_t i = 0; i < filter_count; ++i) {
    if (filter_keys[i] == key)
      return true;
  }
  return false;
}

bool valid_i32_col(pgaccel_expr_usm_col col) {
  return col.values != nullptr && col.type == PGACCEL_VAL_INT32;
}

bool valid_f64_col(pgaccel_expr_usm_col col) {
  return col.values != nullptr && col.type == PGACCEL_VAL_FLOAT64;
}

bool valid_optional_bool_col(pgaccel_expr_usm_col col) {
  return col.values == nullptr || col.type == PGACCEL_VAL_BOOL;
}

bool valid_measure_rhs_col(pgaccel_expr_usm_col col, bool required) {
  if (!required)
    return col.values == nullptr || col.type == PGACCEL_VAL_FLOAT64;
  return col.values != nullptr && col.type == PGACCEL_VAL_FLOAT64;
}

bool valid_cmp_opcode(uint16_t opcode) {
  return opcode == OP_EQ || opcode == OP_NE || opcode == OP_LT || opcode == OP_LE ||
         opcode == OP_GT || opcode == OP_GE || opcode == OP_ALWAYS_TRUE;
}

inline bool pg_is_nan_f64(double value) {
  const uint64_t bits = sycl::bit_cast<uint64_t>(value);
  return (bits & 0x7ff0000000000000ULL) == 0x7ff0000000000000ULL &&
         (bits & 0x000fffffffffffffULL) != 0;
}

inline bool compare_f64(double lhs, uint16_t opcode, double rhs) {
  const bool lhs_nan = pg_is_nan_f64(lhs);
  const bool rhs_nan = pg_is_nan_f64(rhs);
  switch (opcode) {
    case OP_EQ:
      if (lhs_nan && rhs_nan)
        return true;
      if (lhs_nan || rhs_nan)
        return false;
      return lhs == rhs;
    case OP_NE:
      if (lhs_nan && rhs_nan)
        return false;
      if (lhs_nan || rhs_nan)
        return true;
      return lhs != rhs;
    case OP_LT:
      if (lhs_nan)
        return false;
      if (rhs_nan)
        return true;
      return lhs < rhs;
    case OP_LE:
      if (lhs_nan && rhs_nan)
        return true;
      if (lhs_nan)
        return false;
      if (rhs_nan)
        return true;
      return lhs <= rhs;
    case OP_GT:
      if (rhs_nan)
        return false;
      if (lhs_nan)
        return true;
      return lhs > rhs;
    case OP_GE:
      if (lhs_nan && rhs_nan)
        return true;
      if (rhs_nan)
        return false;
      if (lhs_nan)
        return true;
      return lhs >= rhs;
    case OP_ALWAYS_TRUE:
      return true;
    default:
      return false;
  }
}

inline double resident_dense_measure_value(double lhs, double rhs, int32_t measure_op) {
  switch (measure_op) {
    case 0:
      return lhs;
    case 1:
      return lhs * rhs;
    case 2:
      return lhs - rhs;
    default:
      return lhs;
  }
}

inline bool resident_dense_filter_passes(
    size_t row, const uint8_t* filter, const uint8_t* filter_nulls, const double* values,
    const uint8_t* value_nulls, const double* rhs_values, const uint8_t* rhs_nulls,
    int32_t measure_predicate_source, int32_t measure_predicate_op,
    int32_t measure_predicate_range_count, double measure_predicate_lo0,
    double measure_predicate_hi0, double measure_predicate_lo1, double measure_predicate_hi1,
    double measure_predicate_lo2, double measure_predicate_hi2, double measure_predicate_lo3,
    double measure_predicate_hi3) {
  const bool bool_passes =
      filter == nullptr || (!(filter_nulls && filter_nulls[row]) && filter[row] != 0);
  if (!bool_passes)
    return false;
  switch (measure_predicate_op) {
    case DENSE_GROUP_MEASURE_PRED_BOOL_ONLY:
      return true;
    case DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_BETWEEN:
    case DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_RANGES: {
      const bool predicate_source_rhs =
          measure_predicate_source == DENSE_GROUP_MEASURE_PRED_SOURCE_RHS;
      const double* predicate_values = predicate_source_rhs ? rhs_values : values;
      const uint8_t* predicate_nulls = predicate_source_rhs ? rhs_nulls : value_nulls;
      if (predicate_values == nullptr || (predicate_nulls && predicate_nulls[row]))
        return false;
      const double predicate_value = predicate_values[row];
      return (measure_predicate_range_count >= 1 && predicate_value >= measure_predicate_lo0 &&
              predicate_value <= measure_predicate_hi0) ||
             (measure_predicate_range_count >= 2 && predicate_value >= measure_predicate_lo1 &&
              predicate_value <= measure_predicate_hi1) ||
             (measure_predicate_range_count >= 3 && predicate_value >= measure_predicate_lo2 &&
              predicate_value <= measure_predicate_hi2) ||
             (measure_predicate_range_count >= 4 && predicate_value >= measure_predicate_lo3 &&
              predicate_value <= measure_predicate_hi3);
    }
    default:
      return false;
  }
}

void log_dense_grouped_f64_preflight_failure(
    const char* reason, pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col filter_col, size_t row_count, int32_t group_min, int32_t group_count,
    double* scratch_sum, double* scratch_min, double* scratch_max, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_cursor, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_min_by_group, double* out_max_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count) {
  std::fprintf(stderr,
               "pgaccel: resident dense grouped f64 preflight failed: reason=%s "
               "row_count=%zu group_min=%d group_count=%d "
               "group_values=%p group_nulls=%p group_type=%d "
               "value_values=%p value_nulls=%p value_type=%d "
               "filter_values=%p filter_nulls=%p filter_type=%d "
               "scratch_sum=%p scratch_min=%p scratch_max=%p scratch_count=%p "
               "scratch_group_start=%p scratch_group_cursor=%p scratch_group_capacity=%zu "
               "scratch_sorted_group=%p scratch_row_index=%p scratch_row_capacity=%zu "
               "out_sum=%p out_min=%p out_max=%p out_count=%p out_group_capacity=%zu "
               "selected_count=%p uncertain_count=%p\n",
               reason, row_count, group_min, group_count, group_col.values, group_col.nulls,
               static_cast<int>(group_col.type), value_col.values, value_col.nulls,
               static_cast<int>(value_col.type), filter_col.values, filter_col.nulls,
               static_cast<int>(filter_col.type), static_cast<void*>(scratch_sum),
               static_cast<void*>(scratch_min), static_cast<void*>(scratch_max),
               static_cast<void*>(scratch_count), static_cast<void*>(scratch_group_start),
               static_cast<void*>(scratch_group_cursor), scratch_group_capacity,
               static_cast<void*>(scratch_sorted_group), static_cast<void*>(scratch_row_index),
               scratch_row_capacity, static_cast<void*>(out_sum_by_group),
               static_cast<void*>(out_min_by_group), static_cast<void*>(out_max_by_group),
               static_cast<void*>(out_count_by_group), out_group_capacity,
               static_cast<void*>(selected_count), static_cast<void*>(uncertain_count));
  std::fflush(stderr);
}

bool valid_group_output(size_t out_group_capacity, int32_t year_count, int32_t brand_count) {
  if (year_count <= 0 || brand_count <= 0)
    return false;
  const size_t years = static_cast<size_t>(year_count);
  const size_t brands = static_cast<size_t>(brand_count);
  return brands != 0 && years <= (static_cast<size_t>(-1) / brands) &&
         out_group_capacity >= years * brands;
}

bool valid_group_output3(size_t out_group_capacity, int32_t year_count,
                         int32_t customer_group_count, int32_t supplier_group_count) {
  if (year_count <= 0 || customer_group_count <= 0 || supplier_group_count <= 0)
    return false;
  const size_t years = static_cast<size_t>(year_count);
  const size_t customer_groups = static_cast<size_t>(customer_group_count);
  const size_t supplier_groups = static_cast<size_t>(supplier_group_count);
  const size_t max_size = static_cast<size_t>(-1);
  if (customer_groups == 0 || supplier_groups == 0 || years > max_size / customer_groups)
    return false;
  const size_t year_customer_groups = years * customer_groups;
  return year_customer_groups <= max_size / supplier_groups &&
         out_group_capacity >= year_customer_groups * supplier_groups;
}

struct SsbmQ4GroupedProfitKernelParams {
  const int32_t* orderdate;
  const int32_t* custkey;
  const int32_t* suppkey;
  const int32_t* partkey;
  const int32_t* revenue;
  const int32_t* supplycost;
  const uint8_t* orderdate_nulls;
  const uint8_t* custkey_nulls;
  const uint8_t* suppkey_nulls;
  const uint8_t* partkey_nulls;
  const uint8_t* revenue_nulls;
  const uint8_t* supplycost_nulls;
  int32_t date_key_min;
  const int32_t* date_year_by_offset;
  const uint8_t* date_match_by_offset;
  size_t date_year_count;
  const int32_t* customer_group_code_by_key;
  const uint8_t* customer_match_by_key;
  size_t customer_key_count;
  const int32_t* supplier_group_code_by_key;
  const uint8_t* supplier_match_by_key;
  size_t supplier_key_count;
  const int32_t* part_group_code_by_key;
  const uint8_t* part_match_by_key;
  size_t part_key_count;
  int32_t group_geo_source;
  int32_t year_min;
  int32_t year_count;
  int32_t geo_group_count;
  int32_t part_group_count;
  uint32_t* device_profit_lo;
  uint32_t* device_profit_hi;
  uint32_t* device_count;
};

pgaccel_status run_ssbm_q1_revenue_i64_scratch(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col discount_col,
    pgaccel_expr_usm_col quantity_col, pgaccel_expr_usm_col extendedprice_col, size_t row_count,
    int32_t orderdate_lo, int32_t orderdate_hi, const int32_t* orderdate_keys,
    size_t orderdate_key_count, int32_t discount_lo, int32_t discount_hi, int32_t quantity_lo,
    int32_t quantity_hi, int64_t* scratch_revenue_a, int64_t* scratch_count_a,
    int64_t* scratch_revenue_b, int64_t* scratch_count_b, size_t scratch_item_capacity,
    int64_t* out_sum, size_t* selected_count, size_t* uncertain_count) {
  if (out_sum)
    *out_sum = 0;
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;

  if (!out_sum || !selected_count || !uncertain_count)
    return PGACCEL_ERROR;

  if (row_count == 0)
    return PGACCEL_OK;
  if (!valid_i32_col(orderdate_col) || !valid_i32_col(discount_col) ||
      !valid_i32_col(quantity_col) || !valid_i32_col(extendedprice_col))
    return PGACCEL_ERROR;
  if (orderdate_key_count > 0 && !orderdate_keys)
    return PGACCEL_ERROR;

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t item_count = partial_item_count(row_count);
  if (!scratch_revenue_a || !scratch_count_a || !scratch_revenue_b || !scratch_count_b ||
      scratch_item_capacity < item_count)
    return PGACCEL_ERROR;

  const auto* orderdate = static_cast<const int32_t*>(orderdate_col.values);
  const auto* discount = static_cast<const int32_t*>(discount_col.values);
  const auto* quantity = static_cast<const int32_t*>(quantity_col.values);
  const auto* extendedprice = static_cast<const int32_t*>(extendedprice_col.values);
  const uint8_t* orderdate_nulls = orderdate_col.nulls;
  const uint8_t* discount_nulls = discount_col.nulls;
  const uint8_t* quantity_nulls = quantity_col.nulls;
  const uint8_t* extendedprice_nulls = extendedprice_col.nulls;

  try {
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(item_count), [=](sycl::id<1> id) {
         const size_t item_id = id[0];
         const size_t start = item_id * ROWS_PER_ITEM;

         int64_t revenue = 0;
         int64_t selected = 0;
         for (size_t j = 0; j < ROWS_PER_ITEM; ++j) {
           const size_t row = start + j;
           if (row >= row_count)
             continue;
           if ((orderdate_nulls && orderdate_nulls[row]) ||
               (discount_nulls && discount_nulls[row]) || (quantity_nulls && quantity_nulls[row]) ||
               (extendedprice_nulls && extendedprice_nulls[row])) {
             continue;
           }

           const int32_t orderdate_value = orderdate[row];
           const int32_t discount_value = discount[row];
           const int32_t quantity_value = quantity[row];
           if (!date_key_matches(orderdate_value, orderdate_lo, orderdate_hi, orderdate_keys,
                                 orderdate_key_count))
             continue;
           if (discount_value < discount_lo || discount_value > discount_hi)
             continue;
           if (quantity_value < quantity_lo || quantity_value > quantity_hi)
             continue;

           revenue +=
               static_cast<int64_t>(extendedprice[row]) * static_cast<int64_t>(discount_value);
           selected += 1;
         }

         scratch_revenue_a[item_id] = revenue;
         scratch_count_a[item_id] = selected;
       });
     }).wait_and_throw();

    int64_t* in_revenue = scratch_revenue_a;
    int64_t* in_count = scratch_count_a;
    int64_t* out_revenue = scratch_revenue_b;
    int64_t* out_count = scratch_count_b;
    size_t in_count_items = item_count;
    while (in_count_items > 1) {
      const size_t out_count_items = fanin_group_count(in_count_items);
      q->submit([&](sycl::handler& h) {
         h.parallel_for(sycl::range<1>(out_count_items), [=](sycl::id<1> id) {
           const size_t out_idx = id[0];
           const size_t start = out_idx * REDUCE_FANIN;
           const size_t end = sycl::min(start + REDUCE_FANIN, in_count_items);
           int64_t revenue = 0;
           int64_t selected = 0;
           for (size_t i = start; i < end; ++i) {
             revenue += in_revenue[i];
             selected += in_count[i];
           }
           out_revenue[out_idx] = revenue;
           out_count[out_idx] = selected;
         });
       }).wait_and_throw();

      int64_t* tmp_revenue = in_revenue;
      int64_t* tmp_count = in_count;
      in_revenue = out_revenue;
      in_count = out_count;
      out_revenue = tmp_revenue;
      out_count = tmp_count;
      in_count_items = out_count_items;
    }

    int64_t final_revenue = 0;
    int64_t final_selected = 0;
    q->memcpy(&final_revenue, in_revenue, sizeof(final_revenue)).wait_and_throw();
    q->memcpy(&final_selected, in_count, sizeof(final_selected)).wait_and_throw();
    *out_sum = final_revenue;
    *selected_count = static_cast<size_t>(final_selected);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: SSBM Q1 revenue kernel failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: SSBM Q1 revenue kernel failed (unknown)\n");
    return PGACCEL_ERROR;
  }

  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_expr_template_ssbm_q3_grouped_revenue_i64_usm(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col custkey_col,
    pgaccel_expr_usm_col suppkey_col, pgaccel_expr_usm_col revenue_col, size_t row_count,
    int32_t date_key_min, const int32_t* date_year_by_offset, const uint8_t* date_match_by_offset,
    size_t date_year_count, const int32_t* customer_group_code_by_key,
    const uint8_t* customer_match_by_key, size_t customer_key_count,
    const int32_t* supplier_group_code_by_key, const uint8_t* supplier_match_by_key,
    size_t supplier_key_count, int32_t year_min, int32_t year_count, int32_t customer_group_count,
    int32_t supplier_group_count, int64_t* out_revenue_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count) {
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;

  if (!out_revenue_by_group || !out_count_by_group || !selected_count || !uncertain_count)
    return PGACCEL_ERROR;
  if (!valid_group_output3(out_group_capacity, year_count, customer_group_count,
                           supplier_group_count))
    return PGACCEL_ERROR;

  const size_t group_count = static_cast<size_t>(year_count) *
                             static_cast<size_t>(customer_group_count) *
                             static_cast<size_t>(supplier_group_count);
  std::memset(out_revenue_by_group, 0, group_count * sizeof(int64_t));
  std::memset(out_count_by_group, 0, group_count * sizeof(uint32_t));

  if (row_count == 0)
    return PGACCEL_OK;
  if (!valid_i32_col(orderdate_col) || !valid_i32_col(custkey_col) || !valid_i32_col(suppkey_col) ||
      !valid_i32_col(revenue_col))
    return PGACCEL_ERROR;
  if (!date_year_by_offset || !date_match_by_offset || date_year_count == 0 ||
      !customer_group_code_by_key || !customer_match_by_key || customer_key_count == 0 ||
      !supplier_group_code_by_key || !supplier_match_by_key || supplier_key_count == 0)
    return PGACCEL_ERROR;

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  uint32_t* device_revenue_lo = sycl::malloc_device<uint32_t>(group_count, *q);
  uint32_t* device_revenue_hi = sycl::malloc_device<uint32_t>(group_count, *q);
  uint32_t* device_count = sycl::malloc_device<uint32_t>(group_count, *q);
  if (!device_revenue_lo || !device_revenue_hi || !device_count) {
    sycl::free(device_revenue_lo, *q);
    sycl::free(device_revenue_hi, *q);
    sycl::free(device_count, *q);
    return PGACCEL_OOM;
  }

  const auto* orderdate = static_cast<const int32_t*>(orderdate_col.values);
  const auto* custkey = static_cast<const int32_t*>(custkey_col.values);
  const auto* suppkey = static_cast<const int32_t*>(suppkey_col.values);
  const auto* revenue = static_cast<const int32_t*>(revenue_col.values);
  const uint8_t* orderdate_nulls = orderdate_col.nulls;
  const uint8_t* custkey_nulls = custkey_col.nulls;
  const uint8_t* suppkey_nulls = suppkey_col.nulls;
  const uint8_t* revenue_nulls = revenue_col.nulls;

  try {
    q->memset(device_revenue_lo, 0, group_count * sizeof(uint32_t)).wait_and_throw();
    q->memset(device_revenue_hi, 0, group_count * sizeof(uint32_t)).wait_and_throw();
    q->memset(device_count, 0, group_count * sizeof(uint32_t)).wait_and_throw();

    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
         const size_t row = id[0];
         if ((orderdate_nulls && orderdate_nulls[row]) || (custkey_nulls && custkey_nulls[row]) ||
             (suppkey_nulls && suppkey_nulls[row]) || (revenue_nulls && revenue_nulls[row])) {
           return;
         }

         const int32_t orderdate_value = orderdate[row];
         const int64_t date_offset64 =
             static_cast<int64_t>(orderdate_value) - static_cast<int64_t>(date_key_min);
         if (date_offset64 < 0 || static_cast<size_t>(date_offset64) >= date_year_count)
           return;
         const size_t date_offset = static_cast<size_t>(date_offset64);
         if (date_match_by_offset[date_offset] == 0)
           return;
         const int32_t year = date_year_by_offset[date_offset];
         const int32_t year_idx = year - year_min;
         if (year_idx < 0 || year_idx >= year_count)
           return;

         const int32_t cust = custkey[row];
         if (cust < 0 || static_cast<size_t>(cust) >= customer_key_count)
           return;
         if (customer_match_by_key[cust] == 0)
           return;
         const int32_t customer_code = customer_group_code_by_key[cust];
         if (customer_code < 0 || customer_code >= customer_group_count)
           return;

         const int32_t supp = suppkey[row];
         if (supp < 0 || static_cast<size_t>(supp) >= supplier_key_count)
           return;
         if (supplier_match_by_key[supp] == 0)
           return;
         const int32_t supplier_code = supplier_group_code_by_key[supp];
         if (supplier_code < 0 || supplier_code >= supplier_group_count)
           return;

         const size_t group =
             (static_cast<size_t>(year_idx) * static_cast<size_t>(customer_group_count) +
              static_cast<size_t>(customer_code)) *
                 static_cast<size_t>(supplier_group_count) +
             static_cast<size_t>(supplier_code);
         const uint64_t revenue_value = static_cast<uint64_t>(static_cast<uint32_t>(revenue[row]));
         const uint32_t revenue_lo_add = static_cast<uint32_t>(revenue_value);
         const uint32_t revenue_hi_add = static_cast<uint32_t>(revenue_value >> 32);

         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             revenue_lo_ref(device_revenue_lo[group]);
         const uint32_t old_lo = revenue_lo_ref.fetch_add(revenue_lo_add);
         const uint32_t carry = (old_lo + revenue_lo_add < old_lo) ? 1u : 0u;
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             revenue_hi_ref(device_revenue_hi[group]);
         revenue_hi_ref.fetch_add(revenue_hi_add + carry);

         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             count_ref(device_count[group]);
         count_ref.fetch_add(1u);
       });
     }).wait_and_throw();

    std::vector<uint32_t> revenue_lo(group_count);
    std::vector<uint32_t> revenue_hi(group_count);
    q->memcpy(revenue_lo.data(), device_revenue_lo, group_count * sizeof(uint32_t))
        .wait_and_throw();
    q->memcpy(revenue_hi.data(), device_revenue_hi, group_count * sizeof(uint32_t))
        .wait_and_throw();
    q->memcpy(out_count_by_group, device_count, group_count * sizeof(uint32_t)).wait_and_throw();

    size_t selected = 0;
    for (size_t i = 0; i < group_count; ++i) {
      const uint64_t revenue_value =
          (static_cast<uint64_t>(revenue_hi[i]) << 32) | static_cast<uint64_t>(revenue_lo[i]);
      out_revenue_by_group[i] = static_cast<int64_t>(revenue_value);
      selected += out_count_by_group[i];
    }
    *selected_count = selected;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: SSBM Q3 grouped revenue kernel failed: %s\n", e.what());
    sycl::free(device_revenue_lo, *q);
    sycl::free(device_revenue_hi, *q);
    sycl::free(device_count, *q);
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: SSBM Q3 grouped revenue kernel failed (unknown)\n");
    sycl::free(device_revenue_lo, *q);
    sycl::free(device_revenue_hi, *q);
    sycl::free(device_count, *q);
    return PGACCEL_ERROR;
  }

  sycl::free(device_revenue_lo, *q);
  sycl::free(device_revenue_hi, *q);
  sycl::free(device_count, *q);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_expr_template_ssbm_q4_grouped_profit_i64_usm(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col custkey_col,
    pgaccel_expr_usm_col suppkey_col, pgaccel_expr_usm_col partkey_col,
    pgaccel_expr_usm_col revenue_col, pgaccel_expr_usm_col supplycost_col, size_t row_count,
    int32_t date_key_min, const int32_t* date_year_by_offset, const uint8_t* date_match_by_offset,
    size_t date_year_count, const int32_t* customer_group_code_by_key,
    const uint8_t* customer_match_by_key, size_t customer_key_count,
    const int32_t* supplier_group_code_by_key, const uint8_t* supplier_match_by_key,
    size_t supplier_key_count, const int32_t* part_group_code_by_key,
    const uint8_t* part_match_by_key, size_t part_key_count, int32_t group_geo_source,
    int32_t year_min, int32_t year_count, int32_t geo_group_count, int32_t part_group_count,
    uint32_t* scratch_profit_lo, uint32_t* scratch_profit_hi, uint32_t* scratch_count,
    size_t scratch_group_capacity, int64_t* out_profit_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count) {
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;

  if (!out_profit_by_group || !out_count_by_group || !selected_count || !uncertain_count ||
      !scratch_profit_lo || !scratch_profit_hi || !scratch_count)
    return PGACCEL_ERROR;
  if (group_geo_source != 1 && group_geo_source != 2)
    return PGACCEL_ERROR;
  if (!valid_group_output3(out_group_capacity, year_count, geo_group_count, part_group_count))
    return PGACCEL_ERROR;

  const size_t group_count = static_cast<size_t>(year_count) *
                             static_cast<size_t>(geo_group_count) *
                             static_cast<size_t>(part_group_count);
  if (scratch_group_capacity < group_count)
    return PGACCEL_ERROR;
  std::memset(out_profit_by_group, 0, group_count * sizeof(int64_t));
  std::memset(out_count_by_group, 0, group_count * sizeof(uint32_t));

  if (row_count == 0)
    return PGACCEL_OK;
  if (!valid_i32_col(orderdate_col) || !valid_i32_col(custkey_col) || !valid_i32_col(suppkey_col) ||
      !valid_i32_col(partkey_col) || !valid_i32_col(revenue_col) || !valid_i32_col(supplycost_col))
    return PGACCEL_ERROR;
  if (!date_year_by_offset || !date_match_by_offset || date_year_count == 0 ||
      !customer_group_code_by_key || !customer_match_by_key || customer_key_count == 0 ||
      !supplier_group_code_by_key || !supplier_match_by_key || supplier_key_count == 0 ||
      !part_group_code_by_key || !part_match_by_key || part_key_count == 0)
    return PGACCEL_ERROR;

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  uint32_t* device_profit_lo = scratch_profit_lo;
  uint32_t* device_profit_hi = scratch_profit_hi;
  uint32_t* device_count = scratch_count;

  const auto* orderdate = static_cast<const int32_t*>(orderdate_col.values);
  const auto* custkey = static_cast<const int32_t*>(custkey_col.values);
  const auto* suppkey = static_cast<const int32_t*>(suppkey_col.values);
  const auto* partkey = static_cast<const int32_t*>(partkey_col.values);
  const auto* revenue = static_cast<const int32_t*>(revenue_col.values);
  const auto* supplycost = static_cast<const int32_t*>(supplycost_col.values);
  const uint8_t* orderdate_nulls = orderdate_col.nulls;
  const uint8_t* custkey_nulls = custkey_col.nulls;
  const uint8_t* suppkey_nulls = suppkey_col.nulls;
  const uint8_t* partkey_nulls = partkey_col.nulls;
  const uint8_t* revenue_nulls = revenue_col.nulls;
  const uint8_t* supplycost_nulls = supplycost_col.nulls;
  SsbmQ4GroupedProfitKernelParams* params =
      sycl::malloc_shared<SsbmQ4GroupedProfitKernelParams>(1, *q);
  if (!params)
    return PGACCEL_OOM;
  *params = {
      orderdate,
      custkey,
      suppkey,
      partkey,
      revenue,
      supplycost,
      orderdate_nulls,
      custkey_nulls,
      suppkey_nulls,
      partkey_nulls,
      revenue_nulls,
      supplycost_nulls,
      date_key_min,
      date_year_by_offset,
      date_match_by_offset,
      date_year_count,
      customer_group_code_by_key,
      customer_match_by_key,
      customer_key_count,
      supplier_group_code_by_key,
      supplier_match_by_key,
      supplier_key_count,
      part_group_code_by_key,
      part_match_by_key,
      part_key_count,
      group_geo_source,
      year_min,
      year_count,
      geo_group_count,
      part_group_count,
      device_profit_lo,
      device_profit_hi,
      device_count,
  };

  try {
    q->memset(device_profit_lo, 0, group_count * sizeof(uint32_t)).wait_and_throw();
    q->memset(device_profit_hi, 0, group_count * sizeof(uint32_t)).wait_and_throw();
    q->memset(device_count, 0, group_count * sizeof(uint32_t)).wait_and_throw();

    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(row_count), [params](sycl::id<1> id) {
         const size_t row = id[0];
         if ((params->orderdate_nulls && params->orderdate_nulls[row]) ||
             (params->custkey_nulls && params->custkey_nulls[row]) ||
             (params->suppkey_nulls && params->suppkey_nulls[row]) ||
             (params->partkey_nulls && params->partkey_nulls[row]) ||
             (params->revenue_nulls && params->revenue_nulls[row]) ||
             (params->supplycost_nulls && params->supplycost_nulls[row])) {
           return;
         }

         const int64_t date_offset64 = static_cast<int64_t>(params->orderdate[row]) -
                                       static_cast<int64_t>(params->date_key_min);
         if (date_offset64 < 0 || static_cast<size_t>(date_offset64) >= params->date_year_count)
           return;
         const size_t date_offset = static_cast<size_t>(date_offset64);
         if (params->date_match_by_offset[date_offset] == 0)
           return;
         const int32_t year_idx = params->date_year_by_offset[date_offset] - params->year_min;
         if (year_idx < 0 || year_idx >= params->year_count)
           return;

         const int32_t cust = params->custkey[row];
         if (cust < 0 || static_cast<size_t>(cust) >= params->customer_key_count)
           return;
         if (params->customer_match_by_key[cust] == 0)
           return;

         const int32_t supp = params->suppkey[row];
         if (supp < 0 || static_cast<size_t>(supp) >= params->supplier_key_count)
           return;
         if (params->supplier_match_by_key[supp] == 0)
           return;

         const int32_t part = params->partkey[row];
         if (part < 0 || static_cast<size_t>(part) >= params->part_key_count)
           return;
         if (params->part_match_by_key[part] == 0)
           return;
         const int32_t part_code = params->part_group_code_by_key[part];
         if (part_code < 0 || part_code >= params->part_group_count)
           return;

         const int32_t geo_code = params->group_geo_source == 1
                                      ? params->customer_group_code_by_key[cust]
                                      : params->supplier_group_code_by_key[supp];
         if (geo_code < 0 || geo_code >= params->geo_group_count)
           return;

         const size_t group =
             (static_cast<size_t>(year_idx) * static_cast<size_t>(params->geo_group_count) +
              static_cast<size_t>(geo_code)) *
                 static_cast<size_t>(params->part_group_count) +
             static_cast<size_t>(part_code);
         const int64_t profit_signed = static_cast<int64_t>(params->revenue[row]) -
                                       static_cast<int64_t>(params->supplycost[row]);
         const uint64_t profit_value = static_cast<uint64_t>(profit_signed);
         const uint32_t profit_lo_add = static_cast<uint32_t>(profit_value);
         const uint32_t profit_hi_add = static_cast<uint32_t>(profit_value >> 32);

         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             profit_lo_ref(params->device_profit_lo[group]);
         const uint32_t old_lo = profit_lo_ref.fetch_add(profit_lo_add);
         const uint32_t carry = (old_lo + profit_lo_add < old_lo) ? 1u : 0u;
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             profit_hi_ref(params->device_profit_hi[group]);
         profit_hi_ref.fetch_add(profit_hi_add + carry);

         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             count_ref(params->device_count[group]);
         count_ref.fetch_add(1u);
       });
     }).wait_and_throw();

    std::vector<uint32_t> profit_lo(group_count);
    std::vector<uint32_t> profit_hi(group_count);
    q->memcpy(profit_lo.data(), device_profit_lo, group_count * sizeof(uint32_t)).wait_and_throw();
    q->memcpy(profit_hi.data(), device_profit_hi, group_count * sizeof(uint32_t)).wait_and_throw();
    q->memcpy(out_count_by_group, device_count, group_count * sizeof(uint32_t)).wait_and_throw();

    size_t selected = 0;
    for (size_t i = 0; i < group_count; ++i) {
      const uint64_t profit_value =
          (static_cast<uint64_t>(profit_hi[i]) << 32) | static_cast<uint64_t>(profit_lo[i]);
      out_profit_by_group[i] = static_cast<int64_t>(profit_value);
      selected += out_count_by_group[i];
    }
    *selected_count = selected;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: SSBM Q4 grouped profit kernel failed: %s\n", e.what());
    sycl::free(params, *q);
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: SSBM Q4 grouped profit kernel failed (unknown)\n");
    sycl::free(params, *q);
    return PGACCEL_ERROR;
  }

  sycl::free(params, *q);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

}  // namespace

extern "C" size_t pgaccel_expr_template_ssbm_q1_scratch_items(size_t row_count) {
  return partial_item_count(row_count);
}

extern "C" pgaccel_status pgaccel_expr_template_ssbm_q1_revenue_i64_usm_scratch(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col discount_col,
    pgaccel_expr_usm_col quantity_col, pgaccel_expr_usm_col extendedprice_col, size_t row_count,
    int32_t orderdate_lo, int32_t orderdate_hi, const int32_t* orderdate_keys,
    size_t orderdate_key_count, int32_t discount_lo, int32_t discount_hi, int32_t quantity_lo,
    int32_t quantity_hi, int64_t* scratch_revenue_a, int64_t* scratch_count_a,
    int64_t* scratch_revenue_b, int64_t* scratch_count_b, size_t scratch_item_capacity,
    int64_t* out_sum, size_t* selected_count, size_t* uncertain_count) {
  return run_ssbm_q1_revenue_i64_scratch(
      orderdate_col, discount_col, quantity_col, extendedprice_col, row_count, orderdate_lo,
      orderdate_hi, orderdate_keys, orderdate_key_count, discount_lo, discount_hi, quantity_lo,
      quantity_hi, scratch_revenue_a, scratch_count_a, scratch_revenue_b, scratch_count_b,
      scratch_item_capacity, out_sum, selected_count, uncertain_count);
}

extern "C" pgaccel_status pgaccel_expr_template_ssbm_q1_revenue_i64_usm(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col discount_col,
    pgaccel_expr_usm_col quantity_col, pgaccel_expr_usm_col extendedprice_col, size_t row_count,
    int32_t orderdate_lo, int32_t orderdate_hi, const int32_t* orderdate_keys,
    size_t orderdate_key_count, int32_t discount_lo, int32_t discount_hi, int32_t quantity_lo,
    int32_t quantity_hi, int64_t* out_sum, size_t* selected_count, size_t* uncertain_count) {
  if (out_sum)
    *out_sum = 0;
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;

  if (!out_sum || !selected_count || !uncertain_count)
    return PGACCEL_ERROR;
  if (row_count == 0)
    return PGACCEL_OK;

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t item_count = partial_item_count(row_count);
  int64_t* revenue_a = sycl::malloc_device<int64_t>(item_count, *q);
  int64_t* count_a = sycl::malloc_device<int64_t>(item_count, *q);
  int64_t* revenue_b = sycl::malloc_device<int64_t>(item_count, *q);
  int64_t* count_b = sycl::malloc_device<int64_t>(item_count, *q);
  if (!revenue_a || !count_a || !revenue_b || !count_b) {
    sycl::free(revenue_a, *q);
    sycl::free(count_a, *q);
    sycl::free(revenue_b, *q);
    sycl::free(count_b, *q);
    return PGACCEL_OOM;
  }

  pgaccel_status status = run_ssbm_q1_revenue_i64_scratch(
      orderdate_col, discount_col, quantity_col, extendedprice_col, row_count, orderdate_lo,
      orderdate_hi, orderdate_keys, orderdate_key_count, discount_lo, discount_hi, quantity_lo,
      quantity_hi, revenue_a, count_a, revenue_b, count_b, item_count, out_sum, selected_count,
      uncertain_count);

  sycl::free(revenue_a, *q);
  sycl::free(count_a, *q);
  sycl::free(revenue_b, *q);
  sycl::free(count_b, *q);
  return status;
}

extern "C" pgaccel_status pgaccel_expr_template_ssbm_q2_grouped_revenue_i64_usm(
    pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col partkey_col,
    pgaccel_expr_usm_col suppkey_col, pgaccel_expr_usm_col revenue_col, size_t row_count,
    int32_t date_key_min, const int32_t* date_year_by_offset, size_t date_year_count,
    const int32_t* part_brand_code_by_key, const uint8_t* part_match_by_key, size_t part_key_count,
    const uint8_t* supplier_match_by_key, size_t supplier_key_count, int32_t year_min,
    int32_t year_count, int32_t brand_count, int64_t* out_revenue_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count) {
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;

  if (!out_revenue_by_group || !out_count_by_group || !selected_count || !uncertain_count)
    return PGACCEL_ERROR;
  if (!valid_group_output(out_group_capacity, year_count, brand_count))
    return PGACCEL_ERROR;

  const size_t group_count = static_cast<size_t>(year_count) * static_cast<size_t>(brand_count);
  std::memset(out_revenue_by_group, 0, group_count * sizeof(int64_t));
  std::memset(out_count_by_group, 0, group_count * sizeof(uint32_t));

  if (row_count == 0)
    return PGACCEL_OK;
  if (!valid_i32_col(orderdate_col) || !valid_i32_col(partkey_col) || !valid_i32_col(suppkey_col) ||
      !valid_i32_col(revenue_col))
    return PGACCEL_ERROR;
  if (!date_year_by_offset || date_year_count == 0 || !part_brand_code_by_key ||
      !part_match_by_key || part_key_count == 0 || !supplier_match_by_key ||
      supplier_key_count == 0)
    return PGACCEL_ERROR;

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  uint32_t* device_revenue_lo = sycl::malloc_device<uint32_t>(group_count, *q);
  uint32_t* device_revenue_hi = sycl::malloc_device<uint32_t>(group_count, *q);
  uint32_t* device_count = sycl::malloc_device<uint32_t>(group_count, *q);
  if (!device_revenue_lo || !device_revenue_hi || !device_count) {
    sycl::free(device_revenue_lo, *q);
    sycl::free(device_revenue_hi, *q);
    sycl::free(device_count, *q);
    return PGACCEL_OOM;
  }

  const auto* orderdate = static_cast<const int32_t*>(orderdate_col.values);
  const auto* partkey = static_cast<const int32_t*>(partkey_col.values);
  const auto* suppkey = static_cast<const int32_t*>(suppkey_col.values);
  const auto* revenue = static_cast<const int32_t*>(revenue_col.values);
  const uint8_t* orderdate_nulls = orderdate_col.nulls;
  const uint8_t* partkey_nulls = partkey_col.nulls;
  const uint8_t* suppkey_nulls = suppkey_col.nulls;
  const uint8_t* revenue_nulls = revenue_col.nulls;

  try {
    q->memset(device_revenue_lo, 0, group_count * sizeof(uint32_t)).wait_and_throw();
    q->memset(device_revenue_hi, 0, group_count * sizeof(uint32_t)).wait_and_throw();
    q->memset(device_count, 0, group_count * sizeof(uint32_t)).wait_and_throw();

    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
         const size_t row = id[0];
         if ((orderdate_nulls && orderdate_nulls[row]) || (partkey_nulls && partkey_nulls[row]) ||
             (suppkey_nulls && suppkey_nulls[row]) || (revenue_nulls && revenue_nulls[row])) {
           return;
         }

         const int32_t orderdate_value = orderdate[row];
         const int64_t date_offset64 =
             static_cast<int64_t>(orderdate_value) - static_cast<int64_t>(date_key_min);
         if (date_offset64 < 0 || static_cast<size_t>(date_offset64) >= date_year_count)
           return;
         const int32_t year = date_year_by_offset[static_cast<size_t>(date_offset64)];
         const int32_t year_idx = year - year_min;
         if (year_idx < 0 || year_idx >= year_count)
           return;

         const int32_t part = partkey[row];
         if (part < 0 || static_cast<size_t>(part) >= part_key_count)
           return;
         if (part_match_by_key[part] == 0)
           return;
         const int32_t brand_code = part_brand_code_by_key[part];
         if (brand_code < 0 || brand_code >= brand_count)
           return;

         const int32_t supp = suppkey[row];
         if (supp < 0 || static_cast<size_t>(supp) >= supplier_key_count)
           return;
         if (supplier_match_by_key[supp] == 0)
           return;

         const size_t group = static_cast<size_t>(year_idx) * static_cast<size_t>(brand_count) +
                              static_cast<size_t>(brand_code);
         const uint64_t revenue_value = static_cast<uint64_t>(static_cast<uint32_t>(revenue[row]));
         const uint32_t revenue_lo_add = static_cast<uint32_t>(revenue_value);
         const uint32_t revenue_hi_add = static_cast<uint32_t>(revenue_value >> 32);

         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             revenue_lo_ref(device_revenue_lo[group]);
         const uint32_t old_lo = revenue_lo_ref.fetch_add(revenue_lo_add);
         const uint32_t carry = (old_lo + revenue_lo_add < old_lo) ? 1u : 0u;
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             revenue_hi_ref(device_revenue_hi[group]);
         revenue_hi_ref.fetch_add(revenue_hi_add + carry);

         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             count_ref(device_count[group]);
         count_ref.fetch_add(1u);
       });
     }).wait_and_throw();

    std::vector<uint32_t> revenue_lo(group_count);
    std::vector<uint32_t> revenue_hi(group_count);
    q->memcpy(revenue_lo.data(), device_revenue_lo, group_count * sizeof(uint32_t))
        .wait_and_throw();
    q->memcpy(revenue_hi.data(), device_revenue_hi, group_count * sizeof(uint32_t))
        .wait_and_throw();
    q->memcpy(out_count_by_group, device_count, group_count * sizeof(uint32_t)).wait_and_throw();

    size_t selected = 0;
    for (size_t i = 0; i < group_count; ++i) {
      const uint64_t revenue_value =
          (static_cast<uint64_t>(revenue_hi[i]) << 32) | static_cast<uint64_t>(revenue_lo[i]);
      out_revenue_by_group[i] = static_cast<int64_t>(revenue_value);
      selected += out_count_by_group[i];
    }
    *selected_count = selected;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: SSBM Q2 grouped revenue kernel failed: %s\n", e.what());
    sycl::free(device_revenue_lo, *q);
    sycl::free(device_revenue_hi, *q);
    sycl::free(device_count, *q);
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: SSBM Q2 grouped revenue kernel failed (unknown)\n");
    sycl::free(device_revenue_lo, *q);
    sycl::free(device_revenue_hi, *q);
    sycl::free(device_count, *q);
    return PGACCEL_ERROR;
  }

  sycl::free(device_revenue_lo, *q);
  sycl::free(device_revenue_hi, *q);
  sycl::free(device_count, *q);
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_simple_sum_count_usm(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col, size_t row_count,
    int32_t group_min, int32_t group_count, double* scratch_sum, uint32_t* scratch_count,
    double* scratch_partial_sum, uint32_t* scratch_partial_count, size_t scratch_partial_capacity,
    double* out_sum_by_group, uint32_t* out_count_by_group, size_t out_group_capacity,
    size_t* selected_count, size_t* uncertain_count) {
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;
  const auto fail = [&](const char* reason,
                        pgaccel_status status = PGACCEL_ERROR) -> pgaccel_status {
    std::fprintf(stderr,
                 "pgaccel: resident dense grouped f64 simple sum/count preflight failed: "
                 "reason=%s row_count=%zu group_min=%d group_count=%d group_values=%p "
                 "group_nulls=%p group_type=%d value_values=%p value_nulls=%p value_type=%d "
                 "scratch_sum=%p scratch_count=%p partial_sum=%p partial_count=%p "
                 "partial_capacity=%zu out_sum=%p out_count=%p out_group_capacity=%zu "
                 "selected_count=%p uncertain_count=%p\n",
                 reason, row_count, group_min, group_count, group_col.values, group_col.nulls,
                 static_cast<int>(group_col.type), value_col.values, value_col.nulls,
                 static_cast<int>(value_col.type), static_cast<void*>(scratch_sum),
                 static_cast<void*>(scratch_count), static_cast<void*>(scratch_partial_sum),
                 static_cast<void*>(scratch_partial_count), scratch_partial_capacity,
                 static_cast<void*>(out_sum_by_group), static_cast<void*>(out_count_by_group),
                 out_group_capacity, static_cast<void*>(selected_count),
                 static_cast<void*>(uncertain_count));
    std::fflush(stderr);
    return status;
  };

  if (!selected_count || !uncertain_count)
    return fail("missing_count_output");
  if (row_count == 0)
    return PGACCEL_OK;
  if (group_count <= 0)
    return fail("invalid_group_count");
  if (!valid_i32_col(group_col) || !valid_f64_col(value_col))
    return fail("invalid_input_column");
  const size_t groups = static_cast<size_t>(group_count);
  if (groups > DENSE_GROUP_ONE_PASS_MAX_GROUPS)
    return fail("unsupported_group_count");
  if (!scratch_sum || !scratch_count || !out_sum_by_group || !out_count_by_group ||
      out_group_capacity < groups)
    return fail("invalid_output");
  if (row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return fail("row_count_exceeds_u32");

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return fail("no_device_queue", PGACCEL_ERROR_NO_DEVICE);

  const size_t row_block_count =
      (row_count + DENSE_GROUP_ONE_PASS_BLOCK_ROWS - 1) / DENSE_GROUP_ONE_PASS_BLOCK_ROWS;
  if (row_block_count != 0 && groups > std::numeric_limits<size_t>::max() / row_block_count)
    return fail("dense_grouped_simple_partial_size_overflow");
  const size_t partial_items = row_block_count * groups;
  const bool use_cached_partials =
      scratch_partial_capacity >= partial_items && scratch_partial_sum && scratch_partial_count;
  double* partial_sum =
      use_cached_partials ? scratch_partial_sum : sycl::malloc_device<double>(partial_items, *q);
  uint32_t* partial_count = use_cached_partials ? scratch_partial_count
                                                : sycl::malloc_device<uint32_t>(partial_items, *q);
  if (!partial_sum || !partial_count) {
    if (!use_cached_partials && partial_sum)
      sycl::free(partial_sum, *q);
    if (!use_cached_partials && partial_count)
      sycl::free(partial_count, *q);
    return fail("dense_grouped_simple_partial_oom", PGACCEL_ERROR_OOM);
  }
  auto cleanup_partials = [&]() {
    if (!use_cached_partials) {
      sycl::free(partial_sum, *q);
      sycl::free(partial_count, *q);
    }
  };

  const auto* group_values = static_cast<const int32_t*>(group_col.values);
  const auto* values = static_cast<const double*>(value_col.values);
  const uint8_t* group_nulls = group_col.nulls;
  const uint8_t* value_nulls = value_col.nulls;
  constexpr size_t local_size = DENSE_GROUP_SIMPLE_WIDE_LOCAL_SIZE;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<double, 1> local_sum(sycl::range<1>(local_size * groups), h);
       sycl::local_accessor<uint32_t, 1> local_count(sycl::range<1>(local_size * groups), h);
       h.parallel_for(sycl::nd_range<1>(sycl::range<1>(row_block_count * local_size),
                                        sycl::range<1>(local_size)),
                      [=](sycl::nd_item<1> item) {
                        const size_t row_block_idx = item.get_group(0);
                        const size_t local_id = item.get_local_id(0);
                        const size_t local_slots = local_size * groups;
                        const size_t row_start = row_block_idx * DENSE_GROUP_ONE_PASS_BLOCK_ROWS;
                        const size_t row_end =
                            sycl::min(row_start + DENSE_GROUP_ONE_PASS_BLOCK_ROWS, row_count);

                        for (size_t slot = local_id; slot < local_slots; slot += local_size) {
                          local_sum[slot] = 0.0;
                          local_count[slot] = 0;
                        }
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t row = row_start + local_id; row < row_end; row += local_size) {
                          if (group_nulls && group_nulls[row])
                            continue;
                          if (value_nulls && value_nulls[row])
                            continue;
                          const int64_t group_idx64 = static_cast<int64_t>(group_values[row]) -
                                                      static_cast<int64_t>(group_min);
                          if (group_idx64 < 0 || group_idx64 >= static_cast<int64_t>(groups))
                            continue;
                          const size_t group_idx = static_cast<size_t>(group_idx64);
                          const size_t slot = local_id * groups + group_idx;
                          local_sum[slot] += values[row];
                          local_count[slot] += 1;
                        }
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t group_idx = local_id; group_idx < groups;
                             group_idx += local_size) {
                          double sum = 0.0;
                          uint32_t count = 0;
                          for (size_t lane = 0; lane < local_size; ++lane) {
                            const size_t slot = lane * groups + group_idx;
                            sum += local_sum[slot];
                            count += local_count[slot];
                          }
                          const size_t partial_idx = row_block_idx * groups + group_idx;
                          partial_sum[partial_idx] = sum;
                          partial_count[partial_idx] = count;
                        }
                      });
     }).wait_and_throw();

    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<double, 1> local_sum(
           sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
       sycl::local_accessor<uint32_t, 1> local_count(
           sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
       h.parallel_for(
           sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE),
                             sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE)),
           [=](sycl::nd_item<1> item) {
             const size_t group_idx = item.get_group(0);
             const size_t local_id = item.get_local_id(0);
             double sum = 0.0;
             uint32_t count = 0;
             for (size_t block_idx = local_id; block_idx < row_block_count;
                  block_idx += DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE) {
               const size_t partial_idx = block_idx * groups + group_idx;
               sum += partial_sum[partial_idx];
               count += partial_count[partial_idx];
             }
             local_sum[local_id] = sum;
             local_count[local_id] = count;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE / 2; stride > 0;
                  stride /= 2) {
               if (local_id < stride) {
                 local_sum[local_id] += local_sum[local_id + stride];
                 local_count[local_id] += local_count[local_id + stride];
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (local_id == 0) {
               scratch_sum[group_idx] = local_sum[0];
               scratch_count[group_idx] = local_count[0];
             }
           });
     }).wait_and_throw();

    q->memcpy(out_sum_by_group, scratch_sum, sizeof(double) * groups).wait_and_throw();
    q->memcpy(out_count_by_group, scratch_count, sizeof(uint32_t) * groups).wait_and_throw();
    size_t selected_host = 0;
    for (size_t i = 0; i < groups; ++i)
      selected_host += static_cast<size_t>(out_count_by_group[i]);
    *selected_count = selected_host;
    *uncertain_count = 0;
    cleanup_partials();
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident dense grouped f64 simple sum/count kernel failed: %s\n",
                 e.what());
    cleanup_partials();
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr,
                 "pgaccel: resident dense grouped f64 simple sum/count kernel failed (unknown)\n");
    cleanup_partials();
    return PGACCEL_ERROR;
  }
}

extern "C" pgaccel_status
pgaccel_expr_template_reduce_f64_usm(pgaccel_expr_usm_col value_col, uint32_t aggregate_mask,
                                     size_t row_count, double* out_sum, double* out_min,
                                     double* out_max, double* out_sumsq, uint64_t* out_count,
                                     size_t* selected_count, size_t* uncertain_count) {
  if (out_sum)
    *out_sum = 0.0;
  if (out_min)
    *out_min = 0.0;
  if (out_max)
    *out_max = 0.0;
  if (out_sumsq)
    *out_sumsq = 0.0;
  if (out_count)
    *out_count = 0;
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;

  const auto fail = [&](const char* reason,
                        pgaccel_status status = PGACCEL_ERROR) -> pgaccel_status {
    std::fprintf(stderr,
                 "pgaccel: resident scalar f64 reduce preflight failed: reason=%s "
                 "row_count=%zu aggregate_mask=%u value_values=%p value_nulls=%p "
                 "value_type=%d out_sum=%p out_min=%p out_max=%p out_sumsq=%p "
                 "out_count=%p selected_count=%p uncertain_count=%p\n",
                 reason, row_count, aggregate_mask, value_col.values, value_col.nulls,
                 static_cast<int>(value_col.type), static_cast<void*>(out_sum),
                 static_cast<void*>(out_min), static_cast<void*>(out_max),
                 static_cast<void*>(out_sumsq), static_cast<void*>(out_count),
                 static_cast<void*>(selected_count), static_cast<void*>(uncertain_count));
    std::fflush(stderr);
    return status;
  };

  if (!selected_count || !uncertain_count)
    return fail("missing_count_output");
  if (aggregate_mask == 0 || (aggregate_mask & ~RESIDENT_F64_REDUCE_AGG_ALL) != 0)
    return fail("invalid_aggregate_mask");

  const bool need_sum = (aggregate_mask & DENSE_GROUP_AGG_SUM) != 0;
  const bool need_min = (aggregate_mask & DENSE_GROUP_AGG_MIN) != 0;
  const bool need_max = (aggregate_mask & DENSE_GROUP_AGG_MAX) != 0;
  const bool need_count = (aggregate_mask & DENSE_GROUP_AGG_COUNT) != 0;
  const bool need_sumsq = (aggregate_mask & DENSE_GROUP_AGG_SUMSQ) != 0;
  if ((need_sum && !out_sum) || (need_min && !out_min) || (need_max && !out_max) ||
      (need_sumsq && !out_sumsq) || (need_count && !out_count))
    return fail("missing_output_lane");
  if (row_count == 0)
    return PGACCEL_OK;
  if (!valid_f64_col(value_col))
    return fail("invalid_input_column");

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return fail("no_device_queue", PGACCEL_ERROR_NO_DEVICE);

  const size_t row_block_count =
      (row_count + DENSE_GROUP_ONE_PASS_BLOCK_ROWS - 1) / DENSE_GROUP_ONE_PASS_BLOCK_ROWS;
  if (row_block_count == 0)
    return PGACCEL_OK;

  double* partial_sum = nullptr;
  double* partial_min = nullptr;
  double* partial_max = nullptr;
  double* partial_sumsq = nullptr;
  uint64_t* partial_count = nullptr;
  auto cleanup_partials = [&]() {
    if (partial_sum)
      sycl::free(partial_sum, *q);
    if (partial_min)
      sycl::free(partial_min, *q);
    if (partial_max)
      sycl::free(partial_max, *q);
    if (partial_sumsq)
      sycl::free(partial_sumsq, *q);
    if (partial_count)
      sycl::free(partial_count, *q);
  };

  try {
    partial_sum = sycl::malloc_device<double>(row_block_count, *q);
    partial_min = sycl::malloc_device<double>(row_block_count, *q);
    partial_max = sycl::malloc_device<double>(row_block_count, *q);
    partial_sumsq = sycl::malloc_device<double>(row_block_count, *q);
    partial_count = sycl::malloc_device<uint64_t>(row_block_count, *q);
    if (!partial_sum || !partial_min || !partial_max || !partial_sumsq || !partial_count) {
      cleanup_partials();
      return fail("scalar_reduce_partial_oom", PGACCEL_ERROR_OOM);
    }

    const auto* values = static_cast<const double*>(value_col.values);
    const uint8_t* value_nulls = value_col.nulls;
    constexpr size_t local_size = DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE;

    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<double, 1> local_sum(sycl::range<1>(local_size), h);
       sycl::local_accessor<double, 1> local_min(sycl::range<1>(local_size), h);
       sycl::local_accessor<double, 1> local_max(sycl::range<1>(local_size), h);
       sycl::local_accessor<double, 1> local_sumsq(sycl::range<1>(local_size), h);
       sycl::local_accessor<uint64_t, 1> local_count(sycl::range<1>(local_size), h);
       h.parallel_for(sycl::nd_range<1>(sycl::range<1>(row_block_count * local_size),
                                        sycl::range<1>(local_size)),
                      [=](sycl::nd_item<1> item) {
                        const size_t block_idx = item.get_group(0);
                        const size_t local_id = item.get_local_id(0);
                        const size_t row_start = block_idx * DENSE_GROUP_ONE_PASS_BLOCK_ROWS;
                        const size_t row_end =
                            sycl::min(row_start + DENSE_GROUP_ONE_PASS_BLOCK_ROWS, row_count);

                        double sum = 0.0;
                        double min_value = std::numeric_limits<double>::max();
                        double max_value = std::numeric_limits<double>::lowest();
                        double sumsq = 0.0;
                        uint64_t count = 0;
                        for (size_t row = row_start + local_id; row < row_end; row += local_size) {
                          if (value_nulls && value_nulls[row])
                            continue;
                          const double value = values[row];
                          if (need_sum)
                            sum += value;
                          if (need_min)
                            min_value = sycl::fmin(min_value, value);
                          if (need_max)
                            max_value = sycl::fmax(max_value, value);
                          if (need_sumsq)
                            sumsq += value * value;
                          count += 1;
                        }

                        local_sum[local_id] = sum;
                        local_min[local_id] = min_value;
                        local_max[local_id] = max_value;
                        local_sumsq[local_id] = sumsq;
                        local_count[local_id] = count;
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t stride = local_size / 2; stride > 0; stride /= 2) {
                          if (local_id < stride) {
                            if (need_sum)
                              local_sum[local_id] += local_sum[local_id + stride];
                            if (need_min)
                              local_min[local_id] =
                                  sycl::fmin(local_min[local_id], local_min[local_id + stride]);
                            if (need_max)
                              local_max[local_id] =
                                  sycl::fmax(local_max[local_id], local_max[local_id + stride]);
                            if (need_sumsq)
                              local_sumsq[local_id] += local_sumsq[local_id + stride];
                            local_count[local_id] += local_count[local_id + stride];
                          }
                          item.barrier(sycl::access::fence_space::local_space);
                        }

                        if (local_id == 0) {
                          partial_sum[block_idx] = local_sum[0];
                          partial_min[block_idx] = local_min[0];
                          partial_max[block_idx] = local_max[0];
                          partial_sumsq[block_idx] = local_sumsq[0];
                          partial_count[block_idx] = local_count[0];
                        }
                      });
     }).wait_and_throw();

    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<double, 1> local_sum(sycl::range<1>(local_size), h);
       sycl::local_accessor<double, 1> local_min(sycl::range<1>(local_size), h);
       sycl::local_accessor<double, 1> local_max(sycl::range<1>(local_size), h);
       sycl::local_accessor<double, 1> local_sumsq(sycl::range<1>(local_size), h);
       sycl::local_accessor<uint64_t, 1> local_count(sycl::range<1>(local_size), h);
       h.parallel_for(sycl::nd_range<1>(sycl::range<1>(local_size), sycl::range<1>(local_size)),
                      [=](sycl::nd_item<1> item) {
                        const size_t local_id = item.get_local_id(0);
                        double sum = 0.0;
                        double min_value = std::numeric_limits<double>::max();
                        double max_value = std::numeric_limits<double>::lowest();
                        double sumsq = 0.0;
                        uint64_t count = 0;
                        for (size_t block_idx = local_id; block_idx < row_block_count;
                             block_idx += local_size) {
                          if (need_sum)
                            sum += partial_sum[block_idx];
                          if (need_min)
                            min_value = sycl::fmin(min_value, partial_min[block_idx]);
                          if (need_max)
                            max_value = sycl::fmax(max_value, partial_max[block_idx]);
                          if (need_sumsq)
                            sumsq += partial_sumsq[block_idx];
                          count += partial_count[block_idx];
                        }

                        local_sum[local_id] = sum;
                        local_min[local_id] = min_value;
                        local_max[local_id] = max_value;
                        local_sumsq[local_id] = sumsq;
                        local_count[local_id] = count;
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t stride = local_size / 2; stride > 0; stride /= 2) {
                          if (local_id < stride) {
                            if (need_sum)
                              local_sum[local_id] += local_sum[local_id + stride];
                            if (need_min)
                              local_min[local_id] =
                                  sycl::fmin(local_min[local_id], local_min[local_id + stride]);
                            if (need_max)
                              local_max[local_id] =
                                  sycl::fmax(local_max[local_id], local_max[local_id + stride]);
                            if (need_sumsq)
                              local_sumsq[local_id] += local_sumsq[local_id + stride];
                            local_count[local_id] += local_count[local_id + stride];
                          }
                          item.barrier(sycl::access::fence_space::local_space);
                        }

                        if (local_id == 0) {
                          partial_sum[0] = local_sum[0];
                          partial_min[0] = local_min[0];
                          partial_max[0] = local_max[0];
                          partial_sumsq[0] = local_sumsq[0];
                          partial_count[0] = local_count[0];
                        }
                      });
     }).wait_and_throw();

    uint64_t count_host = 0;
    q->memcpy(&count_host, partial_count, sizeof(uint64_t)).wait_and_throw();
    if (need_sum)
      q->memcpy(out_sum, partial_sum, sizeof(double)).wait_and_throw();
    if (need_min)
      q->memcpy(out_min, partial_min, sizeof(double)).wait_and_throw();
    if (need_max)
      q->memcpy(out_max, partial_max, sizeof(double)).wait_and_throw();
    if (need_sumsq)
      q->memcpy(out_sumsq, partial_sumsq, sizeof(double)).wait_and_throw();
    if (need_count)
      *out_count = count_host;
    *selected_count = static_cast<size_t>(count_host);
    *uncertain_count = 0;
    cleanup_partials();
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident scalar f64 reduce kernel failed: %s\n", e.what());
    cleanup_partials();
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: resident scalar f64 reduce kernel failed (unknown)\n");
    cleanup_partials();
    return PGACCEL_ERROR;
  }
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_stats_pair_usm(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, size_t row_count, int32_t group_min, int32_t group_count,
    double* scratch_sum, double* scratch_sumsq, double* scratch_rhs_sum, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_len, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_sumsq_by_group, uint32_t* out_count_by_group,
    double* out_rhs_sum_by_group, uint32_t* out_rhs_count_by_group, size_t out_group_capacity,
    size_t* selected_count, size_t* uncertain_count) {
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;

  const auto fail = [&](const char* reason,
                        pgaccel_status status = PGACCEL_ERROR) -> pgaccel_status {
    std::fprintf(stderr,
                 "pgaccel: resident dense grouped f64 stats-pair preflight failed: "
                 "reason=%s row_count=%zu group_min=%d group_count=%d group_values=%p "
                 "group_nulls=%p group_type=%d value_values=%p value_nulls=%p value_type=%d "
                 "rhs_values=%p rhs_nulls=%p rhs_type=%d scratch_sum=%p scratch_sumsq=%p "
                 "scratch_rhs_sum=%p scratch_count=%p scratch_group_start=%p "
                 "scratch_group_len=%p scratch_group_capacity=%zu scratch_sorted_group=%p "
                 "scratch_row_index=%p scratch_row_capacity=%zu out_sum=%p out_sumsq=%p "
                 "out_count=%p out_rhs_sum=%p out_rhs_count=%p out_group_capacity=%zu "
                 "selected_count=%p uncertain_count=%p\n",
                 reason, row_count, group_min, group_count, group_col.values, group_col.nulls,
                 static_cast<int>(group_col.type), value_col.values, value_col.nulls,
                 static_cast<int>(value_col.type), value_rhs_col.values, value_rhs_col.nulls,
                 static_cast<int>(value_rhs_col.type), static_cast<void*>(scratch_sum),
                 static_cast<void*>(scratch_sumsq), static_cast<void*>(scratch_rhs_sum),
                 static_cast<void*>(scratch_count), static_cast<void*>(scratch_group_start),
                 static_cast<void*>(scratch_group_len), scratch_group_capacity,
                 static_cast<void*>(scratch_sorted_group), static_cast<void*>(scratch_row_index),
                 scratch_row_capacity, static_cast<void*>(out_sum_by_group),
                 static_cast<void*>(out_sumsq_by_group), static_cast<void*>(out_count_by_group),
                 static_cast<void*>(out_rhs_sum_by_group),
                 static_cast<void*>(out_rhs_count_by_group), out_group_capacity,
                 static_cast<void*>(selected_count), static_cast<void*>(uncertain_count));
    std::fflush(stderr);
    return status;
  };

  if (!selected_count || !uncertain_count)
    return fail("missing_count_output");
  if (row_count == 0)
    return PGACCEL_OK;
  if (group_count <= 0)
    return fail("invalid_group_count");
  if (!valid_i32_col(group_col) || !valid_f64_col(value_col) || !valid_f64_col(value_rhs_col))
    return fail("invalid_input_column");
  const size_t groups = static_cast<size_t>(group_count);
  if (!scratch_sum || !scratch_sumsq || !scratch_rhs_sum || !scratch_count ||
      !scratch_group_start || !scratch_group_len || scratch_group_capacity < groups ||
      !scratch_sorted_group || !scratch_row_index || scratch_row_capacity < row_count ||
      !out_sum_by_group || !out_sumsq_by_group || !out_count_by_group || !out_rhs_sum_by_group ||
      !out_rhs_count_by_group || out_group_capacity < groups)
    return fail("invalid_output");
  if (row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return fail("row_count_exceeds_u32");

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return fail("no_device_queue", PGACCEL_ERROR_NO_DEVICE);

  try {
    const auto* group_values = static_cast<const int32_t*>(group_col.values);
    const auto* values = static_cast<const double*>(value_col.values);
    const auto* rhs_values = static_cast<const double*>(value_rhs_col.values);
    const uint8_t* group_nulls = group_col.nulls;
    const uint8_t* value_nulls = value_col.nulls;
    const uint8_t* rhs_nulls = value_rhs_col.nulls;

    q->parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const uint32_t row = static_cast<uint32_t>(id[0]);
       scratch_sorted_group[row] = group_values[row];
       scratch_row_index[row] = row;
     }).wait_and_throw();

    const pgaccel_status sort_status =
        pgaccel_sort_kv_i32(scratch_sorted_group, scratch_row_index, row_count);
    if (sort_status != PGACCEL_OK)
      return fail("sort_kv_i32_failed");

    const uint32_t no_group = std::numeric_limits<uint32_t>::max();
    q->fill(scratch_group_start, no_group, groups).wait_and_throw();
    q->memset(scratch_group_len, 0, sizeof(uint32_t) * groups).wait_and_throw();

    q->parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const uint32_t pos = static_cast<uint32_t>(id[0]);
       const int32_t key = scratch_sorted_group[pos];
       if (pos > 0 && scratch_sorted_group[pos - 1] == key)
         return;
       const int64_t group_idx64 = static_cast<int64_t>(key) - static_cast<int64_t>(group_min);
       if (group_idx64 < 0 || group_idx64 >= static_cast<int64_t>(group_count))
         return;

       uint32_t end = pos + 1;
       while (end < row_count && scratch_sorted_group[end] == key)
         ++end;

       const uint32_t group_idx = static_cast<uint32_t>(group_idx64);
       scratch_group_start[group_idx] = pos;
       scratch_group_len[group_idx] = end - pos;
     }).wait_and_throw();

    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<double, 1> local_sum(sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE), h);
       sycl::local_accessor<double, 1> local_sumsq(sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE), h);
       sycl::local_accessor<uint32_t, 1> local_count(sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE),
                                                     h);
       sycl::local_accessor<double, 1> local_rhs_sum(sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE),
                                                     h);
       sycl::local_accessor<uint32_t, 1> local_rhs_count(
           sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE), h);
       h.parallel_for(
           sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_HIGH_LOCAL_SIZE),
                             sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE)),
           [=](sycl::nd_item<1> item) {
             const size_t group_idx = item.get_group(0);
             const size_t local_id = item.get_local_id(0);
             const uint32_t segment_start = scratch_group_start[group_idx];
             const uint32_t segment_len = scratch_group_len[group_idx];
             double sum = 0.0;
             double sumsq = 0.0;
             uint32_t count = 0;
             double rhs_sum = 0.0;
             uint32_t rhs_count = 0;

             if (segment_start != no_group && segment_len != 0) {
               for (uint32_t offset = static_cast<uint32_t>(local_id); offset < segment_len;
                    offset += static_cast<uint32_t>(DENSE_GROUP_HIGH_LOCAL_SIZE)) {
                 const uint32_t sorted_pos = segment_start + offset;
                 const uint32_t row = scratch_row_index[sorted_pos];
                 if (group_nulls && group_nulls[row])
                   continue;
                 if (!(value_nulls && value_nulls[row])) {
                   const double value = values[row];
                   sum += value;
                   sumsq += value * value;
                   count += 1;
                 }
                 if (!(rhs_nulls && rhs_nulls[row])) {
                   rhs_sum += rhs_values[row];
                   rhs_count += 1;
                 }
               }
             }

             local_sum[local_id] = sum;
             local_sumsq[local_id] = sumsq;
             local_count[local_id] = count;
             local_rhs_sum[local_id] = rhs_sum;
             local_rhs_count[local_id] = rhs_count;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = DENSE_GROUP_HIGH_LOCAL_SIZE / 2; stride > 0; stride /= 2) {
               if (local_id < stride) {
                 local_sum[local_id] += local_sum[local_id + stride];
                 local_sumsq[local_id] += local_sumsq[local_id + stride];
                 local_count[local_id] += local_count[local_id + stride];
                 local_rhs_sum[local_id] += local_rhs_sum[local_id + stride];
                 local_rhs_count[local_id] += local_rhs_count[local_id + stride];
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (local_id == 0) {
               scratch_sum[group_idx] = local_sum[0];
               scratch_sumsq[group_idx] = local_sumsq[0];
               scratch_count[group_idx] = local_count[0];
               scratch_rhs_sum[group_idx] = local_rhs_sum[0];
               scratch_group_len[group_idx] = local_rhs_count[0];
             }
           });
     }).wait_and_throw();

    q->memcpy(out_sum_by_group, scratch_sum, sizeof(double) * groups).wait_and_throw();
    q->memcpy(out_sumsq_by_group, scratch_sumsq, sizeof(double) * groups).wait_and_throw();
    q->memcpy(out_count_by_group, scratch_count, sizeof(uint32_t) * groups).wait_and_throw();
    q->memcpy(out_rhs_sum_by_group, scratch_rhs_sum, sizeof(double) * groups).wait_and_throw();
    q->memcpy(out_rhs_count_by_group, scratch_group_len, sizeof(uint32_t) * groups)
        .wait_and_throw();

    size_t selected_host = 0;
    for (size_t i = 0; i < groups; ++i)
      selected_host += static_cast<size_t>(out_count_by_group[i]);
    *selected_count = selected_host;
    *uncertain_count = 0;
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident dense grouped f64 stats-pair kernel failed: %s\n",
                 e.what());
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr,
                 "pgaccel: resident dense grouped f64 stats-pair kernel failed (unknown)\n");
    return PGACCEL_ERROR;
  }
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_mul_sum_count_usm(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col lhs_col, pgaccel_expr_usm_col rhs_col,
    pgaccel_expr_usm_col filter_col, int32_t filter_mode, size_t row_count, int32_t group_min,
    int32_t group_count, double* scratch_sum, uint32_t* scratch_count, double* scratch_partial_sum,
    uint32_t* scratch_partial_count, size_t scratch_partial_capacity, double* out_sum_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count) {
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;
  const auto fail = [&](const char* reason,
                        pgaccel_status status = PGACCEL_ERROR) -> pgaccel_status {
    std::fprintf(stderr,
                 "pgaccel: resident dense grouped f64 mul sum/count preflight failed: "
                 "reason=%s filter_mode=%d row_count=%zu group_min=%d group_count=%d "
                 "group_values=%p group_nulls=%p group_type=%d lhs_values=%p lhs_nulls=%p "
                 "lhs_type=%d rhs_values=%p rhs_nulls=%p rhs_type=%d filter_values=%p "
                 "filter_nulls=%p filter_type=%d scratch_sum=%p scratch_count=%p "
                 "partial_sum=%p partial_count=%p partial_capacity=%zu out_sum=%p out_count=%p "
                 "out_group_capacity=%zu selected_count=%p uncertain_count=%p\n",
                 reason, filter_mode, row_count, group_min, group_count, group_col.values,
                 group_col.nulls, static_cast<int>(group_col.type), lhs_col.values, lhs_col.nulls,
                 static_cast<int>(lhs_col.type), rhs_col.values, rhs_col.nulls,
                 static_cast<int>(rhs_col.type), filter_col.values, filter_col.nulls,
                 static_cast<int>(filter_col.type), static_cast<void*>(scratch_sum),
                 static_cast<void*>(scratch_count), static_cast<void*>(scratch_partial_sum),
                 static_cast<void*>(scratch_partial_count), scratch_partial_capacity,
                 static_cast<void*>(out_sum_by_group), static_cast<void*>(out_count_by_group),
                 out_group_capacity, static_cast<void*>(selected_count),
                 static_cast<void*>(uncertain_count));
    std::fflush(stderr);
    return status;
  };

  if (!selected_count || !uncertain_count)
    return fail("missing_count_output");
  if (row_count == 0)
    return PGACCEL_OK;
  if (group_count <= 0)
    return fail("invalid_group_count");
  if (filter_mode != DENSE_GROUP_MUL_FILTER_NONE &&
      filter_mode != DENSE_GROUP_MUL_FILTER_AGGREGATE &&
      filter_mode != DENSE_GROUP_MUL_FILTER_MEASURE_ONLY)
    return fail("invalid_filter_mode");
  const bool filter_required = filter_mode != DENSE_GROUP_MUL_FILTER_NONE;
  if (!valid_i32_col(group_col) || !valid_f64_col(lhs_col) || !valid_f64_col(rhs_col) ||
      (filter_required && !valid_optional_bool_col(filter_col)) ||
      (filter_required && filter_col.values == nullptr) ||
      (!filter_required && filter_col.values != nullptr))
    return fail("invalid_input_column");
  const size_t groups = static_cast<size_t>(group_count);
  if (groups > DENSE_GROUP_ONE_PASS_MAX_GROUPS)
    return fail("unsupported_group_count");
  if (!scratch_sum || !scratch_count || !out_sum_by_group || !out_count_by_group ||
      out_group_capacity < groups)
    return fail("invalid_output");
  if (row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return fail("row_count_exceeds_u32");

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return fail("no_device_queue", PGACCEL_ERROR_NO_DEVICE);

  const size_t row_block_count =
      (row_count + DENSE_GROUP_ONE_PASS_BLOCK_ROWS - 1) / DENSE_GROUP_ONE_PASS_BLOCK_ROWS;
  if (row_block_count != 0 && groups > std::numeric_limits<size_t>::max() / row_block_count)
    return fail("dense_grouped_mul_partial_size_overflow");
  const size_t partial_items = row_block_count * groups;
  const bool use_cached_partials =
      scratch_partial_capacity >= partial_items && scratch_partial_sum && scratch_partial_count;
  double* partial_sum =
      use_cached_partials ? scratch_partial_sum : sycl::malloc_device<double>(partial_items, *q);
  uint32_t* partial_count = use_cached_partials ? scratch_partial_count
                                                : sycl::malloc_device<uint32_t>(partial_items, *q);
  if (!partial_sum || !partial_count) {
    if (!use_cached_partials && partial_sum)
      sycl::free(partial_sum, *q);
    if (!use_cached_partials && partial_count)
      sycl::free(partial_count, *q);
    return fail("dense_grouped_mul_partial_oom", PGACCEL_ERROR_OOM);
  }
  auto cleanup_partials = [&]() {
    if (!use_cached_partials) {
      sycl::free(partial_sum, *q);
      sycl::free(partial_count, *q);
    }
  };

  const auto* group_values = static_cast<const int32_t*>(group_col.values);
  const auto* lhs_values = static_cast<const double*>(lhs_col.values);
  const auto* rhs_values = static_cast<const double*>(rhs_col.values);
  const auto* filter = static_cast<const uint8_t*>(filter_col.values);
  const uint8_t* group_nulls = group_col.nulls;
  const uint8_t* lhs_nulls = lhs_col.nulls;
  const uint8_t* rhs_nulls = rhs_col.nulls;
  const uint8_t* filter_nulls = filter_col.nulls;
  constexpr size_t local_size = DENSE_GROUP_SIMPLE_WIDE_LOCAL_SIZE;

  try {
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<double, 1> local_sum(sycl::range<1>(local_size * groups), h);
       sycl::local_accessor<uint32_t, 1> local_count(sycl::range<1>(local_size * groups), h);
       h.parallel_for(sycl::nd_range<1>(sycl::range<1>(row_block_count * local_size),
                                        sycl::range<1>(local_size)),
                      [=](sycl::nd_item<1> item) {
                        const size_t row_block_idx = item.get_group(0);
                        const size_t local_id = item.get_local_id(0);
                        const size_t local_slots = local_size * groups;
                        const size_t row_start = row_block_idx * DENSE_GROUP_ONE_PASS_BLOCK_ROWS;
                        const size_t row_end =
                            sycl::min(row_start + DENSE_GROUP_ONE_PASS_BLOCK_ROWS, row_count);

                        for (size_t slot = local_id; slot < local_slots; slot += local_size) {
                          local_sum[slot] = 0.0;
                          local_count[slot] = 0;
                        }
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t row = row_start + local_id; row < row_end; row += local_size) {
                          if (group_nulls && group_nulls[row])
                            continue;
                          const int64_t group_idx64 = static_cast<int64_t>(group_values[row]) -
                                                      static_cast<int64_t>(group_min);
                          if (group_idx64 < 0 || group_idx64 >= static_cast<int64_t>(groups))
                            continue;
                          const size_t group_idx = static_cast<size_t>(group_idx64);
                          const size_t slot = local_id * groups + group_idx;
                          const bool filter_passes =
                              filter_mode == DENSE_GROUP_MUL_FILTER_NONE ||
                              (filter && !(filter_nulls && filter_nulls[row]) && filter[row] != 0);
                          const bool measure_valid =
                              !(lhs_nulls && lhs_nulls[row]) && !(rhs_nulls && rhs_nulls[row]);

                          if (filter_mode == DENSE_GROUP_MUL_FILTER_AGGREGATE) {
                            if (filter_passes) {
                              if (measure_valid)
                                local_sum[slot] += lhs_values[row] * rhs_values[row];
                              local_count[slot] += 1;
                            }
                          } else {
                            if (filter_passes && measure_valid)
                              local_sum[slot] += lhs_values[row] * rhs_values[row];
                            local_count[slot] += 1;
                          }
                        }
                        item.barrier(sycl::access::fence_space::local_space);

                        for (size_t group_idx = local_id; group_idx < groups;
                             group_idx += local_size) {
                          double sum = 0.0;
                          uint32_t count = 0;
                          for (size_t lane = 0; lane < local_size; ++lane) {
                            const size_t slot = lane * groups + group_idx;
                            sum += local_sum[slot];
                            count += local_count[slot];
                          }
                          const size_t partial_idx = row_block_idx * groups + group_idx;
                          partial_sum[partial_idx] = sum;
                          partial_count[partial_idx] = count;
                        }
                      });
     }).wait_and_throw();

    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<double, 1> local_sum(
           sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
       sycl::local_accessor<uint32_t, 1> local_count(
           sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
       h.parallel_for(
           sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE),
                             sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE)),
           [=](sycl::nd_item<1> item) {
             const size_t group_idx = item.get_group(0);
             const size_t local_id = item.get_local_id(0);
             double sum = 0.0;
             uint32_t count = 0;
             for (size_t block_idx = local_id; block_idx < row_block_count;
                  block_idx += DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE) {
               const size_t partial_idx = block_idx * groups + group_idx;
               sum += partial_sum[partial_idx];
               count += partial_count[partial_idx];
             }
             local_sum[local_id] = sum;
             local_count[local_id] = count;
             item.barrier(sycl::access::fence_space::local_space);

             for (size_t stride = DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE / 2; stride > 0;
                  stride /= 2) {
               if (local_id < stride) {
                 local_sum[local_id] += local_sum[local_id + stride];
                 local_count[local_id] += local_count[local_id + stride];
               }
               item.barrier(sycl::access::fence_space::local_space);
             }

             if (local_id == 0) {
               scratch_sum[group_idx] = local_sum[0];
               scratch_count[group_idx] = local_count[0];
             }
           });
     }).wait_and_throw();

    q->memcpy(out_sum_by_group, scratch_sum, sizeof(double) * groups).wait_and_throw();
    q->memcpy(out_count_by_group, scratch_count, sizeof(uint32_t) * groups).wait_and_throw();
    size_t selected_host = 0;
    for (size_t i = 0; i < groups; ++i)
      selected_host += static_cast<size_t>(out_count_by_group[i]);
    *selected_count = selected_host;
    *uncertain_count = 0;
    cleanup_partials();
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident dense grouped f64 mul sum/count kernel failed: %s\n",
                 e.what());
    cleanup_partials();
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr,
                 "pgaccel: resident dense grouped f64 mul sum/count kernel failed (unknown)\n");
    cleanup_partials();
    return PGACCEL_ERROR;
  }
}

extern "C" pgaccel_status pgaccel_expr_template_resident_star_dim_group_project_f64_usm(
    pgaccel_expr_usm_col fact_key_col, pgaccel_expr_usm_col value_col, size_t row_count,
    const uint8_t* dim_match_by_key, const int32_t* dim_group_code_by_key, size_t dim_key_count,
    uint16_t value_cmp_opcode, double value_const, int32_t* out_group_codes,
    size_t out_group_capacity) {
  const auto fail = [&](const char* reason,
                        pgaccel_status status = PGACCEL_ERROR) -> pgaccel_status {
    std::fprintf(stderr,
                 "pgaccel: resident star dim group projection preflight failed: reason=%s "
                 "row_count=%zu fact_key_values=%p fact_key_nulls=%p fact_key_type=%d "
                 "value_values=%p value_nulls=%p value_type=%d dim_match=%p dim_group=%p "
                 "dim_key_count=%zu cmp_opcode=%u out_group_codes=%p out_capacity=%zu\n",
                 reason, row_count, fact_key_col.values, fact_key_col.nulls,
                 static_cast<int>(fact_key_col.type), value_col.values, value_col.nulls,
                 static_cast<int>(value_col.type), static_cast<const void*>(dim_match_by_key),
                 static_cast<const void*>(dim_group_code_by_key), dim_key_count,
                 static_cast<unsigned>(value_cmp_opcode), static_cast<void*>(out_group_codes),
                 out_group_capacity);
    std::fflush(stderr);
    return status;
  };

  if (row_count == 0)
    return PGACCEL_OK;
  if (!valid_i32_col(fact_key_col) || !valid_f64_col(value_col))
    return fail("invalid_input_column");
  if (!dim_match_by_key || !dim_group_code_by_key || dim_key_count == 0)
    return fail("invalid_dimension_map");
  if (!out_group_codes || out_group_capacity < row_count)
    return fail("invalid_output");
  if (!valid_cmp_opcode(value_cmp_opcode))
    return fail("invalid_cmp_opcode");

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return fail("no_device_queue", PGACCEL_ERROR_NO_DEVICE);

  const auto* fact_keys = static_cast<const int32_t*>(fact_key_col.values);
  const uint8_t* fact_key_nulls = fact_key_col.nulls;
  const auto* values = static_cast<const double*>(value_col.values);
  const uint8_t* value_nulls = value_col.nulls;
  constexpr size_t local_size = 256;
  if (row_count > std::numeric_limits<size_t>::max() - (local_size - 1))
    return fail("global_size_overflow", PGACCEL_UNSUPPORTED);
  const size_t global_size = ((row_count + local_size - 1) / local_size) * local_size;

  try {
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::nd_range<1>(sycl::range<1>(global_size), sycl::range<1>(local_size)),
                      [=](sycl::nd_item<1> item) {
                        const size_t row = item.get_global_id(0);
                        if (row >= row_count)
                          return;

                        int32_t group_code = -1;
                        if (!(fact_key_nulls && fact_key_nulls[row]) &&
                            !(value_nulls && value_nulls[row])) {
                          const double value = values[row];
                          const int32_t dim_key = fact_keys[row];
                          if (compare_f64(value, value_cmp_opcode, value_const) && dim_key >= 0 &&
                              static_cast<size_t>(dim_key) < dim_key_count) {
                            const size_t key_idx = static_cast<size_t>(dim_key);
                            if (dim_match_by_key[key_idx] != 0) {
                              const int32_t mapped_group = dim_group_code_by_key[key_idx];
                              if (mapped_group >= 0)
                                group_code = mapped_group;
                            }
                          }
                        }
                        out_group_codes[row] = group_code;
                      });
     }).wait_and_throw();
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident star dim group projection failed: %s\n", e.what());
    std::fflush(stderr);
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: resident star dim group projection failed (unknown)\n");
    std::fflush(stderr);
    return PGACCEL_ERROR;
  }
}

extern "C" pgaccel_status pgaccel_expr_template_resident_star_dim_group_compact_f64_usm(
    pgaccel_expr_usm_col fact_key_col, pgaccel_expr_usm_col value_col, size_t row_count,
    const uint8_t* dim_match_by_key, const int32_t* dim_group_code_by_key, size_t dim_key_count,
    uint16_t value_cmp_opcode, double value_const, int32_t* out_group_codes, double* out_values,
    size_t out_capacity, size_t* selected_count, size_t* uncertain_count) {
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;
  const auto fail = [&](const char* reason,
                        pgaccel_status status = PGACCEL_ERROR) -> pgaccel_status {
    std::fprintf(stderr,
                 "pgaccel: resident star dim group compact preflight failed: reason=%s "
                 "row_count=%zu fact_key_values=%p fact_key_nulls=%p fact_key_type=%d "
                 "value_values=%p value_nulls=%p value_type=%d dim_match=%p dim_group=%p "
                 "dim_key_count=%zu cmp_opcode=%u out_group_codes=%p out_values=%p "
                 "out_capacity=%zu selected_count=%p uncertain_count=%p\n",
                 reason, row_count, fact_key_col.values, fact_key_col.nulls,
                 static_cast<int>(fact_key_col.type), value_col.values, value_col.nulls,
                 static_cast<int>(value_col.type), static_cast<const void*>(dim_match_by_key),
                 static_cast<const void*>(dim_group_code_by_key), dim_key_count,
                 static_cast<unsigned>(value_cmp_opcode), static_cast<void*>(out_group_codes),
                 static_cast<void*>(out_values), out_capacity, static_cast<void*>(selected_count),
                 static_cast<void*>(uncertain_count));
    std::fflush(stderr);
    return status;
  };

  if (!selected_count || !uncertain_count)
    return fail("missing_count_output");
  if (row_count == 0)
    return PGACCEL_OK;
  if (row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return fail("row_count_exceeds_u32");
  if (!valid_i32_col(fact_key_col) || !valid_f64_col(value_col))
    return fail("invalid_input_column");
  if (!dim_match_by_key || !dim_group_code_by_key || dim_key_count == 0)
    return fail("invalid_dimension_map");
  if (!out_group_codes || !out_values || out_capacity < row_count)
    return fail("invalid_output");
  if (!valid_cmp_opcode(value_cmp_opcode))
    return fail("invalid_cmp_opcode");

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return fail("no_device_queue", PGACCEL_ERROR_NO_DEVICE);

  uint32_t* compact_count = nullptr;
  auto cleanup_compact_count = [&]() {
    if (compact_count) {
      try {
        sycl::free(compact_count, *q);
      } catch (...) {}
      compact_count = nullptr;
    }
  };
  const auto* fact_keys = static_cast<const int32_t*>(fact_key_col.values);
  const uint8_t* fact_key_nulls = fact_key_col.nulls;
  const auto* values = static_cast<const double*>(value_col.values);
  const uint8_t* value_nulls = value_col.nulls;
  constexpr size_t local_size = 256;
  const size_t global_size = ((row_count + local_size - 1) / local_size) * local_size;

  try {
    compact_count = sycl::malloc_shared<uint32_t>(1, *q);
    if (!compact_count)
      return fail("compact_count_oom", PGACCEL_ERROR_OOM);
    *compact_count = 0;

    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::nd_range<1>(sycl::range<1>(global_size), sycl::range<1>(local_size)),
                      [=](sycl::nd_item<1> item) {
                        const size_t row = item.get_global_id(0);
                        if (row >= row_count)
                          return;
                        if ((fact_key_nulls && fact_key_nulls[row]) ||
                            (value_nulls && value_nulls[row])) {
                          return;
                        }

                        const double value = values[row];
                        const int32_t dim_key = fact_keys[row];
                        if (!compare_f64(value, value_cmp_opcode, value_const) || dim_key < 0 ||
                            static_cast<size_t>(dim_key) >= dim_key_count) {
                          return;
                        }

                        const size_t key_idx = static_cast<size_t>(dim_key);
                        if (dim_match_by_key[key_idx] == 0)
                          return;
                        const int32_t mapped_group = dim_group_code_by_key[key_idx];
                        if (mapped_group < 0)
                          return;

                        sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed,
                                         sycl::memory_scope::device,
                                         sycl::access::address_space::global_space>
                            count_ref(compact_count[0]);
                        const uint32_t pos = count_ref.fetch_add(1u);
                        out_group_codes[pos] = mapped_group;
                        out_values[pos] = value;
                      });
     }).wait_and_throw();

    *selected_count = static_cast<size_t>(*compact_count);
    *uncertain_count = 0;
    cleanup_compact_count();
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident star dim group compaction failed: %s\n", e.what());
    std::fflush(stderr);
    cleanup_compact_count();
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: resident star dim group compaction failed (unknown)\n");
    std::fflush(stderr);
    cleanup_compact_count();
    return PGACCEL_ERROR;
  }
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v9(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, int32_t measure_predicate_op, int32_t measure_predicate_source,
    int32_t measure_predicate_range_count, double measure_predicate_lo0,
    double measure_predicate_hi0, double measure_predicate_lo1, double measure_predicate_hi1,
    double measure_predicate_lo2, double measure_predicate_hi2, double measure_predicate_lo3,
    double measure_predicate_hi3, pgaccel_expr_usm_col filter_col, size_t row_count,
    int32_t group_min, int32_t group_count, double* scratch_sum, double* scratch_min,
    double* scratch_max, uint32_t* scratch_count, uint32_t* scratch_group_start,
    uint32_t* scratch_group_cursor, size_t scratch_group_capacity, int32_t* scratch_sorted_group,
    uint32_t* scratch_row_index, size_t scratch_row_capacity, double* scratch_partial_sum,
    double* scratch_partial_min, double* scratch_partial_max, uint32_t* scratch_partial_count,
    size_t scratch_partial_capacity, double* out_sum_by_group, double* out_min_by_group,
    double* out_max_by_group, uint32_t* out_count_by_group, size_t out_group_capacity,
    size_t* selected_count, size_t* uncertain_count) {
  if (selected_count)
    *selected_count = 0;
  if (uncertain_count)
    *uncertain_count = 0;
  const auto fail = [&](const char* reason,
                        pgaccel_status status = PGACCEL_ERROR) -> pgaccel_status {
    log_dense_grouped_f64_preflight_failure(
        reason, group_col, value_col, filter_col, row_count, group_min, group_count, scratch_sum,
        scratch_min, scratch_max, scratch_count, scratch_group_start, scratch_group_cursor,
        scratch_group_capacity, scratch_sorted_group, scratch_row_index, scratch_row_capacity,
        out_sum_by_group, out_min_by_group, out_max_by_group, out_count_by_group,
        out_group_capacity, selected_count, uncertain_count);
    return status;
  };
  if (!selected_count || !uncertain_count)
    return fail("missing_count_output");
  if (row_count == 0)
    return PGACCEL_OK;
  if (group_count <= 0)
    return fail("invalid_group_count");
  if (measure_op < 0 || measure_op > 2)
    return fail("invalid_measure_op");
  if (filter_mode != DENSE_GROUP_FILTER_ROWS && filter_mode != DENSE_GROUP_FILTER_MEASURE_ONLY)
    return fail("invalid_filter_mode");
  if (measure_predicate_op != DENSE_GROUP_MEASURE_PRED_BOOL_ONLY &&
      measure_predicate_op != DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_BETWEEN &&
      measure_predicate_op != DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_RANGES)
    return fail("invalid_measure_predicate_op");
  if (measure_predicate_source != DENSE_GROUP_MEASURE_PRED_SOURCE_VALUE &&
      measure_predicate_source != DENSE_GROUP_MEASURE_PRED_SOURCE_RHS)
    return fail("invalid_measure_predicate_source");
  if (measure_predicate_range_count < 0 || measure_predicate_range_count > 4)
    return fail("invalid_measure_predicate_range_count");
  if (measure_predicate_op == DENSE_GROUP_MEASURE_PRED_BOOL_ONLY &&
      measure_predicate_range_count != 0)
    return fail("bool_only_predicate_with_ranges");
  if (measure_predicate_op == DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_BETWEEN &&
      measure_predicate_range_count != 1)
    return fail("between_predicate_requires_one_range");
  if (measure_predicate_op == DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_RANGES &&
      measure_predicate_range_count <= 0)
    return fail("ranges_predicate_requires_ranges");
  if (aggregate_mask == 0 || (aggregate_mask & ~DENSE_GROUP_AGG_ALL) != 0)
    return fail("invalid_aggregate_mask");
  const bool need_sum = (aggregate_mask & DENSE_GROUP_AGG_SUM) != 0;
  const bool need_min = (aggregate_mask & DENSE_GROUP_AGG_MIN) != 0;
  const bool need_max = (aggregate_mask & DENSE_GROUP_AGG_MAX) != 0;
  const bool need_count = (aggregate_mask & DENSE_GROUP_AGG_COUNT) != 0;
  if (!need_sum || !need_count)
    return fail("unsupported_aggregate_mask");
  if (need_min != need_max)
    return fail("unsupported_min_max_mask");
  if (filter_mode == DENSE_GROUP_FILTER_MEASURE_ONLY && (need_min || need_max))
    return fail("unsupported_measure_filter_min_max");
  const bool range_predicate =
      measure_predicate_op == DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_BETWEEN ||
      measure_predicate_op == DENSE_GROUP_MEASURE_PRED_BOOL_AND_RHS_RANGES;
  const bool rhs_required_for_predicate =
      range_predicate && measure_predicate_source == DENSE_GROUP_MEASURE_PRED_SOURCE_RHS;
  if (rhs_required_for_predicate && !valid_f64_col(value_rhs_col))
    return fail("measure_predicate_requires_rhs");
  const bool range0_valid =
      measure_predicate_range_count < 1 || (measure_predicate_lo0 <= measure_predicate_hi0 &&
                                            measure_predicate_lo0 == measure_predicate_lo0 &&
                                            measure_predicate_hi0 == measure_predicate_hi0);
  const bool range1_valid =
      measure_predicate_range_count < 2 || (measure_predicate_lo1 <= measure_predicate_hi1 &&
                                            measure_predicate_lo1 == measure_predicate_lo1 &&
                                            measure_predicate_hi1 == measure_predicate_hi1);
  const bool range2_valid =
      measure_predicate_range_count < 3 || (measure_predicate_lo2 <= measure_predicate_hi2 &&
                                            measure_predicate_lo2 == measure_predicate_lo2 &&
                                            measure_predicate_hi2 == measure_predicate_hi2);
  const bool range3_valid =
      measure_predicate_range_count < 4 || (measure_predicate_lo3 <= measure_predicate_hi3 &&
                                            measure_predicate_lo3 == measure_predicate_lo3 &&
                                            measure_predicate_hi3 == measure_predicate_hi3);
  if (!range0_valid || !range1_valid || !range2_valid || !range3_valid)
    return fail("invalid_measure_predicate_range");
  const bool rhs_required_for_measure = measure_op != 0;
  if (!valid_i32_col(group_col) || !valid_f64_col(value_col) ||
      !valid_measure_rhs_col(value_rhs_col,
                             rhs_required_for_measure || rhs_required_for_predicate) ||
      !valid_optional_bool_col(filter_col))
    return fail("invalid_input_column");

  const size_t groups = static_cast<size_t>(group_count);
  if (!scratch_sum || (need_min && !scratch_min) || (need_max && !scratch_max) || !scratch_count ||
      !scratch_group_start || !scratch_group_cursor || scratch_group_capacity < groups ||
      !scratch_sorted_group || !scratch_row_index || scratch_row_capacity < row_count ||
      !out_sum_by_group || (need_min && !out_min_by_group) || (need_max && !out_max_by_group) ||
      !out_count_by_group || out_group_capacity < groups)
    return fail("invalid_scratch_or_output");
  if (row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return fail("row_count_exceeds_u32");

  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return fail("no_device_queue", PGACCEL_ERROR_NO_DEVICE);

  const auto* group_values = static_cast<const int32_t*>(group_col.values);
  const auto* values = static_cast<const double*>(value_col.values);
  const auto* rhs_values = static_cast<const double*>(value_rhs_col.values);
  const auto* filter = static_cast<const uint8_t*>(filter_col.values);
  const uint8_t* group_nulls = group_col.nulls;
  const uint8_t* value_nulls = value_col.nulls;
  const uint8_t* rhs_nulls = value_rhs_col.nulls;
  const uint8_t* filter_nulls = filter_col.nulls;
  const bool rhs_required = rhs_required_for_measure;
  const bool measure_filter_only = filter_mode == DENSE_GROUP_FILTER_MEASURE_ONLY;
  const bool simple_column_sumcount = !measure_filter_only && !rhs_required && measure_op == 0 &&
                                      filter == nullptr && filter_nulls == nullptr &&
                                      measure_predicate_op == DENSE_GROUP_MEASURE_PRED_BOOL_ONLY;

  try {
    q->memset(scratch_sum, 0, sizeof(double) * groups).wait_and_throw();
    q->memset(scratch_count, 0, sizeof(uint32_t) * groups).wait_and_throw();
    if (need_min)
      q->fill(scratch_min, std::numeric_limits<double>::max(), groups).wait_and_throw();
    if (need_max)
      q->fill(scratch_max, std::numeric_limits<double>::lowest(), groups).wait_and_throw();

    if (!need_min && !need_max && groups <= DENSE_GROUP_ONE_PASS_MAX_GROUPS &&
        row_count >= DENSE_GROUP_SORT_MIN_ROWS) {
      const size_t one_pass_local_size = DENSE_GROUP_ONE_PASS_LOCAL_SIZE;
      const size_t one_pass_tile_groups = sycl::min(groups, DENSE_GROUP_ONE_PASS_TILE_GROUPS);
      const size_t one_pass_tile_count = (groups + one_pass_tile_groups - 1) / one_pass_tile_groups;
      const size_t row_block_count =
          (row_count + DENSE_GROUP_ONE_PASS_BLOCK_ROWS - 1) / DENSE_GROUP_ONE_PASS_BLOCK_ROWS;
      if (row_block_count != 0 && groups > std::numeric_limits<size_t>::max() / row_block_count) {
        return fail("dense_grouped_one_pass_partial_size_overflow");
      }
      if (row_block_count != 0 &&
          one_pass_tile_count > std::numeric_limits<size_t>::max() / row_block_count) {
        return fail("dense_grouped_one_pass_workgroup_overflow");
      }
      const size_t one_pass_workgroup_count = row_block_count * one_pass_tile_count;
      const size_t partial_items = row_block_count * groups;
      const bool use_cached_partials =
          scratch_partial_capacity >= partial_items && scratch_partial_sum && scratch_partial_count;
      double* partial_sum = use_cached_partials ? scratch_partial_sum
                                                : sycl::malloc_device<double>(partial_items, *q);
      uint32_t* partial_count = use_cached_partials
                                    ? scratch_partial_count
                                    : sycl::malloc_device<uint32_t>(partial_items, *q);
      if (!partial_sum || !partial_count) {
        if (!use_cached_partials && partial_sum)
          sycl::free(partial_sum, *q);
        if (!use_cached_partials && partial_count)
          sycl::free(partial_count, *q);
        return fail("dense_grouped_one_pass_partial_oom", PGACCEL_ERROR_OOM);
      }
      auto cleanup_partials = [&]() {
        if (!use_cached_partials) {
          sycl::free(partial_sum, *q);
          sycl::free(partial_count, *q);
        }
      };
      try {
        if (simple_column_sumcount) {
          q->submit([&](sycl::handler& h) {
             sycl::local_accessor<double, 1> local_sum(
                 sycl::range<1>(one_pass_local_size * one_pass_tile_groups), h);
             sycl::local_accessor<uint32_t, 1> local_count(
                 sycl::range<1>(one_pass_local_size * one_pass_tile_groups), h);
             h.parallel_for(
                 sycl::nd_range<1>(sycl::range<1>(one_pass_workgroup_count * one_pass_local_size),
                                   sycl::range<1>(one_pass_local_size)),
                 [=](sycl::nd_item<1> item) {
                   const size_t workgroup_idx = item.get_group(0);
                   const size_t tile_idx = workgroup_idx % one_pass_tile_count;
                   const size_t row_block_idx = workgroup_idx / one_pass_tile_count;
                   const size_t local_id = item.get_local_id(0);
                   const size_t tile_start = tile_idx * one_pass_tile_groups;
                   const size_t groups_in_tile =
                       sycl::min(one_pass_tile_groups, groups - tile_start);
                   const size_t local_slots = one_pass_local_size * one_pass_tile_groups;
                   const size_t row_start = row_block_idx * DENSE_GROUP_ONE_PASS_BLOCK_ROWS;
                   const size_t row_end =
                       sycl::min(row_start + DENSE_GROUP_ONE_PASS_BLOCK_ROWS, row_count);

                   for (size_t slot = local_id; slot < local_slots; slot += one_pass_local_size) {
                     local_sum[slot] = 0.0;
                     local_count[slot] = 0;
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t row = row_start + local_id; row < row_end;
                        row += one_pass_local_size) {
                     if (group_nulls && group_nulls[row]) {
                       continue;
                     }
                     if (value_nulls && value_nulls[row]) {
                       continue;
                     }
                     const int64_t group_idx64 =
                         static_cast<int64_t>(group_values[row]) - static_cast<int64_t>(group_min);
                     if (group_idx64 < static_cast<int64_t>(tile_start) ||
                         group_idx64 >= static_cast<int64_t>(tile_start + groups_in_tile)) {
                       continue;
                     }
                     const size_t group_offset = static_cast<size_t>(group_idx64) - tile_start;
                     const size_t slot = local_id * one_pass_tile_groups + group_offset;
                     local_sum[slot] += values[row];
                     local_count[slot] += 1;
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t group_offset = local_id; group_offset < groups_in_tile;
                        group_offset += one_pass_local_size) {
                     double sum = 0.0;
                     uint32_t count = 0;
                     for (size_t lane = 0; lane < one_pass_local_size; ++lane) {
                       const size_t slot = lane * one_pass_tile_groups + group_offset;
                       sum += local_sum[slot];
                       count += local_count[slot];
                     }
                     const size_t group_idx = tile_start + group_offset;
                     const size_t partial_idx = row_block_idx * groups + group_idx;
                     partial_sum[partial_idx] = sum;
                     partial_count[partial_idx] = count;
                   }
                 });
           }).wait_and_throw();
        } else {
          q->submit([&](sycl::handler& h) {
             sycl::local_accessor<double, 1> local_sum(
                 sycl::range<1>(one_pass_local_size * one_pass_tile_groups), h);
             sycl::local_accessor<uint32_t, 1> local_count(
                 sycl::range<1>(one_pass_local_size * one_pass_tile_groups), h);
             h.parallel_for(
                 sycl::nd_range<1>(sycl::range<1>(one_pass_workgroup_count * one_pass_local_size),
                                   sycl::range<1>(one_pass_local_size)),
                 [=](sycl::nd_item<1> item) {
                   const size_t workgroup_idx = item.get_group(0);
                   const size_t tile_idx = workgroup_idx % one_pass_tile_count;
                   const size_t row_block_idx = workgroup_idx / one_pass_tile_count;
                   const size_t local_id = item.get_local_id(0);
                   const size_t tile_start = tile_idx * one_pass_tile_groups;
                   const size_t groups_in_tile =
                       sycl::min(one_pass_tile_groups, groups - tile_start);
                   const size_t local_slots = one_pass_local_size * one_pass_tile_groups;
                   const size_t row_start = row_block_idx * DENSE_GROUP_ONE_PASS_BLOCK_ROWS;
                   const size_t row_end =
                       sycl::min(row_start + DENSE_GROUP_ONE_PASS_BLOCK_ROWS, row_count);

                   for (size_t slot = local_id; slot < local_slots; slot += one_pass_local_size) {
                     local_sum[slot] = 0.0;
                     local_count[slot] = 0;
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t row = row_start + local_id; row < row_end;
                        row += one_pass_local_size) {
                     if (group_nulls && group_nulls[row]) {
                       continue;
                     }
                     const int64_t group_idx64 =
                         static_cast<int64_t>(group_values[row]) - static_cast<int64_t>(group_min);
                     if (group_idx64 < static_cast<int64_t>(tile_start) ||
                         group_idx64 >= static_cast<int64_t>(tile_start + groups_in_tile)) {
                       continue;
                     }
                     const size_t group_offset = static_cast<size_t>(group_idx64) - tile_start;
                     const size_t slot = local_id * one_pass_tile_groups + group_offset;
                     const bool measure_valid = !(value_nulls && value_nulls[row]) &&
                                                !(rhs_required && rhs_nulls && rhs_nulls[row]);
                     const bool filter_passes = resident_dense_filter_passes(
                         row, filter, filter_nulls, values, value_nulls, rhs_values, rhs_nulls,
                         measure_predicate_source, measure_predicate_op,
                         measure_predicate_range_count, measure_predicate_lo0,
                         measure_predicate_hi0, measure_predicate_lo1, measure_predicate_hi1,
                         measure_predicate_lo2, measure_predicate_hi2, measure_predicate_lo3,
                         measure_predicate_hi3);
                     if (measure_filter_only) {
                       if (measure_valid && filter_passes) {
                         local_sum[slot] += resident_dense_measure_value(
                             values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                       }
                       local_count[slot] += 1;
                     } else if (measure_valid && filter_passes) {
                       local_sum[slot] += resident_dense_measure_value(
                           values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                       local_count[slot] += 1;
                     }
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t group_offset = local_id; group_offset < groups_in_tile;
                        group_offset += one_pass_local_size) {
                     double sum = 0.0;
                     uint32_t count = 0;
                     for (size_t lane = 0; lane < one_pass_local_size; ++lane) {
                       const size_t slot = lane * one_pass_tile_groups + group_offset;
                       sum += local_sum[slot];
                       count += local_count[slot];
                     }
                     const size_t group_idx = tile_start + group_offset;
                     const size_t partial_idx = row_block_idx * groups + group_idx;
                     partial_sum[partial_idx] = sum;
                     partial_count[partial_idx] = count;
                   }
                 });
           }).wait_and_throw();
        }

        q->submit([&](sycl::handler& h) {
           sycl::local_accessor<double, 1> local_sum(
               sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
           sycl::local_accessor<uint32_t, 1> local_count(
               sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
           h.parallel_for(
               sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE),
                                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE)),
               [=](sycl::nd_item<1> item) {
                 const size_t group_idx = item.get_group(0);
                 const size_t local_id = item.get_local_id(0);
                 double sum = 0.0;
                 uint32_t count = 0;
                 for (size_t block_idx = local_id; block_idx < row_block_count;
                      block_idx += DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE) {
                   const size_t partial_idx = block_idx * groups + group_idx;
                   sum += partial_sum[partial_idx];
                   count += partial_count[partial_idx];
                 }

                 local_sum[local_id] = sum;
                 local_count[local_id] = count;
                 item.barrier(sycl::access::fence_space::local_space);

                 for (size_t stride = DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE / 2; stride > 0;
                      stride /= 2) {
                   if (local_id < stride) {
                     local_sum[local_id] += local_sum[local_id + stride];
                     local_count[local_id] += local_count[local_id + stride];
                   }
                   item.barrier(sycl::access::fence_space::local_space);
                 }

                 if (local_id == 0) {
                   scratch_sum[group_idx] = local_sum[0];
                   scratch_count[group_idx] = local_count[0];
                 }
               });
         }).wait_and_throw();
      } catch (...) {
        cleanup_partials();
        throw;
      }
      cleanup_partials();
    } else if (!need_min && !need_max && groups < DENSE_GROUP_SORT_MIN_GROUPS &&
               row_count >= DENSE_GROUP_BLOCKED_MIN_ROWS) {
      const size_t row_block_count =
          (row_count + DENSE_GROUP_BLOCK_ROWS - 1) / DENSE_GROUP_BLOCK_ROWS;
      const size_t tile_count = (groups + DENSE_GROUP_TILE_GROUPS - 1) / DENSE_GROUP_TILE_GROUPS;
      if (row_block_count != 0 &&
          tile_count > std::numeric_limits<size_t>::max() / row_block_count) {
        return fail("dense_grouped_block_workgroup_overflow");
      }
      if (row_block_count != 0 && groups > std::numeric_limits<size_t>::max() / row_block_count) {
        return fail("dense_grouped_block_partial_size_overflow");
      }
      const size_t workgroup_count = row_block_count * tile_count;
      const size_t partial_items = row_block_count * groups;
      const bool use_cached_partials =
          scratch_partial_capacity >= partial_items && scratch_partial_sum && scratch_partial_count;
      double* partial_sum = use_cached_partials ? scratch_partial_sum
                                                : sycl::malloc_device<double>(partial_items, *q);
      uint32_t* partial_count = use_cached_partials
                                    ? scratch_partial_count
                                    : sycl::malloc_device<uint32_t>(partial_items, *q);
      if (!partial_sum || !partial_count) {
        if (!use_cached_partials && partial_sum)
          sycl::free(partial_sum, *q);
        if (!use_cached_partials && partial_count)
          sycl::free(partial_count, *q);
        return fail("dense_grouped_block_partial_oom", PGACCEL_ERROR_OOM);
      }
      auto cleanup_partials = [&]() {
        if (!use_cached_partials) {
          sycl::free(partial_sum, *q);
          sycl::free(partial_count, *q);
        }
      };
      try {
        q->submit([&](sycl::handler& h) {
           sycl::local_accessor<double, 1> local_sum(
               sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_TILE_GROUPS), h);
           sycl::local_accessor<uint32_t, 1> local_count(
               sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_TILE_GROUPS), h);
           h.parallel_for(
               sycl::nd_range<1>(sycl::range<1>(workgroup_count * DENSE_GROUP_TILE_LOCAL_SIZE),
                                 sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE)),
               [=](sycl::nd_item<1> item) {
                 const size_t workgroup_idx = item.get_group(0);
                 const size_t tile_idx = workgroup_idx % tile_count;
                 const size_t row_block_idx = workgroup_idx / tile_count;
                 const size_t local_id = item.get_local_id(0);
                 const size_t tile_start = tile_idx * DENSE_GROUP_TILE_GROUPS;
                 const size_t groups_in_tile =
                     sycl::min(DENSE_GROUP_TILE_GROUPS, groups - tile_start);
                 const size_t local_slots = DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_TILE_GROUPS;
                 const size_t row_start = row_block_idx * DENSE_GROUP_BLOCK_ROWS;
                 const size_t row_end = sycl::min(row_start + DENSE_GROUP_BLOCK_ROWS, row_count);

                 for (size_t slot = local_id; slot < local_slots;
                      slot += DENSE_GROUP_TILE_LOCAL_SIZE) {
                   local_sum[slot] = 0.0;
                   local_count[slot] = 0;
                 }
                 item.barrier(sycl::access::fence_space::local_space);

                 for (size_t row = row_start + local_id; row < row_end;
                      row += DENSE_GROUP_TILE_LOCAL_SIZE) {
                   if (group_nulls && group_nulls[row]) {
                     continue;
                   }
                   const int64_t group_idx64 =
                       static_cast<int64_t>(group_values[row]) - static_cast<int64_t>(group_min);
                   if (group_idx64 < static_cast<int64_t>(tile_start) ||
                       group_idx64 >= static_cast<int64_t>(tile_start + groups_in_tile)) {
                     continue;
                   }

                   const size_t group_offset = static_cast<size_t>(group_idx64) - tile_start;
                   const size_t slot = local_id * DENSE_GROUP_TILE_GROUPS + group_offset;
                   const bool measure_valid = !(value_nulls && value_nulls[row]) &&
                                              !(rhs_required && rhs_nulls && rhs_nulls[row]);
                   const bool filter_passes = resident_dense_filter_passes(
                       row, filter, filter_nulls, values, value_nulls, rhs_values, rhs_nulls,
                       measure_predicate_source, measure_predicate_op,
                       measure_predicate_range_count, measure_predicate_lo0, measure_predicate_hi0,
                       measure_predicate_lo1, measure_predicate_hi1, measure_predicate_lo2,
                       measure_predicate_hi2, measure_predicate_lo3, measure_predicate_hi3);
                   if (measure_filter_only) {
                     if (measure_valid && filter_passes) {
                       local_sum[slot] += resident_dense_measure_value(
                           values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                     }
                     local_count[slot] += 1;
                   } else if (measure_valid && filter_passes) {
                     local_sum[slot] += resident_dense_measure_value(
                         values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                     local_count[slot] += 1;
                   }
                 }
                 item.barrier(sycl::access::fence_space::local_space);

                 for (size_t group_offset = local_id; group_offset < groups_in_tile;
                      group_offset += DENSE_GROUP_TILE_LOCAL_SIZE) {
                   double sum = 0.0;
                   uint32_t count = 0;
                   for (size_t lane = 0; lane < DENSE_GROUP_TILE_LOCAL_SIZE; ++lane) {
                     const size_t slot = lane * DENSE_GROUP_TILE_GROUPS + group_offset;
                     sum += local_sum[slot];
                     count += local_count[slot];
                   }
                   const size_t group_idx = tile_start + group_offset;
                   const size_t partial_idx = row_block_idx * groups + group_idx;
                   partial_sum[partial_idx] = sum;
                   partial_count[partial_idx] = count;
                 }
               });
         }).wait_and_throw();

        q->submit([&](sycl::handler& h) {
           sycl::local_accessor<double, 1> local_sum(
               sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
           sycl::local_accessor<uint32_t, 1> local_count(
               sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
           h.parallel_for(
               sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE),
                                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE)),
               [=](sycl::nd_item<1> item) {
                 const size_t group_idx = item.get_group(0);
                 const size_t local_id = item.get_local_id(0);
                 double sum = 0.0;
                 uint32_t count = 0;
                 for (size_t block_idx = local_id; block_idx < row_block_count;
                      block_idx += DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE) {
                   const size_t partial_idx = block_idx * groups + group_idx;
                   sum += partial_sum[partial_idx];
                   count += partial_count[partial_idx];
                 }

                 local_sum[local_id] = sum;
                 local_count[local_id] = count;
                 item.barrier(sycl::access::fence_space::local_space);

                 for (size_t stride = DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE / 2; stride > 0;
                      stride /= 2) {
                   if (local_id < stride) {
                     local_sum[local_id] += local_sum[local_id + stride];
                     local_count[local_id] += local_count[local_id + stride];
                   }
                   item.barrier(sycl::access::fence_space::local_space);
                 }

                 if (local_id == 0) {
                   scratch_sum[group_idx] = local_sum[0];
                   scratch_count[group_idx] = local_count[0];
                 }
               });
         }).wait_and_throw();
      } catch (...) {
        cleanup_partials();
        throw;
      }
      cleanup_partials();
    } else if (need_min && need_max && groups < DENSE_GROUP_SORT_MIN_GROUPS &&
               row_count >= DENSE_GROUP_BLOCKED_MIN_ROWS) {
      const size_t row_block_count =
          (row_count + DENSE_GROUP_MINMAX_BLOCK_ROWS - 1) / DENSE_GROUP_MINMAX_BLOCK_ROWS;
      const size_t tile_count =
          (groups + DENSE_GROUP_MINMAX_TILE_GROUPS - 1) / DENSE_GROUP_MINMAX_TILE_GROUPS;
      if (row_block_count != 0 &&
          tile_count > std::numeric_limits<size_t>::max() / row_block_count) {
        return fail("dense_grouped_minmax_block_workgroup_overflow");
      }
      if (row_block_count != 0 && groups > std::numeric_limits<size_t>::max() / row_block_count) {
        return fail("dense_grouped_minmax_block_partial_size_overflow");
      }
      const size_t workgroup_count = row_block_count * tile_count;
      const size_t partial_items = row_block_count * groups;
      const bool use_cached_partials = scratch_partial_capacity >= partial_items &&
                                       scratch_partial_sum && scratch_partial_min &&
                                       scratch_partial_max && scratch_partial_count;
      double* partial_sum = use_cached_partials ? scratch_partial_sum
                                                : sycl::malloc_device<double>(partial_items, *q);
      double* partial_min = use_cached_partials ? scratch_partial_min
                                                : sycl::malloc_device<double>(partial_items, *q);
      double* partial_max = use_cached_partials ? scratch_partial_max
                                                : sycl::malloc_device<double>(partial_items, *q);
      uint32_t* partial_count = use_cached_partials
                                    ? scratch_partial_count
                                    : sycl::malloc_device<uint32_t>(partial_items, *q);
      if (!partial_sum || !partial_min || !partial_max || !partial_count) {
        if (!use_cached_partials && partial_sum)
          sycl::free(partial_sum, *q);
        if (!use_cached_partials && partial_min)
          sycl::free(partial_min, *q);
        if (!use_cached_partials && partial_max)
          sycl::free(partial_max, *q);
        if (!use_cached_partials && partial_count)
          sycl::free(partial_count, *q);
        return fail("dense_grouped_minmax_block_partial_oom", PGACCEL_ERROR_OOM);
      }
      auto cleanup_partials = [&]() {
        if (!use_cached_partials) {
          sycl::free(partial_sum, *q);
          sycl::free(partial_min, *q);
          sycl::free(partial_max, *q);
          sycl::free(partial_count, *q);
        }
      };
      try {
        const bool simple_column_minmaxavg =
            !measure_filter_only && !rhs_required && filter == nullptr && filter_nulls == nullptr &&
            measure_predicate_op == DENSE_GROUP_MEASURE_PRED_BOOL_ONLY;
        if (simple_column_minmaxavg) {
          q->submit([&](sycl::handler& h) {
             sycl::local_accessor<double, 1> local_sum(
                 sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS), h);
             sycl::local_accessor<double, 1> local_min(
                 sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS), h);
             sycl::local_accessor<double, 1> local_max(
                 sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS), h);
             sycl::local_accessor<uint32_t, 1> local_count(
                 sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS), h);
             h.parallel_for(
                 sycl::nd_range<1>(sycl::range<1>(workgroup_count * DENSE_GROUP_TILE_LOCAL_SIZE),
                                   sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE)),
                 [=](sycl::nd_item<1> item) {
                   const size_t workgroup_idx = item.get_group(0);
                   const size_t tile_idx = workgroup_idx % tile_count;
                   const size_t row_block_idx = workgroup_idx / tile_count;
                   const size_t local_id = item.get_local_id(0);
                   const size_t tile_start = tile_idx * DENSE_GROUP_MINMAX_TILE_GROUPS;
                   const size_t groups_in_tile =
                       sycl::min(DENSE_GROUP_MINMAX_TILE_GROUPS, groups - tile_start);
                   const size_t local_slots =
                       DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS;
                   const size_t row_start = row_block_idx * DENSE_GROUP_MINMAX_BLOCK_ROWS;
                   const size_t row_end =
                       sycl::min(row_start + DENSE_GROUP_MINMAX_BLOCK_ROWS, row_count);

                   for (size_t slot = local_id; slot < local_slots;
                        slot += DENSE_GROUP_TILE_LOCAL_SIZE) {
                     local_sum[slot] = 0.0;
                     local_min[slot] = std::numeric_limits<double>::max();
                     local_max[slot] = std::numeric_limits<double>::lowest();
                     local_count[slot] = 0;
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t row = row_start + local_id; row < row_end;
                        row += DENSE_GROUP_TILE_LOCAL_SIZE) {
                     if (group_nulls && group_nulls[row]) {
                       continue;
                     }
                     if (value_nulls && value_nulls[row]) {
                       continue;
                     }
                     const int64_t group_idx64 =
                         static_cast<int64_t>(group_values[row]) - static_cast<int64_t>(group_min);
                     if (group_idx64 < static_cast<int64_t>(tile_start) ||
                         group_idx64 >= static_cast<int64_t>(tile_start + groups_in_tile)) {
                       continue;
                     }

                     const size_t group_offset = static_cast<size_t>(group_idx64) - tile_start;
                     const size_t slot = local_id * DENSE_GROUP_MINMAX_TILE_GROUPS + group_offset;
                     const double value = values[row];
                     local_sum[slot] += value;
                     local_min[slot] = sycl::fmin(local_min[slot], value);
                     local_max[slot] = sycl::fmax(local_max[slot], value);
                     local_count[slot] += 1;
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t group_offset = local_id; group_offset < groups_in_tile;
                        group_offset += DENSE_GROUP_TILE_LOCAL_SIZE) {
                     double sum = 0.0;
                     double min_value = std::numeric_limits<double>::max();
                     double max_value = std::numeric_limits<double>::lowest();
                     uint32_t count = 0;
                     for (size_t lane = 0; lane < DENSE_GROUP_TILE_LOCAL_SIZE; ++lane) {
                       const size_t slot = lane * DENSE_GROUP_MINMAX_TILE_GROUPS + group_offset;
                       sum += local_sum[slot];
                       min_value = sycl::fmin(min_value, local_min[slot]);
                       max_value = sycl::fmax(max_value, local_max[slot]);
                       count += local_count[slot];
                     }
                     const size_t group_idx = tile_start + group_offset;
                     const size_t partial_idx = row_block_idx * groups + group_idx;
                     partial_sum[partial_idx] = sum;
                     partial_min[partial_idx] = min_value;
                     partial_max[partial_idx] = max_value;
                     partial_count[partial_idx] = count;
                   }
                 });
           }).wait_and_throw();

          q->submit([&](sycl::handler& h) {
             sycl::local_accessor<double, 1> local_sum(
                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
             sycl::local_accessor<double, 1> local_min(
                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
             sycl::local_accessor<double, 1> local_max(
                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
             sycl::local_accessor<uint32_t, 1> local_count(
                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
             h.parallel_for(
                 sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE),
                                   sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE)),
                 [=](sycl::nd_item<1> item) {
                   const size_t group_idx = item.get_group(0);
                   const size_t local_id = item.get_local_id(0);
                   double sum = 0.0;
                   double min_value = std::numeric_limits<double>::max();
                   double max_value = std::numeric_limits<double>::lowest();
                   uint32_t count = 0;
                   for (size_t block_idx = local_id; block_idx < row_block_count;
                        block_idx += DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE) {
                     const size_t partial_idx = block_idx * groups + group_idx;
                     sum += partial_sum[partial_idx];
                     min_value = sycl::fmin(min_value, partial_min[partial_idx]);
                     max_value = sycl::fmax(max_value, partial_max[partial_idx]);
                     count += partial_count[partial_idx];
                   }

                   local_sum[local_id] = sum;
                   local_min[local_id] = min_value;
                   local_max[local_id] = max_value;
                   local_count[local_id] = count;
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t stride = DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE / 2; stride > 0;
                        stride /= 2) {
                     if (local_id < stride) {
                       local_sum[local_id] += local_sum[local_id + stride];
                       local_min[local_id] =
                           sycl::fmin(local_min[local_id], local_min[local_id + stride]);
                       local_max[local_id] =
                           sycl::fmax(local_max[local_id], local_max[local_id + stride]);
                       local_count[local_id] += local_count[local_id + stride];
                     }
                     item.barrier(sycl::access::fence_space::local_space);
                   }

                   if (local_id == 0) {
                     scratch_sum[group_idx] = local_sum[0];
                     scratch_min[group_idx] = local_min[0];
                     scratch_max[group_idx] = local_max[0];
                     scratch_count[group_idx] = local_count[0];
                   }
                 });
           }).wait_and_throw();
        } else {
          q->submit([&](sycl::handler& h) {
             sycl::local_accessor<double, 1> local_sum(
                 sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS), h);
             sycl::local_accessor<uint32_t, 1> local_count(
                 sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS), h);
             h.parallel_for(
                 sycl::nd_range<1>(sycl::range<1>(workgroup_count * DENSE_GROUP_TILE_LOCAL_SIZE),
                                   sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE)),
                 [=](sycl::nd_item<1> item) {
                   const size_t workgroup_idx = item.get_group(0);
                   const size_t tile_idx = workgroup_idx % tile_count;
                   const size_t row_block_idx = workgroup_idx / tile_count;
                   const size_t local_id = item.get_local_id(0);
                   const size_t tile_start = tile_idx * DENSE_GROUP_MINMAX_TILE_GROUPS;
                   const size_t groups_in_tile =
                       sycl::min(DENSE_GROUP_MINMAX_TILE_GROUPS, groups - tile_start);
                   const size_t local_slots =
                       DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS;
                   const size_t row_start = row_block_idx * DENSE_GROUP_MINMAX_BLOCK_ROWS;
                   const size_t row_end =
                       sycl::min(row_start + DENSE_GROUP_MINMAX_BLOCK_ROWS, row_count);

                   for (size_t slot = local_id; slot < local_slots;
                        slot += DENSE_GROUP_TILE_LOCAL_SIZE) {
                     local_sum[slot] = 0.0;
                     local_count[slot] = 0;
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t row = row_start + local_id; row < row_end;
                        row += DENSE_GROUP_TILE_LOCAL_SIZE) {
                     if (group_nulls && group_nulls[row]) {
                       continue;
                     }
                     const int64_t group_idx64 =
                         static_cast<int64_t>(group_values[row]) - static_cast<int64_t>(group_min);
                     if (group_idx64 < static_cast<int64_t>(tile_start) ||
                         group_idx64 >= static_cast<int64_t>(tile_start + groups_in_tile)) {
                       continue;
                     }

                     const size_t group_offset = static_cast<size_t>(group_idx64) - tile_start;
                     const size_t slot = local_id * DENSE_GROUP_MINMAX_TILE_GROUPS + group_offset;
                     const bool measure_valid = !(value_nulls && value_nulls[row]) &&
                                                !(rhs_required && rhs_nulls && rhs_nulls[row]);
                     const bool filter_passes = resident_dense_filter_passes(
                         row, filter, filter_nulls, values, value_nulls, rhs_values, rhs_nulls,
                         measure_predicate_source, measure_predicate_op,
                         measure_predicate_range_count, measure_predicate_lo0,
                         measure_predicate_hi0, measure_predicate_lo1, measure_predicate_hi1,
                         measure_predicate_lo2, measure_predicate_hi2, measure_predicate_lo3,
                         measure_predicate_hi3);
                     if (measure_filter_only) {
                       if (measure_valid && filter_passes) {
                         const double value = resident_dense_measure_value(
                             values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                         local_sum[slot] += value;
                       }
                       local_count[slot] += 1;
                     } else if (measure_valid && filter_passes) {
                       const double value = resident_dense_measure_value(
                           values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                       local_sum[slot] += value;
                       local_count[slot] += 1;
                     }
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t group_offset = local_id; group_offset < groups_in_tile;
                        group_offset += DENSE_GROUP_TILE_LOCAL_SIZE) {
                     double sum = 0.0;
                     uint32_t count = 0;
                     for (size_t lane = 0; lane < DENSE_GROUP_TILE_LOCAL_SIZE; ++lane) {
                       const size_t slot = lane * DENSE_GROUP_MINMAX_TILE_GROUPS + group_offset;
                       sum += local_sum[slot];
                       count += local_count[slot];
                     }
                     const size_t group_idx = tile_start + group_offset;
                     const size_t partial_idx = row_block_idx * groups + group_idx;
                     partial_sum[partial_idx] = sum;
                     partial_count[partial_idx] = count;
                   }
                 });
           }).wait_and_throw();

          q->submit([&](sycl::handler& h) {
             sycl::local_accessor<double, 1> local_min(
                 sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS), h);
             sycl::local_accessor<double, 1> local_max(
                 sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS), h);
             h.parallel_for(
                 sycl::nd_range<1>(sycl::range<1>(workgroup_count * DENSE_GROUP_TILE_LOCAL_SIZE),
                                   sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE)),
                 [=](sycl::nd_item<1> item) {
                   const size_t workgroup_idx = item.get_group(0);
                   const size_t tile_idx = workgroup_idx % tile_count;
                   const size_t row_block_idx = workgroup_idx / tile_count;
                   const size_t local_id = item.get_local_id(0);
                   const size_t tile_start = tile_idx * DENSE_GROUP_MINMAX_TILE_GROUPS;
                   const size_t groups_in_tile =
                       sycl::min(DENSE_GROUP_MINMAX_TILE_GROUPS, groups - tile_start);
                   const size_t local_slots =
                       DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_MINMAX_TILE_GROUPS;
                   const size_t row_start = row_block_idx * DENSE_GROUP_MINMAX_BLOCK_ROWS;
                   const size_t row_end =
                       sycl::min(row_start + DENSE_GROUP_MINMAX_BLOCK_ROWS, row_count);

                   for (size_t slot = local_id; slot < local_slots;
                        slot += DENSE_GROUP_TILE_LOCAL_SIZE) {
                     local_min[slot] = std::numeric_limits<double>::max();
                     local_max[slot] = std::numeric_limits<double>::lowest();
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t row = row_start + local_id; row < row_end;
                        row += DENSE_GROUP_TILE_LOCAL_SIZE) {
                     if (group_nulls && group_nulls[row]) {
                       continue;
                     }
                     const int64_t group_idx64 =
                         static_cast<int64_t>(group_values[row]) - static_cast<int64_t>(group_min);
                     if (group_idx64 < static_cast<int64_t>(tile_start) ||
                         group_idx64 >= static_cast<int64_t>(tile_start + groups_in_tile)) {
                       continue;
                     }

                     const size_t group_offset = static_cast<size_t>(group_idx64) - tile_start;
                     const size_t slot = local_id * DENSE_GROUP_MINMAX_TILE_GROUPS + group_offset;
                     const bool measure_valid = !(value_nulls && value_nulls[row]) &&
                                                !(rhs_required && rhs_nulls && rhs_nulls[row]);
                     const bool filter_passes = resident_dense_filter_passes(
                         row, filter, filter_nulls, values, value_nulls, rhs_values, rhs_nulls,
                         measure_predicate_source, measure_predicate_op,
                         measure_predicate_range_count, measure_predicate_lo0,
                         measure_predicate_hi0, measure_predicate_lo1, measure_predicate_hi1,
                         measure_predicate_lo2, measure_predicate_hi2, measure_predicate_lo3,
                         measure_predicate_hi3);
                     if (measure_valid && filter_passes) {
                       const double value = resident_dense_measure_value(
                           values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                       local_min[slot] = sycl::fmin(local_min[slot], value);
                       local_max[slot] = sycl::fmax(local_max[slot], value);
                     }
                   }
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t group_offset = local_id; group_offset < groups_in_tile;
                        group_offset += DENSE_GROUP_TILE_LOCAL_SIZE) {
                     double min_value = std::numeric_limits<double>::max();
                     double max_value = std::numeric_limits<double>::lowest();
                     for (size_t lane = 0; lane < DENSE_GROUP_TILE_LOCAL_SIZE; ++lane) {
                       const size_t slot = lane * DENSE_GROUP_MINMAX_TILE_GROUPS + group_offset;
                       min_value = sycl::fmin(min_value, local_min[slot]);
                       max_value = sycl::fmax(max_value, local_max[slot]);
                     }
                     const size_t group_idx = tile_start + group_offset;
                     const size_t partial_idx = row_block_idx * groups + group_idx;
                     partial_min[partial_idx] = min_value;
                     partial_max[partial_idx] = max_value;
                   }
                 });
           }).wait_and_throw();

          q->submit([&](sycl::handler& h) {
             sycl::local_accessor<double, 1> local_sum(
                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
             sycl::local_accessor<uint32_t, 1> local_count(
                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
             h.parallel_for(
                 sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE),
                                   sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE)),
                 [=](sycl::nd_item<1> item) {
                   const size_t group_idx = item.get_group(0);
                   const size_t local_id = item.get_local_id(0);
                   double sum = 0.0;
                   uint32_t count = 0;
                   for (size_t block_idx = local_id; block_idx < row_block_count;
                        block_idx += DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE) {
                     const size_t partial_idx = block_idx * groups + group_idx;
                     sum += partial_sum[partial_idx];
                     count += partial_count[partial_idx];
                   }

                   local_sum[local_id] = sum;
                   local_count[local_id] = count;
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t stride = DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE / 2; stride > 0;
                        stride /= 2) {
                     if (local_id < stride) {
                       local_sum[local_id] += local_sum[local_id + stride];
                       local_count[local_id] += local_count[local_id + stride];
                     }
                     item.barrier(sycl::access::fence_space::local_space);
                   }

                   if (local_id == 0) {
                     scratch_sum[group_idx] = local_sum[0];
                     scratch_count[group_idx] = local_count[0];
                   }
                 });
           }).wait_and_throw();

          q->submit([&](sycl::handler& h) {
             sycl::local_accessor<double, 1> local_min(
                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
             sycl::local_accessor<double, 1> local_max(
                 sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE), h);
             h.parallel_for(
                 sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE),
                                   sycl::range<1>(DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE)),
                 [=](sycl::nd_item<1> item) {
                   const size_t group_idx = item.get_group(0);
                   const size_t local_id = item.get_local_id(0);
                   double min_value = std::numeric_limits<double>::max();
                   double max_value = std::numeric_limits<double>::lowest();
                   for (size_t block_idx = local_id; block_idx < row_block_count;
                        block_idx += DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE) {
                     const size_t partial_idx = block_idx * groups + group_idx;
                     min_value = sycl::fmin(min_value, partial_min[partial_idx]);
                     max_value = sycl::fmax(max_value, partial_max[partial_idx]);
                   }

                   local_min[local_id] = min_value;
                   local_max[local_id] = max_value;
                   item.barrier(sycl::access::fence_space::local_space);

                   for (size_t stride = DENSE_GROUP_BLOCK_REDUCE_LOCAL_SIZE / 2; stride > 0;
                        stride /= 2) {
                     if (local_id < stride) {
                       local_min[local_id] =
                           sycl::fmin(local_min[local_id], local_min[local_id + stride]);
                       local_max[local_id] =
                           sycl::fmax(local_max[local_id], local_max[local_id + stride]);
                     }
                     item.barrier(sycl::access::fence_space::local_space);
                   }

                   if (local_id == 0) {
                     scratch_min[group_idx] = local_min[0];
                     scratch_max[group_idx] = local_max[0];
                   }
                 });
           }).wait_and_throw();
        }
      } catch (...) {
        cleanup_partials();
        throw;
      }
      cleanup_partials();
    } else if (!need_min && !need_max &&
               (groups < DENSE_GROUP_SORT_MIN_GROUPS || row_count < DENSE_GROUP_SORT_MIN_ROWS)) {
      const size_t tile_count = (groups + DENSE_GROUP_TILE_GROUPS - 1) / DENSE_GROUP_TILE_GROUPS;
      const size_t local_slots = DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_TILE_GROUPS;
      q->submit([&](sycl::handler& h) {
         sycl::local_accessor<double, 1> local_sum(sycl::range<1>(local_slots), h);
         sycl::local_accessor<uint32_t, 1> local_count(sycl::range<1>(local_slots), h);
         h.parallel_for(
             sycl::nd_range<1>(sycl::range<1>(tile_count * DENSE_GROUP_TILE_LOCAL_SIZE),
                               sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE)),
             [=](sycl::nd_item<1> item) {
               const size_t tile_idx = item.get_group(0);
               const size_t local_id = item.get_local_id(0);
               const size_t tile_start = tile_idx * DENSE_GROUP_TILE_GROUPS;
               const size_t groups_in_tile =
                   sycl::min(DENSE_GROUP_TILE_GROUPS, groups - tile_start);

               for (size_t slot = local_id; slot < local_slots;
                    slot += DENSE_GROUP_TILE_LOCAL_SIZE) {
                 local_sum[slot] = 0.0;
                 local_count[slot] = 0;
               }
               item.barrier(sycl::access::fence_space::local_space);

               for (size_t row = local_id; row < row_count; row += DENSE_GROUP_TILE_LOCAL_SIZE) {
                 if (group_nulls && group_nulls[row]) {
                   continue;
                 }
                 const int64_t group_idx64 =
                     static_cast<int64_t>(group_values[row]) - static_cast<int64_t>(group_min);
                 if (group_idx64 < static_cast<int64_t>(tile_start) ||
                     group_idx64 >= static_cast<int64_t>(tile_start + groups_in_tile)) {
                   continue;
                 }

                 const size_t group_offset = static_cast<size_t>(group_idx64) - tile_start;
                 const size_t slot = local_id * DENSE_GROUP_TILE_GROUPS + group_offset;
                 const bool measure_valid = !(value_nulls && value_nulls[row]) &&
                                            !(rhs_required && rhs_nulls && rhs_nulls[row]);
                 const bool filter_passes = resident_dense_filter_passes(
                     row, filter, filter_nulls, values, value_nulls, rhs_values, rhs_nulls,
                     measure_predicate_source, measure_predicate_op, measure_predicate_range_count,
                     measure_predicate_lo0, measure_predicate_hi0, measure_predicate_lo1,
                     measure_predicate_hi1, measure_predicate_lo2, measure_predicate_hi2,
                     measure_predicate_lo3, measure_predicate_hi3);
                 if (measure_filter_only) {
                   if (measure_valid && filter_passes) {
                     local_sum[slot] += resident_dense_measure_value(
                         values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                   }
                   local_count[slot] += 1;
                 } else if (measure_valid && filter_passes) {
                   local_sum[slot] += resident_dense_measure_value(
                       values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                   local_count[slot] += 1;
                 }
               }
               item.barrier(sycl::access::fence_space::local_space);

               for (size_t group_offset = local_id; group_offset < groups_in_tile;
                    group_offset += DENSE_GROUP_TILE_LOCAL_SIZE) {
                 double sum = 0.0;
                 uint32_t count = 0;
                 for (size_t lane = 0; lane < DENSE_GROUP_TILE_LOCAL_SIZE; ++lane) {
                   const size_t slot = lane * DENSE_GROUP_TILE_GROUPS + group_offset;
                   sum += local_sum[slot];
                   count += local_count[slot];
                 }
                 const size_t group_idx = tile_start + group_offset;
                 scratch_sum[group_idx] = sum;
                 scratch_count[group_idx] = count;
               }
             });
       }).wait_and_throw();
    } else if (groups < DENSE_GROUP_SORT_MIN_GROUPS) {
      const size_t group_local_size =
          groups <= 128 ? DENSE_GROUP_LOW_LOCAL_SIZE : DENSE_GROUP_HIGH_LOCAL_SIZE;
      q->submit([&](sycl::handler& h) {
         sycl::local_accessor<double, 1> local_sum(sycl::range<1>(group_local_size), h);
         sycl::local_accessor<double, 1> local_min(sycl::range<1>(group_local_size), h);
         sycl::local_accessor<double, 1> local_max(sycl::range<1>(group_local_size), h);
         sycl::local_accessor<uint32_t, 1> local_count(sycl::range<1>(group_local_size), h);
         h.parallel_for(
             sycl::nd_range<1>(sycl::range<1>(groups * group_local_size),
                               sycl::range<1>(group_local_size)),
             [=](sycl::nd_item<1> item) {
               const size_t group_idx = item.get_group(0);
               const size_t local_id = item.get_local_id(0);
               const int32_t wanted_group = group_min + static_cast<int32_t>(group_idx);
               double sum = 0.0;
               double min_value = std::numeric_limits<double>::max();
               double max_value = std::numeric_limits<double>::lowest();
               uint32_t count = 0;
               for (size_t row = local_id; row < row_count; row += group_local_size) {
                 if (group_nulls && group_nulls[row]) {
                   continue;
                 }
                 if (group_values[row] != wanted_group)
                   continue;
                 const bool measure_valid = !(value_nulls && value_nulls[row]) &&
                                            !(rhs_required && rhs_nulls && rhs_nulls[row]);
                 const bool filter_passes = resident_dense_filter_passes(
                     row, filter, filter_nulls, values, value_nulls, rhs_values, rhs_nulls,
                     measure_predicate_source, measure_predicate_op, measure_predicate_range_count,
                     measure_predicate_lo0, measure_predicate_hi0, measure_predicate_lo1,
                     measure_predicate_hi1, measure_predicate_lo2, measure_predicate_hi2,
                     measure_predicate_lo3, measure_predicate_hi3);
                 if (measure_filter_only) {
                   if (measure_valid && filter_passes) {
                     const double value = resident_dense_measure_value(
                         values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                     sum += value;
                     min_value = sycl::fmin(min_value, value);
                     max_value = sycl::fmax(max_value, value);
                   }
                   count += 1;
                 } else if (measure_valid && filter_passes) {
                   const double value = resident_dense_measure_value(
                       values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                   sum += value;
                   min_value = sycl::fmin(min_value, value);
                   max_value = sycl::fmax(max_value, value);
                   count += 1;
                 }
               }

               local_sum[local_id] = sum;
               local_min[local_id] = min_value;
               local_max[local_id] = max_value;
               local_count[local_id] = count;
               item.barrier(sycl::access::fence_space::local_space);

               for (size_t stride = group_local_size / 2; stride > 0; stride /= 2) {
                 if (local_id < stride) {
                   local_sum[local_id] += local_sum[local_id + stride];
                   local_min[local_id] =
                       sycl::fmin(local_min[local_id], local_min[local_id + stride]);
                   local_max[local_id] =
                       sycl::fmax(local_max[local_id], local_max[local_id + stride]);
                   local_count[local_id] += local_count[local_id + stride];
                 }
                 item.barrier(sycl::access::fence_space::local_space);
               }
               if (local_id == 0) {
                 scratch_sum[group_idx] = local_sum[0];
                 scratch_min[group_idx] = local_min[0];
                 scratch_max[group_idx] = local_max[0];
                 scratch_count[group_idx] = local_count[0];
               }
             });
       }).wait_and_throw();
    } else if (row_count < DENSE_GROUP_SORT_MIN_ROWS) {
      const size_t tile_count = (groups + DENSE_GROUP_TILE_GROUPS - 1) / DENSE_GROUP_TILE_GROUPS;
      q->submit([&](sycl::handler& h) {
         sycl::local_accessor<double, 1> local_sum(
             sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_TILE_GROUPS), h);
         sycl::local_accessor<double, 1> local_min(
             sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_TILE_GROUPS), h);
         sycl::local_accessor<double, 1> local_max(
             sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_TILE_GROUPS), h);
         sycl::local_accessor<uint32_t, 1> local_count(
             sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_TILE_GROUPS), h);
         h.parallel_for(
             sycl::nd_range<1>(sycl::range<1>(tile_count * DENSE_GROUP_TILE_LOCAL_SIZE),
                               sycl::range<1>(DENSE_GROUP_TILE_LOCAL_SIZE)),
             [=](sycl::nd_item<1> item) {
               const size_t tile_idx = item.get_group(0);
               const size_t local_id = item.get_local_id(0);
               const size_t tile_start = tile_idx * DENSE_GROUP_TILE_GROUPS;
               const size_t groups_in_tile =
                   sycl::min(DENSE_GROUP_TILE_GROUPS, groups - tile_start);
               const size_t local_slots = DENSE_GROUP_TILE_LOCAL_SIZE * DENSE_GROUP_TILE_GROUPS;

               for (size_t slot = local_id; slot < local_slots;
                    slot += DENSE_GROUP_TILE_LOCAL_SIZE) {
                 local_sum[slot] = 0.0;
                 local_min[slot] = std::numeric_limits<double>::max();
                 local_max[slot] = std::numeric_limits<double>::lowest();
                 local_count[slot] = 0;
               }
               item.barrier(sycl::access::fence_space::local_space);

               for (size_t row = local_id; row < row_count; row += DENSE_GROUP_TILE_LOCAL_SIZE) {
                 if (group_nulls && group_nulls[row]) {
                   continue;
                 }
                 const int64_t group_idx64 =
                     static_cast<int64_t>(group_values[row]) - static_cast<int64_t>(group_min);
                 if (group_idx64 < static_cast<int64_t>(tile_start) ||
                     group_idx64 >= static_cast<int64_t>(tile_start + groups_in_tile)) {
                   continue;
                 }

                 const size_t group_offset = static_cast<size_t>(group_idx64) - tile_start;
                 const size_t slot = local_id * DENSE_GROUP_TILE_GROUPS + group_offset;
                 const bool measure_valid = !(value_nulls && value_nulls[row]) &&
                                            !(rhs_required && rhs_nulls && rhs_nulls[row]);
                 const bool filter_passes = resident_dense_filter_passes(
                     row, filter, filter_nulls, values, value_nulls, rhs_values, rhs_nulls,
                     measure_predicate_source, measure_predicate_op, measure_predicate_range_count,
                     measure_predicate_lo0, measure_predicate_hi0, measure_predicate_lo1,
                     measure_predicate_hi1, measure_predicate_lo2, measure_predicate_hi2,
                     measure_predicate_lo3, measure_predicate_hi3);
                 if (measure_filter_only) {
                   if (measure_valid && filter_passes) {
                     const double value = resident_dense_measure_value(
                         values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                     local_sum[slot] += value;
                     local_min[slot] = sycl::fmin(local_min[slot], value);
                     local_max[slot] = sycl::fmax(local_max[slot], value);
                   }
                   local_count[slot] += 1;
                 } else if (measure_valid && filter_passes) {
                   const double value = resident_dense_measure_value(
                       values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                   local_sum[slot] += value;
                   local_min[slot] = sycl::fmin(local_min[slot], value);
                   local_max[slot] = sycl::fmax(local_max[slot], value);
                   local_count[slot] += 1;
                 }
               }
               item.barrier(sycl::access::fence_space::local_space);

               for (size_t group_offset = local_id; group_offset < groups_in_tile;
                    group_offset += DENSE_GROUP_TILE_LOCAL_SIZE) {
                 double sum = 0.0;
                 double min_value = std::numeric_limits<double>::max();
                 double max_value = std::numeric_limits<double>::lowest();
                 uint32_t count = 0;
                 for (size_t lane = 0; lane < DENSE_GROUP_TILE_LOCAL_SIZE; ++lane) {
                   const size_t slot = lane * DENSE_GROUP_TILE_GROUPS + group_offset;
                   sum += local_sum[slot];
                   min_value = sycl::fmin(min_value, local_min[slot]);
                   max_value = sycl::fmax(max_value, local_max[slot]);
                   count += local_count[slot];
                 }
                 const size_t group_idx = tile_start + group_offset;
                 scratch_sum[group_idx] = sum;
                 scratch_min[group_idx] = min_value;
                 scratch_max[group_idx] = max_value;
                 scratch_count[group_idx] = count;
               }
             });
       }).wait_and_throw();
    } else {
      q->parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
         const uint32_t row = static_cast<uint32_t>(id[0]);
         scratch_sorted_group[row] = group_values[row];
         scratch_row_index[row] = row;
       }).wait_and_throw();

      const pgaccel_status sort_status =
          pgaccel_sort_kv_i32(scratch_sorted_group, scratch_row_index, row_count);
      if (sort_status != PGACCEL_OK)
        return fail("sort_kv_i32_failed");

      const uint32_t no_group = std::numeric_limits<uint32_t>::max();
      q->fill(scratch_group_start, no_group, groups).wait_and_throw();
      q->memset(scratch_count, 0, sizeof(uint32_t) * groups).wait_and_throw();

      q->parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
         const uint32_t pos = static_cast<uint32_t>(id[0]);
         const int32_t key = scratch_sorted_group[pos];
         if (pos > 0 && scratch_sorted_group[pos - 1] == key)
           return;
         const int64_t group_idx64 = static_cast<int64_t>(key) - static_cast<int64_t>(group_min);
         if (group_idx64 < 0 || group_idx64 >= static_cast<int64_t>(group_count))
           return;

         uint32_t end = pos + 1;
         while (end < row_count && scratch_sorted_group[end] == key)
           ++end;

         const uint32_t group_idx = static_cast<uint32_t>(group_idx64);
         scratch_group_start[group_idx] = pos;
         scratch_count[group_idx] = end - pos;
       }).wait_and_throw();

      if (!need_min && !need_max) {
        q->submit([&](sycl::handler& h) {
           sycl::local_accessor<double, 1> local_sum(sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE),
                                                     h);
           sycl::local_accessor<uint32_t, 1> local_count(
               sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE), h);
           h.parallel_for(
               sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_HIGH_LOCAL_SIZE),
                                 sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE)),
               [=](sycl::nd_item<1> item) {
                 const size_t group_idx = item.get_group(0);
                 const size_t local_id = item.get_local_id(0);
                 const uint32_t segment_start = scratch_group_start[group_idx];
                 const uint32_t segment_len = scratch_count[group_idx];
                 double sum = 0.0;
                 uint32_t count = 0;

                 if (segment_start != no_group && segment_len != 0) {
                   for (uint32_t offset = static_cast<uint32_t>(local_id); offset < segment_len;
                        offset += static_cast<uint32_t>(DENSE_GROUP_HIGH_LOCAL_SIZE)) {
                     const uint32_t sorted_pos = segment_start + offset;
                     const uint32_t row = scratch_row_index[sorted_pos];
                     if (group_nulls && group_nulls[row]) {
                       continue;
                     }
                     const bool measure_valid = !(value_nulls && value_nulls[row]) &&
                                                !(rhs_required && rhs_nulls && rhs_nulls[row]);
                     const bool filter_passes = resident_dense_filter_passes(
                         row, filter, filter_nulls, values, value_nulls, rhs_values, rhs_nulls,
                         measure_predicate_source, measure_predicate_op,
                         measure_predicate_range_count, measure_predicate_lo0,
                         measure_predicate_hi0, measure_predicate_lo1, measure_predicate_hi1,
                         measure_predicate_lo2, measure_predicate_hi2, measure_predicate_lo3,
                         measure_predicate_hi3);
                     if (measure_filter_only) {
                       if (measure_valid && filter_passes) {
                         sum += resident_dense_measure_value(
                             values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                       }
                       count += 1;
                     } else if (measure_valid && filter_passes) {
                       sum += resident_dense_measure_value(
                           values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                       count += 1;
                     }
                   }
                 }

                 local_sum[local_id] = sum;
                 local_count[local_id] = count;
                 item.barrier(sycl::access::fence_space::local_space);

                 for (size_t stride = DENSE_GROUP_HIGH_LOCAL_SIZE / 2; stride > 0; stride /= 2) {
                   if (local_id < stride) {
                     local_sum[local_id] += local_sum[local_id + stride];
                     local_count[local_id] += local_count[local_id + stride];
                   }
                   item.barrier(sycl::access::fence_space::local_space);
                 }

                 if (local_id == 0) {
                   scratch_sum[group_idx] = local_sum[0];
                   scratch_count[group_idx] = local_count[0];
                 }
               });
         }).wait_and_throw();
      } else {
        q->submit([&](sycl::handler& h) {
           sycl::local_accessor<double, 1> local_sum(sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE),
                                                     h);
           sycl::local_accessor<double, 1> local_min(sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE),
                                                     h);
           sycl::local_accessor<double, 1> local_max(sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE),
                                                     h);
           sycl::local_accessor<uint32_t, 1> local_count(
               sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE), h);
           h.parallel_for(
               sycl::nd_range<1>(sycl::range<1>(groups * DENSE_GROUP_HIGH_LOCAL_SIZE),
                                 sycl::range<1>(DENSE_GROUP_HIGH_LOCAL_SIZE)),
               [=](sycl::nd_item<1> item) {
                 const size_t group_idx = item.get_group(0);
                 const size_t local_id = item.get_local_id(0);
                 const uint32_t segment_start = scratch_group_start[group_idx];
                 const uint32_t segment_len = scratch_count[group_idx];
                 double sum = 0.0;
                 double min_value = std::numeric_limits<double>::max();
                 double max_value = std::numeric_limits<double>::lowest();
                 uint32_t count = 0;

                 if (segment_start != no_group && segment_len != 0) {
                   for (uint32_t offset = static_cast<uint32_t>(local_id); offset < segment_len;
                        offset += static_cast<uint32_t>(DENSE_GROUP_HIGH_LOCAL_SIZE)) {
                     const uint32_t sorted_pos = segment_start + offset;
                     const uint32_t row = scratch_row_index[sorted_pos];
                     if (group_nulls && group_nulls[row]) {
                       continue;
                     }
                     const bool measure_valid = !(value_nulls && value_nulls[row]) &&
                                                !(rhs_required && rhs_nulls && rhs_nulls[row]);
                     const bool filter_passes = resident_dense_filter_passes(
                         row, filter, filter_nulls, values, value_nulls, rhs_values, rhs_nulls,
                         measure_predicate_source, measure_predicate_op,
                         measure_predicate_range_count, measure_predicate_lo0,
                         measure_predicate_hi0, measure_predicate_lo1, measure_predicate_hi1,
                         measure_predicate_lo2, measure_predicate_hi2, measure_predicate_lo3,
                         measure_predicate_hi3);
                     if (measure_filter_only) {
                       if (measure_valid && filter_passes) {
                         const double value = resident_dense_measure_value(
                             values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                         sum += value;
                         min_value = sycl::fmin(min_value, value);
                         max_value = sycl::fmax(max_value, value);
                       }
                       count += 1;
                     } else if (measure_valid && filter_passes) {
                       const double value = resident_dense_measure_value(
                           values[row], rhs_required ? rhs_values[row] : 0.0, measure_op);
                       sum += value;
                       min_value = sycl::fmin(min_value, value);
                       max_value = sycl::fmax(max_value, value);
                       count += 1;
                     }
                   }
                 }

                 local_sum[local_id] = sum;
                 local_min[local_id] = min_value;
                 local_max[local_id] = max_value;
                 local_count[local_id] = count;
                 item.barrier(sycl::access::fence_space::local_space);

                 for (size_t stride = DENSE_GROUP_HIGH_LOCAL_SIZE / 2; stride > 0; stride /= 2) {
                   if (local_id < stride) {
                     local_sum[local_id] += local_sum[local_id + stride];
                     local_min[local_id] =
                         sycl::fmin(local_min[local_id], local_min[local_id + stride]);
                     local_max[local_id] =
                         sycl::fmax(local_max[local_id], local_max[local_id + stride]);
                     local_count[local_id] += local_count[local_id + stride];
                   }
                   item.barrier(sycl::access::fence_space::local_space);
                 }

                 if (local_id == 0) {
                   scratch_sum[group_idx] = local_sum[0];
                   scratch_min[group_idx] = local_min[0];
                   scratch_max[group_idx] = local_max[0];
                   scratch_count[group_idx] = local_count[0];
                 }
               });
         }).wait_and_throw();
      }
    }

    q->memcpy(out_sum_by_group, scratch_sum, sizeof(double) * groups).wait();
    if (need_min)
      q->memcpy(out_min_by_group, scratch_min, sizeof(double) * groups).wait();
    if (need_max)
      q->memcpy(out_max_by_group, scratch_max, sizeof(double) * groups).wait();
    q->memcpy(out_count_by_group, scratch_count, sizeof(uint32_t) * groups).wait();
    size_t selected_host = 0;
    for (size_t i = 0; i < groups; ++i)
      selected_host += static_cast<size_t>(out_count_by_group[i]);

    *selected_count = selected_host;
    *uncertain_count = 0;
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident dense grouped f64 kernel failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: resident dense grouped f64 kernel failed (unknown)\n");
    return PGACCEL_ERROR;
  }
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v8(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, int32_t measure_predicate_op, int32_t measure_predicate_source,
    int32_t measure_predicate_range_count, double measure_predicate_lo0,
    double measure_predicate_hi0, double measure_predicate_lo1, double measure_predicate_hi1,
    double measure_predicate_lo2, double measure_predicate_hi2, double measure_predicate_lo3,
    double measure_predicate_hi3, pgaccel_expr_usm_col filter_col, size_t row_count,
    int32_t group_min, int32_t group_count, double* scratch_sum, double* scratch_min,
    double* scratch_max, uint32_t* scratch_count, uint32_t* scratch_group_start,
    uint32_t* scratch_group_cursor, size_t scratch_group_capacity, int32_t* scratch_sorted_group,
    uint32_t* scratch_row_index, size_t scratch_row_capacity, double* out_sum_by_group,
    double* out_min_by_group, double* out_max_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count) {
  return pgaccel_expr_template_resident_dense_grouped_f64_usm_v9(
      group_col, value_col, value_rhs_col, measure_op, aggregate_mask, filter_mode,
      measure_predicate_op, measure_predicate_source, measure_predicate_range_count,
      measure_predicate_lo0, measure_predicate_hi0, measure_predicate_lo1, measure_predicate_hi1,
      measure_predicate_lo2, measure_predicate_hi2, measure_predicate_lo3, measure_predicate_hi3,
      filter_col, row_count, group_min, group_count, scratch_sum, scratch_min, scratch_max,
      scratch_count, scratch_group_start, scratch_group_cursor, scratch_group_capacity,
      scratch_sorted_group, scratch_row_index, scratch_row_capacity, nullptr, nullptr, nullptr,
      nullptr, 0, out_sum_by_group, out_min_by_group, out_max_by_group, out_count_by_group,
      out_group_capacity, selected_count, uncertain_count);
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v7(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, int32_t measure_predicate_op, int32_t measure_predicate_range_count,
    double measure_predicate_lo0, double measure_predicate_hi0, double measure_predicate_lo1,
    double measure_predicate_hi1, double measure_predicate_lo2, double measure_predicate_hi2,
    double measure_predicate_lo3, double measure_predicate_hi3, pgaccel_expr_usm_col filter_col,
    size_t row_count, int32_t group_min, int32_t group_count, double* scratch_sum,
    double* scratch_min, double* scratch_max, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_cursor, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_min_by_group, double* out_max_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count) {
  return pgaccel_expr_template_resident_dense_grouped_f64_usm_v8(
      group_col, value_col, value_rhs_col, measure_op, aggregate_mask, filter_mode,
      measure_predicate_op, DENSE_GROUP_MEASURE_PRED_SOURCE_RHS, measure_predicate_range_count,
      measure_predicate_lo0, measure_predicate_hi0, measure_predicate_lo1, measure_predicate_hi1,
      measure_predicate_lo2, measure_predicate_hi2, measure_predicate_lo3, measure_predicate_hi3,
      filter_col, row_count, group_min, group_count, scratch_sum, scratch_min, scratch_max,
      scratch_count, scratch_group_start, scratch_group_cursor, scratch_group_capacity,
      scratch_sorted_group, scratch_row_index, scratch_row_capacity, out_sum_by_group,
      out_min_by_group, out_max_by_group, out_count_by_group, out_group_capacity, selected_count,
      uncertain_count);
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v5(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, pgaccel_expr_usm_col filter_col, size_t row_count, int32_t group_min,
    int32_t group_count, double* scratch_sum, double* scratch_min, double* scratch_max,
    uint32_t* scratch_count, uint32_t* scratch_group_start, uint32_t* scratch_group_cursor,
    size_t scratch_group_capacity, int32_t* scratch_sorted_group, uint32_t* scratch_row_index,
    size_t scratch_row_capacity, double* out_sum_by_group, double* out_min_by_group,
    double* out_max_by_group, uint32_t* out_count_by_group, size_t out_group_capacity,
    size_t* selected_count, size_t* uncertain_count) {
  return pgaccel_expr_template_resident_dense_grouped_f64_usm_v6(
      group_col, value_col, value_rhs_col, measure_op, aggregate_mask, filter_mode,
      DENSE_GROUP_MEASURE_PRED_BOOL_ONLY, 0.0, 0.0, filter_col, row_count, group_min, group_count,
      scratch_sum, scratch_min, scratch_max, scratch_count, scratch_group_start,
      scratch_group_cursor, scratch_group_capacity, scratch_sorted_group, scratch_row_index,
      scratch_row_capacity, out_sum_by_group, out_min_by_group, out_max_by_group,
      out_count_by_group, out_group_capacity, selected_count, uncertain_count);
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v6(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    int32_t filter_mode, int32_t measure_predicate_op, double measure_predicate_arg1,
    double measure_predicate_arg2, pgaccel_expr_usm_col filter_col, size_t row_count,
    int32_t group_min, int32_t group_count, double* scratch_sum, double* scratch_min,
    double* scratch_max, uint32_t* scratch_count, uint32_t* scratch_group_start,
    uint32_t* scratch_group_cursor, size_t scratch_group_capacity, int32_t* scratch_sorted_group,
    uint32_t* scratch_row_index, size_t scratch_row_capacity, double* out_sum_by_group,
    double* out_min_by_group, double* out_max_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count) {
  const int32_t range_count = measure_predicate_op == DENSE_GROUP_MEASURE_PRED_BOOL_ONLY ? 0 : 1;
  return pgaccel_expr_template_resident_dense_grouped_f64_usm_v7(
      group_col, value_col, value_rhs_col, measure_op, aggregate_mask, filter_mode,
      measure_predicate_op, range_count, measure_predicate_arg1, measure_predicate_arg2, 0.0, 0.0,
      0.0, 0.0, 0.0, 0.0, filter_col, row_count, group_min, group_count, scratch_sum, scratch_min,
      scratch_max, scratch_count, scratch_group_start, scratch_group_cursor, scratch_group_capacity,
      scratch_sorted_group, scratch_row_index, scratch_row_capacity, out_sum_by_group,
      out_min_by_group, out_max_by_group, out_count_by_group, out_group_capacity, selected_count,
      uncertain_count);
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v4(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, uint32_t aggregate_mask,
    pgaccel_expr_usm_col filter_col, size_t row_count, int32_t group_min, int32_t group_count,
    double* scratch_sum, double* scratch_min, double* scratch_max, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_cursor, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_min_by_group, double* out_max_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count) {
  return pgaccel_expr_template_resident_dense_grouped_f64_usm_v5(
      group_col, value_col, value_rhs_col, measure_op, aggregate_mask, DENSE_GROUP_FILTER_ROWS,
      filter_col, row_count, group_min, group_count, scratch_sum, scratch_min, scratch_max,
      scratch_count, scratch_group_start, scratch_group_cursor, scratch_group_capacity,
      scratch_sorted_group, scratch_row_index, scratch_row_capacity, out_sum_by_group,
      out_min_by_group, out_max_by_group, out_count_by_group, out_group_capacity, selected_count,
      uncertain_count);
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v3(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col,
    pgaccel_expr_usm_col value_rhs_col, int32_t measure_op, pgaccel_expr_usm_col filter_col,
    size_t row_count, int32_t group_min, int32_t group_count, double* scratch_sum,
    double* scratch_min, double* scratch_max, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_cursor, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_min_by_group, double* out_max_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count) {
  return pgaccel_expr_template_resident_dense_grouped_f64_usm_v4(
      group_col, value_col, value_rhs_col, measure_op, DENSE_GROUP_AGG_ALL, filter_col, row_count,
      group_min, group_count, scratch_sum, scratch_min, scratch_max, scratch_count,
      scratch_group_start, scratch_group_cursor, scratch_group_capacity, scratch_sorted_group,
      scratch_row_index, scratch_row_capacity, out_sum_by_group, out_min_by_group, out_max_by_group,
      out_count_by_group, out_group_capacity, selected_count, uncertain_count);
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm_v2(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col, pgaccel_expr_usm_col filter_col,
    size_t row_count, int32_t group_min, int32_t group_count, double* scratch_sum,
    double* scratch_min, double* scratch_max, uint32_t* scratch_count,
    uint32_t* scratch_group_start, uint32_t* scratch_group_cursor, size_t scratch_group_capacity,
    int32_t* scratch_sorted_group, uint32_t* scratch_row_index, size_t scratch_row_capacity,
    double* out_sum_by_group, double* out_min_by_group, double* out_max_by_group,
    uint32_t* out_count_by_group, size_t out_group_capacity, size_t* selected_count,
    size_t* uncertain_count) {
  pgaccel_expr_usm_col value_rhs_col{};
  value_rhs_col.values = nullptr;
  value_rhs_col.nulls = nullptr;
  value_rhs_col.type = PGACCEL_VAL_NULL;
  return pgaccel_expr_template_resident_dense_grouped_f64_usm_v3(
      group_col, value_col, value_rhs_col, 0, filter_col, row_count, group_min, group_count,
      scratch_sum, scratch_min, scratch_max, scratch_count, scratch_group_start,
      scratch_group_cursor, scratch_group_capacity, scratch_sorted_group, scratch_row_index,
      scratch_row_capacity, out_sum_by_group, out_min_by_group, out_max_by_group,
      out_count_by_group, out_group_capacity, selected_count, uncertain_count);
}

extern "C" pgaccel_status pgaccel_expr_template_resident_dense_grouped_f64_usm(
    pgaccel_expr_usm_col group_col, pgaccel_expr_usm_col value_col, pgaccel_expr_usm_col filter_col,
    size_t row_count, int32_t group_min, int32_t group_count, double* scratch_sum,
    uint32_t* scratch_count, uint32_t* scratch_group_start, uint32_t* scratch_group_cursor,
    size_t scratch_group_capacity, int32_t* scratch_sorted_group, uint32_t* scratch_row_index,
    size_t scratch_row_capacity, double* out_sum_by_group, uint32_t* out_count_by_group,
    size_t out_group_capacity, size_t* selected_count, size_t* uncertain_count) {
  if (row_count == 0) {
    return pgaccel_expr_template_resident_dense_grouped_f64_usm_v2(
        group_col, value_col, filter_col, row_count, group_min, group_count, scratch_sum, nullptr,
        nullptr, scratch_count, scratch_group_start, scratch_group_cursor, scratch_group_capacity,
        scratch_sorted_group, scratch_row_index, scratch_row_capacity, out_sum_by_group, nullptr,
        nullptr, out_count_by_group, out_group_capacity, selected_count, uncertain_count);
  }
  if (group_count <= 0)
    return PGACCEL_ERROR;
  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t groups = static_cast<size_t>(group_count);
  double* scratch_min = nullptr;
  double* scratch_max = nullptr;
  std::vector<double> min_by_group(groups, 0.0);
  std::vector<double> max_by_group(groups, 0.0);
  try {
    scratch_min = sycl::malloc_device<double>(groups, *q);
    scratch_max = sycl::malloc_device<double>(groups, *q);
    if (!scratch_min || !scratch_max)
      throw std::bad_alloc();

    const pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v2(
        group_col, value_col, filter_col, row_count, group_min, group_count, scratch_sum,
        scratch_min, scratch_max, scratch_count, scratch_group_start, scratch_group_cursor,
        scratch_group_capacity, scratch_sorted_group, scratch_row_index, scratch_row_capacity,
        out_sum_by_group, min_by_group.data(), max_by_group.data(), out_count_by_group,
        out_group_capacity, selected_count, uncertain_count);
    sycl::free(scratch_min, *q);
    sycl::free(scratch_max, *q);
    return status;
  } catch (...) {
    if (scratch_min)
      sycl::free(scratch_min, *q);
    if (scratch_max)
      sycl::free(scratch_max, *q);
    return PGACCEL_ERROR;
  }
}
