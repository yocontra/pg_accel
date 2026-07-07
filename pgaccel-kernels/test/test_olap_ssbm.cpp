#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"

namespace {

static int g_pass = 0;
static int g_fail = 0;

#define ASSERT_TRUE(desc, cond)                 \
  do {                                          \
    if (cond) {                                 \
      g_pass++;                                 \
    } else {                                    \
      std::fprintf(stderr, "FAIL: %s\n", desc); \
      g_fail++;                                 \
    }                                           \
  } while (0)

#define ASSERT_STATUS_OK(desc, status)                                              \
  do {                                                                              \
    if ((status) == PGACCEL_OK) {                                                   \
      g_pass++;                                                                     \
    } else {                                                                        \
      std::fprintf(stderr, "FAIL: %s status=%d\n", desc, static_cast<int>(status)); \
      g_fail++;                                                                     \
    }                                                                               \
  } while (0)

template <typename T>
T* alloc_shared_array(const char* desc, size_t count) {
  void* ptr = nullptr;
  pgaccel_status status = pgaccel_expr_shared_alloc(count * sizeof(T), &ptr);
  ASSERT_STATUS_OK(desc, status);
  ASSERT_TRUE(desc, ptr != nullptr);
  return static_cast<T*>(ptr);
}

template <typename T>
T* alloc_device_copy(const char* desc, const T* src, size_t count) {
  void* ptr = nullptr;
  pgaccel_status status = pgaccel_expr_device_alloc_copy(src, count * sizeof(T), &ptr);
  ASSERT_STATUS_OK(desc, status);
  ASSERT_TRUE(desc, ptr != nullptr);
  return static_cast<T*>(ptr);
}

template <typename T>
T* alloc_device_array(const char* desc, size_t count) {
  void* ptr = nullptr;
  pgaccel_status status = pgaccel_expr_device_alloc(count * sizeof(T), &ptr);
  ASSERT_STATUS_OK(desc, status);
  ASSERT_TRUE(desc, ptr != nullptr);
  return static_cast<T*>(ptr);
}

void free_shared(void* ptr) {
  pgaccel_expr_shared_free(ptr);
}

void free_device(void* ptr) {
  pgaccel_expr_device_free(ptr);
}

void test_resident_sort_kv_i32_device_radix() {
  constexpr size_t N = 65536;
  std::vector<int32_t> original_keys(N);
  std::vector<uint32_t> original_indices(N);
  for (size_t i = 0; i < N; ++i) {
    original_keys[i] = static_cast<int32_t>((i * 37) % 2048) - 1024;
    original_indices[i] = static_cast<uint32_t>(i);
  }

  int32_t* keys = alloc_device_array<int32_t>("resident i32 kv sort keys", N);
  uint32_t* indices = alloc_device_array<uint32_t>("resident i32 kv sort indices", N);
  ASSERT_TRUE("resident i32 kv sort buffers", keys != nullptr && indices != nullptr);
  if (keys == nullptr || indices == nullptr) {
    free_device(keys);
    free_device(indices);
    return;
  }

  ASSERT_STATUS_OK("resident i32 kv sort copy keys",
                   pgaccel_expr_device_copy_from_host(keys, original_keys.data(),
                                                      N * sizeof(int32_t)));
  ASSERT_STATUS_OK("resident i32 kv sort copy indices",
                   pgaccel_expr_device_copy_from_host(indices, original_indices.data(),
                                                      N * sizeof(uint32_t)));

  const pgaccel_status status = pgaccel_sort_kv_i32_device(keys, indices, N);
  ASSERT_STATUS_OK("resident i32 kv sort status", status);

  std::vector<int32_t> sorted_keys(N);
  std::vector<uint32_t> sorted_indices(N);
  ASSERT_STATUS_OK("resident i32 kv sort read keys",
                   pgaccel_expr_device_copy_to_host(sorted_keys.data(), keys,
                                                    N * sizeof(int32_t)));
  ASSERT_STATUS_OK("resident i32 kv sort read indices",
                   pgaccel_expr_device_copy_to_host(sorted_indices.data(), indices,
                                                    N * sizeof(uint32_t)));

  bool sorted = true;
  bool indices_match = true;
  bool stable = true;
  for (size_t i = 0; i < N; ++i) {
    const uint32_t idx = sorted_indices[i];
    if (idx >= N || original_keys[idx] != sorted_keys[i])
      indices_match = false;
    if (i > 0) {
      if (sorted_keys[i] < sorted_keys[i - 1])
        sorted = false;
      if (sorted_keys[i] == sorted_keys[i - 1] && sorted_indices[i] < sorted_indices[i - 1])
        stable = false;
    }
  }
  ASSERT_TRUE("resident i32 kv sort sorted", sorted);
  ASSERT_TRUE("resident i32 kv sort indices", indices_match);
  ASSERT_TRUE("resident i32 kv sort stable", stable);

  free_device(keys);
  free_device(indices);
}

pgaccel_expr_usm_col i32_col(const int32_t* values, const uint8_t* nulls = nullptr) {
  pgaccel_expr_usm_col col = {};
  col.values = values;
  col.nulls = nulls;
  col.type = PGACCEL_VAL_INT32;
  return col;
}

pgaccel_expr_usm_col f64_col(const double* values, const uint8_t* nulls = nullptr) {
  pgaccel_expr_usm_col col = {};
  col.values = values;
  col.nulls = nulls;
  col.type = PGACCEL_VAL_FLOAT64;
  return col;
}

pgaccel_expr_usm_col bool_col(const uint8_t* values, const uint8_t* nulls = nullptr) {
  pgaccel_expr_usm_col col = {};
  col.values = values;
  col.nulls = nulls;
  col.type = PGACCEL_VAL_BOOL;
  return col;
}

void fill_fixture(int32_t* orderdate, int32_t* discount, int32_t* quantity, int32_t* extendedprice,
                  size_t count) {
  const int32_t orderdate_values[] = {19930001, 19930002, 19930003, 19930004, 19940101, 19940102,
                                      19940103, 19940104, 19940040, 19940041, 19940042, 19940043};
  const int32_t discount_values[] = {1, 2, 3, 4, 4, 5, 6, 7, 5, 6, 7, 8};
  const int32_t quantity_values[] = {10, 20, 24, 25, 26, 30, 35, 36, 26, 31, 35, 40};
  const int32_t price_values[] = {100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1100, 1200};

  for (size_t i = 0; i < count; ++i) {
    orderdate[i] = orderdate_values[i];
    discount[i] = discount_values[i];
    quantity[i] = quantity_values[i];
    extendedprice[i] = price_values[i];
  }
}

bool run_kernel_case(const char* label, pgaccel_expr_usm_col orderdate_col,
                     pgaccel_expr_usm_col discount_col, pgaccel_expr_usm_col quantity_col,
                     pgaccel_expr_usm_col extendedprice_col, size_t row_count, int32_t orderdate_lo,
                     int32_t orderdate_hi, const int32_t* orderdate_keys,
                     size_t orderdate_key_count, int32_t discount_lo, int32_t discount_hi,
                     int32_t quantity_lo, int32_t quantity_hi, int64_t expected_revenue,
                     size_t expected_count) {
  int64_t revenue = -1;
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_ssbm_q1_revenue_i64_usm(
      orderdate_col, discount_col, quantity_col, extendedprice_col, row_count, orderdate_lo,
      orderdate_hi, orderdate_keys, orderdate_key_count, discount_lo, discount_hi, quantity_lo,
      quantity_hi, &revenue, &selected, &uncertain);
  if (status != PGACCEL_OK) {
    std::fprintf(stderr, "FAIL: %s status=%d\n", label, static_cast<int>(status));
    g_fail++;
    return false;
  }
  g_pass++;
  if (revenue != expected_revenue || selected != expected_count || uncertain != 0) {
    std::fprintf(stderr,
                 "FAIL: %s got revenue=%lld count=%zu uncertain=%zu expected revenue=%lld "
                 "count=%zu uncertain=0\n",
                 label, static_cast<long long>(revenue), selected, uncertain,
                 static_cast<long long>(expected_revenue), expected_count);
    g_fail++;
    return false;
  }
  g_pass++;
  return true;
}

bool run_kernel_case_with_scratch(
    const char* label, pgaccel_expr_usm_col orderdate_col, pgaccel_expr_usm_col discount_col,
    pgaccel_expr_usm_col quantity_col, pgaccel_expr_usm_col extendedprice_col, size_t row_count,
    int32_t orderdate_lo, int32_t orderdate_hi, const int32_t* orderdate_keys,
    size_t orderdate_key_count, int32_t discount_lo, int32_t discount_hi, int32_t quantity_lo,
    int32_t quantity_hi, int64_t expected_revenue, size_t expected_count) {
  const size_t scratch_items = pgaccel_expr_template_ssbm_q1_scratch_items(row_count);
  int64_t* revenue_a = alloc_device_array<int64_t>("scratch revenue_a", scratch_items);
  int64_t* count_a = alloc_device_array<int64_t>("scratch count_a", scratch_items);
  int64_t* revenue_b = alloc_device_array<int64_t>("scratch revenue_b", scratch_items);
  int64_t* count_b = alloc_device_array<int64_t>("scratch count_b", scratch_items);

  int64_t revenue = -1;
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_ssbm_q1_revenue_i64_usm_scratch(
      orderdate_col, discount_col, quantity_col, extendedprice_col, row_count, orderdate_lo,
      orderdate_hi, orderdate_keys, orderdate_key_count, discount_lo, discount_hi, quantity_lo,
      quantity_hi, revenue_a, count_a, revenue_b, count_b, scratch_items, &revenue, &selected,
      &uncertain);

  free_device(revenue_a);
  free_device(count_a);
  free_device(revenue_b);
  free_device(count_b);

  if (status != PGACCEL_OK) {
    std::fprintf(stderr, "FAIL: %s status=%d\n", label, static_cast<int>(status));
    g_fail++;
    return false;
  }
  g_pass++;
  if (revenue != expected_revenue || selected != expected_count || uncertain != 0) {
    std::fprintf(stderr,
                 "FAIL: %s got revenue=%lld count=%zu uncertain=%zu expected revenue=%lld "
                 "count=%zu uncertain=0\n",
                 label, static_cast<long long>(revenue), selected, uncertain,
                 static_cast<long long>(expected_revenue), expected_count);
    g_fail++;
    return false;
  }
  g_pass++;
  return true;
}

void test_range_filters() {
  constexpr size_t N = 12;
  int32_t* orderdate = alloc_shared_array<int32_t>("range orderdate", N);
  int32_t* discount = alloc_shared_array<int32_t>("range discount", N);
  int32_t* quantity = alloc_shared_array<int32_t>("range quantity", N);
  int32_t* extendedprice = alloc_shared_array<int32_t>("range extendedprice", N);
  fill_fixture(orderdate, discount, quantity, extendedprice, N);

  run_kernel_case("SSBM Q1.1-style date range", i32_col(orderdate), i32_col(discount),
                  i32_col(quantity), i32_col(extendedprice), N, 19930001, 19930099, nullptr, 0, 1,
                  3, 0, 24, 1400, 3);

  run_kernel_case_with_scratch("SSBM Q1.1-style date range with resident scratch",
                               i32_col(orderdate), i32_col(discount), i32_col(quantity),
                               i32_col(extendedprice), N, 19930001, 19930099, nullptr, 0, 1, 3, 0,
                               24, 1400, 3);

  free_shared(orderdate);
  free_shared(discount);
  free_shared(quantity);
  free_shared(extendedprice);
}

void test_date_membership_and_nulls() {
  constexpr size_t N = 12;
  int32_t* orderdate = alloc_shared_array<int32_t>("membership orderdate", N);
  int32_t* discount = alloc_shared_array<int32_t>("membership discount", N);
  int32_t* quantity = alloc_shared_array<int32_t>("membership quantity", N);
  int32_t* extendedprice = alloc_shared_array<int32_t>("membership extendedprice", N);
  uint8_t* price_nulls = alloc_shared_array<uint8_t>("membership price nulls", N);
  int32_t* date_keys = alloc_shared_array<int32_t>("membership date keys", 2);
  fill_fixture(orderdate, discount, quantity, extendedprice, N);
  std::memset(price_nulls, 0, N);
  price_nulls[9] = 1;
  date_keys[0] = 19940040;
  date_keys[1] = 19940041;

  run_kernel_case("SSBM Q1.3-style date membership with null", i32_col(orderdate),
                  i32_col(discount), i32_col(quantity), i32_col(extendedprice, price_nulls), N, 0,
                  0, date_keys, 2, 5, 7, 26, 35, 4500, 1);

  free_shared(orderdate);
  free_shared(discount);
  free_shared(quantity);
  free_shared(extendedprice);
  free_shared(price_nulls);
  free_shared(date_keys);
}

void test_device_resident_membership() {
  constexpr size_t N = 12;
  int32_t orderdate_host[N];
  int32_t discount_host[N];
  int32_t quantity_host[N];
  int32_t extendedprice_host[N];
  int32_t date_keys_host[2] = {19940040, 19940041};
  fill_fixture(orderdate_host, discount_host, quantity_host, extendedprice_host, N);

  int32_t* orderdate = alloc_device_copy<int32_t>("device orderdate", orderdate_host, N);
  int32_t* discount = alloc_device_copy<int32_t>("device discount", discount_host, N);
  int32_t* quantity = alloc_device_copy<int32_t>("device quantity", quantity_host, N);
  int32_t* extendedprice =
      alloc_device_copy<int32_t>("device extendedprice", extendedprice_host, N);
  int32_t* date_keys = alloc_device_copy<int32_t>("device date keys", date_keys_host, 2);

  run_kernel_case("SSBM Q1 device-resident date membership", i32_col(orderdate), i32_col(discount),
                  i32_col(quantity), i32_col(extendedprice), N, 0, 0, date_keys, 2, 5, 7, 26, 35,
                  10500, 2);

  free_device(orderdate);
  free_device(discount);
  free_device(quantity);
  free_device(extendedprice);
  free_device(date_keys);
}

void test_q2_grouped_revenue() {
  constexpr size_t N = 8;
  int32_t* orderdate = alloc_shared_array<int32_t>("q2 orderdate", N);
  int32_t* partkey = alloc_shared_array<int32_t>("q2 partkey", N);
  int32_t* suppkey = alloc_shared_array<int32_t>("q2 suppkey", N);
  int32_t* revenue = alloc_shared_array<int32_t>("q2 revenue", N);

  const int32_t orderdate_host[N] = {100, 101, 102, 103, 100, 101, 102, 103};
  const int32_t partkey_host[N] = {1, 2, 3, 4, 2, 3, 4, 1};
  const int32_t suppkey_host[N] = {1, 1, 1, 1, 2, 2, 2, 2};
  const int32_t revenue_host[N] = {10, 20, 30, 40, 50, 60, 70, 80};
  for (size_t i = 0; i < N; ++i) {
    orderdate[i] = orderdate_host[i];
    partkey[i] = partkey_host[i];
    suppkey[i] = suppkey_host[i];
    revenue[i] = revenue_host[i];
  }

  const int32_t date_year_host[4] = {1992, 1992, 1993, 1993};
  const int32_t part_brand_host[5] = {-1, 0, 1, 2, 3};
  const uint8_t part_match_host[5] = {0, 1, 1, 0, 1};
  const uint8_t supplier_match_host[3] = {0, 1, 0};

  int32_t* date_year = alloc_device_copy<int32_t>("q2 date year by offset", date_year_host, 4);
  int32_t* part_brand = alloc_device_copy<int32_t>("q2 part brand code", part_brand_host, 5);
  uint8_t* part_match = alloc_device_copy<uint8_t>("q2 part match", part_match_host, 5);
  uint8_t* supplier_match = alloc_device_copy<uint8_t>("q2 supplier match", supplier_match_host, 3);

  int64_t revenue_by_group[8];
  uint32_t count_by_group[8];
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_ssbm_q2_grouped_revenue_i64_usm(
      i32_col(orderdate), i32_col(partkey), i32_col(suppkey), i32_col(revenue), N, 100, date_year,
      4, part_brand, part_match, 5, supplier_match, 3, 1992, 2, 4, revenue_by_group, count_by_group,
      8, &selected, &uncertain);

  ASSERT_STATUS_OK("SSBM Q2 grouped revenue status", status);
  ASSERT_TRUE("SSBM Q2 selected count", selected == 3);
  ASSERT_TRUE("SSBM Q2 uncertain count", uncertain == 0);

  const int64_t expected_revenue[8] = {10, 20, 0, 0, 0, 0, 0, 40};
  const uint32_t expected_count[8] = {1, 1, 0, 0, 0, 0, 0, 1};
  for (size_t i = 0; i < 8; ++i) {
    ASSERT_TRUE("SSBM Q2 revenue group", revenue_by_group[i] == expected_revenue[i]);
    ASSERT_TRUE("SSBM Q2 count group", count_by_group[i] == expected_count[i]);
  }

  free_device(date_year);
  free_device(part_brand);
  free_device(part_match);
  free_device(supplier_match);
  free_shared(orderdate);
  free_shared(partkey);
  free_shared(suppkey);
  free_shared(revenue);
}

void test_q3_grouped_revenue() {
  constexpr size_t N = 8;
  int32_t* orderdate = alloc_shared_array<int32_t>("q3 orderdate", N);
  int32_t* custkey = alloc_shared_array<int32_t>("q3 custkey", N);
  int32_t* suppkey = alloc_shared_array<int32_t>("q3 suppkey", N);
  int32_t* revenue = alloc_shared_array<int32_t>("q3 revenue", N);

  const int32_t orderdate_host[N] = {100, 101, 102, 103, 100, 101, 102, 103};
  const int32_t custkey_host[N] = {1, 2, 1, 2, 3, 1, 2, 1};
  const int32_t suppkey_host[N] = {1, 1, 2, 2, 1, 2, 1, 3};
  const int32_t revenue_host[N] = {10, 20, 30, 40, 50, 60, 70, 80};
  for (size_t i = 0; i < N; ++i) {
    orderdate[i] = orderdate_host[i];
    custkey[i] = custkey_host[i];
    suppkey[i] = suppkey_host[i];
    revenue[i] = revenue_host[i];
  }

  const int32_t date_year_host[4] = {1992, 1992, 1993, 1993};
  const uint8_t date_match_host[4] = {1, 1, 1, 0};
  const int32_t customer_code_host[4] = {-1, 0, 1, 0};
  const uint8_t customer_match_host[4] = {0, 1, 1, 0};
  const int32_t supplier_code_host[4] = {-1, 0, 1, 0};
  const uint8_t supplier_match_host[4] = {0, 1, 1, 0};

  int32_t* date_year = alloc_device_copy<int32_t>("q3 date year by offset", date_year_host, 4);
  uint8_t* date_match = alloc_device_copy<uint8_t>("q3 date match", date_match_host, 4);
  int32_t* customer_code =
      alloc_device_copy<int32_t>("q3 customer group code", customer_code_host, 4);
  uint8_t* customer_match = alloc_device_copy<uint8_t>("q3 customer match", customer_match_host, 4);
  int32_t* supplier_code =
      alloc_device_copy<int32_t>("q3 supplier group code", supplier_code_host, 4);
  uint8_t* supplier_match = alloc_device_copy<uint8_t>("q3 supplier match", supplier_match_host, 4);

  int64_t revenue_by_group[8];
  uint32_t count_by_group[8];
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_ssbm_q3_grouped_revenue_i64_usm(
      i32_col(orderdate), i32_col(custkey), i32_col(suppkey), i32_col(revenue), N, 100, date_year,
      date_match, 4, customer_code, customer_match, 4, supplier_code, supplier_match, 4, 1992, 2, 2,
      2, revenue_by_group, count_by_group, 8, &selected, &uncertain);

  ASSERT_STATUS_OK("SSBM Q3 grouped revenue status", status);
  ASSERT_TRUE("SSBM Q3 selected count", selected == 5);
  ASSERT_TRUE("SSBM Q3 uncertain count", uncertain == 0);

  const int64_t expected_revenue[8] = {10, 60, 20, 0, 0, 30, 70, 0};
  const uint32_t expected_count[8] = {1, 1, 1, 0, 0, 1, 1, 0};
  for (size_t i = 0; i < 8; ++i) {
    ASSERT_TRUE("SSBM Q3 revenue group", revenue_by_group[i] == expected_revenue[i]);
    ASSERT_TRUE("SSBM Q3 count group", count_by_group[i] == expected_count[i]);
  }

  free_device(date_year);
  free_device(date_match);
  free_device(customer_code);
  free_device(customer_match);
  free_device(supplier_code);
  free_device(supplier_match);
  free_shared(orderdate);
  free_shared(custkey);
  free_shared(suppkey);
  free_shared(revenue);
}

void test_q4_grouped_profit() {
  constexpr size_t N = 8;
  int32_t* orderdate = alloc_shared_array<int32_t>("q4 orderdate", N);
  int32_t* custkey = alloc_shared_array<int32_t>("q4 custkey", N);
  int32_t* suppkey = alloc_shared_array<int32_t>("q4 suppkey", N);
  int32_t* partkey = alloc_shared_array<int32_t>("q4 partkey", N);
  int32_t* revenue = alloc_shared_array<int32_t>("q4 revenue", N);
  int32_t* supplycost = alloc_shared_array<int32_t>("q4 supplycost", N);

  const int32_t orderdate_host[N] = {100, 101, 102, 103, 100, 101, 102, 102};
  const int32_t custkey_host[N] = {1, 2, 1, 2, 1, 1, 2, 1};
  const int32_t suppkey_host[N] = {1, 1, 2, 2, 1, 2, 1, 2};
  const int32_t partkey_host[N] = {1, 1, 2, 2, 3, 1, 2, 2};
  const int32_t revenue_host[N] = {100, 50, 30, 40, 80, 10, 70, 60};
  const int32_t supplycost_host[N] = {40, 70, 10, 5, 10, 30, 20, 50};
  for (size_t i = 0; i < N; ++i) {
    orderdate[i] = orderdate_host[i];
    custkey[i] = custkey_host[i];
    suppkey[i] = suppkey_host[i];
    partkey[i] = partkey_host[i];
    revenue[i] = revenue_host[i];
    supplycost[i] = supplycost_host[i];
  }

  const int32_t date_year_host[4] = {1992, 1992, 1993, 1993};
  const uint8_t date_match_host[4] = {1, 1, 1, 0};
  const int32_t customer_code_host[3] = {-1, 0, 1};
  const uint8_t customer_match_host[3] = {0, 1, 1};
  const int32_t supplier_code_host[3] = {-1, 0, 1};
  const uint8_t supplier_match_host[3] = {0, 1, 1};
  const int32_t part_code_host[4] = {-1, 0, 1, 0};
  const uint8_t part_match_host[4] = {0, 1, 1, 0};

  int32_t* date_year = alloc_device_copy<int32_t>("q4 date year by offset", date_year_host, 4);
  uint8_t* date_match = alloc_device_copy<uint8_t>("q4 date match", date_match_host, 4);
  int32_t* customer_code =
      alloc_device_copy<int32_t>("q4 customer group code", customer_code_host, 3);
  uint8_t* customer_match = alloc_device_copy<uint8_t>("q4 customer match", customer_match_host, 3);
  int32_t* supplier_code =
      alloc_device_copy<int32_t>("q4 supplier group code", supplier_code_host, 3);
  uint8_t* supplier_match = alloc_device_copy<uint8_t>("q4 supplier match", supplier_match_host, 3);
  int32_t* part_code = alloc_device_copy<int32_t>("q4 part group code", part_code_host, 4);
  uint8_t* part_match = alloc_device_copy<uint8_t>("q4 part match", part_match_host, 4);
  uint32_t* scratch_profit_lo = alloc_device_array<uint32_t>("q4 scratch profit lo", 8);
  uint32_t* scratch_profit_hi = alloc_device_array<uint32_t>("q4 scratch profit hi", 8);
  uint32_t* scratch_count = alloc_device_array<uint32_t>("q4 scratch count", 8);

  int64_t profit_by_group[8];
  uint32_t count_by_group[8];
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_ssbm_q4_grouped_profit_i64_usm(
      i32_col(orderdate), i32_col(custkey), i32_col(suppkey), i32_col(partkey), i32_col(revenue),
      i32_col(supplycost), N, 100, date_year, date_match, 4, customer_code, customer_match, 3,
      supplier_code, supplier_match, 3, part_code, part_match, 4, 2, 1992, 2, 2, 2,
      scratch_profit_lo, scratch_profit_hi, scratch_count, 8, profit_by_group, count_by_group, 8,
      &selected, &uncertain);

  ASSERT_STATUS_OK("SSBM Q4 grouped profit status", status);
  ASSERT_TRUE("SSBM Q4 selected count", selected == 6);
  ASSERT_TRUE("SSBM Q4 uncertain count", uncertain == 0);

  const int64_t expected_profit[8] = {40, 0, -20, 0, 0, 50, 0, 30};
  const uint32_t expected_count[8] = {2, 0, 1, 0, 0, 1, 0, 2};
  for (size_t i = 0; i < 8; ++i) {
    ASSERT_TRUE("SSBM Q4 profit group", profit_by_group[i] == expected_profit[i]);
    ASSERT_TRUE("SSBM Q4 count group", count_by_group[i] == expected_count[i]);
  }

  free_device(date_year);
  free_device(date_match);
  free_device(customer_code);
  free_device(customer_match);
  free_device(supplier_code);
  free_device(supplier_match);
  free_device(part_code);
  free_device(part_match);
  free_device(scratch_profit_lo);
  free_device(scratch_profit_hi);
  free_device(scratch_count);
  free_shared(orderdate);
  free_shared(custkey);
  free_shared(suppkey);
  free_shared(partkey);
  free_shared(revenue);
  free_shared(supplycost);
}

void test_resident_dense_grouped_f64() {
  constexpr size_t N = 8;
  const int32_t group_host[N] = {10, 11, 10, 12, 10, 11, 12, 12};
  const double value_host[N] = {1.0, 2.5, 3.0, 4.0, 5.0, 6.5, 8.0, 16.0};
  const uint8_t filter_host[N] = {1, 1, 0, 1, 1, 0, 1, 0};

  int32_t* groups = alloc_device_copy<int32_t>("dense f64 groups", group_host, N);
  double* values = alloc_device_copy<double>("dense f64 values", value_host, N);
  uint8_t* filter = alloc_device_copy<uint8_t>("dense f64 filter", filter_host, N);
  double* scratch_sum = alloc_device_array<double>("dense f64 sum scratch", 3);
  double* scratch_min = alloc_device_array<double>("dense f64 min scratch", 3);
  double* scratch_max = alloc_device_array<double>("dense f64 max scratch", 3);
  uint32_t* scratch_count = alloc_device_array<uint32_t>("dense f64 count scratch", 3);
  uint32_t* scratch_start = alloc_device_array<uint32_t>("dense f64 start scratch", 3);
  uint32_t* scratch_cursor = alloc_device_array<uint32_t>("dense f64 cursor scratch", 3);
  int32_t* scratch_sorted = alloc_device_array<int32_t>("dense f64 sorted scratch", N);
  uint32_t* scratch_index = alloc_device_array<uint32_t>("dense f64 index scratch", N);

  double sum_by_group[3] = {0.0, 0.0, 0.0};
  double min_by_group[3] = {0.0, 0.0, 0.0};
  double max_by_group[3] = {0.0, 0.0, 0.0};
  uint32_t count_by_group[3] = {0, 0, 0};
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v2(
      i32_col(groups), f64_col(values), bool_col(filter), N, 10, 3, scratch_sum, scratch_min,
      scratch_max, scratch_count, scratch_start, scratch_cursor, 3, scratch_sorted, scratch_index,
      N, sum_by_group, min_by_group, max_by_group, count_by_group, 3, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 status", status);
  ASSERT_TRUE("resident dense grouped f64 selected", selected == 5);
  ASSERT_TRUE("resident dense grouped f64 uncertain", uncertain == 0);
  ASSERT_TRUE("resident dense grouped f64 group 10 sum", sum_by_group[0] == 6.0);
  ASSERT_TRUE("resident dense grouped f64 group 10 min", min_by_group[0] == 1.0);
  ASSERT_TRUE("resident dense grouped f64 group 10 max", max_by_group[0] == 5.0);
  ASSERT_TRUE("resident dense grouped f64 group 10 count", count_by_group[0] == 2);
  ASSERT_TRUE("resident dense grouped f64 group 11 sum", sum_by_group[1] == 2.5);
  ASSERT_TRUE("resident dense grouped f64 group 11 min", min_by_group[1] == 2.5);
  ASSERT_TRUE("resident dense grouped f64 group 11 max", max_by_group[1] == 2.5);
  ASSERT_TRUE("resident dense grouped f64 group 11 count", count_by_group[1] == 1);
  ASSERT_TRUE("resident dense grouped f64 group 12 sum", sum_by_group[2] == 12.0);
  ASSERT_TRUE("resident dense grouped f64 group 12 min", min_by_group[2] == 4.0);
  ASSERT_TRUE("resident dense grouped f64 group 12 max", max_by_group[2] == 8.0);
  ASSERT_TRUE("resident dense grouped f64 group 12 count", count_by_group[2] == 2);

  free_device(groups);
  free_device(values);
  free_device(filter);
  free_device(scratch_sum);
  free_device(scratch_min);
  free_device(scratch_max);
  free_device(scratch_count);
  free_device(scratch_start);
  free_device(scratch_cursor);
  free_device(scratch_sorted);
  free_device(scratch_index);
}

void test_resident_dense_grouped_f64_v8_predicate_sources() {
  constexpr size_t N = 6;
  constexpr int32_t GROUP_COUNT = 2;
  constexpr uint32_t AGG_SUM_COUNT = (1u << 0) | (1u << 3);
  constexpr int32_t FILTER_ROWS = 0;
  constexpr int32_t PRED_BETWEEN = 1;
  constexpr int32_t PRED_SOURCE_VALUE = 0;

  const int32_t group_host[N] = {0, 0, 1, 1, 0, 1};
  const double value_host[N] = {1.0, 2.0, 3.0, 4.0, 5.0, 6.0};
  const uint8_t value_null_host[N] = {0, 0, 0, 0, 1, 0};
  const double rhs_host[N] = {10.0, 20.0, 30.0, 40.0, 50.0, 60.0};
  const uint8_t rhs_null_host[N] = {0, 0, 0, 0, 1, 0};
  const uint8_t filter_host[N] = {1, 1, 1, 1, 1, 1};

  int32_t* groups = alloc_device_copy<int32_t>("dense f64 v8 groups", group_host, N);
  double* values = alloc_device_copy<double>("dense f64 v8 values", value_host, N);
  uint8_t* value_nulls = alloc_device_copy<uint8_t>("dense f64 v8 value nulls", value_null_host, N);
  double* rhs = alloc_device_copy<double>("dense f64 v8 rhs", rhs_host, N);
  uint8_t* rhs_nulls = alloc_device_copy<uint8_t>("dense f64 v8 rhs nulls", rhs_null_host, N);
  uint8_t* filter = alloc_device_copy<uint8_t>("dense f64 v8 filter", filter_host, N);
  double* scratch_sum = alloc_device_array<double>("dense f64 v8 sum scratch", GROUP_COUNT);
  double* scratch_min = alloc_device_array<double>("dense f64 v8 min scratch", GROUP_COUNT);
  double* scratch_max = alloc_device_array<double>("dense f64 v8 max scratch", GROUP_COUNT);
  uint32_t* scratch_count = alloc_device_array<uint32_t>("dense f64 v8 count scratch", GROUP_COUNT);
  uint32_t* scratch_start = alloc_device_array<uint32_t>("dense f64 v8 start scratch", GROUP_COUNT);
  uint32_t* scratch_cursor =
      alloc_device_array<uint32_t>("dense f64 v8 cursor scratch", GROUP_COUNT);
  int32_t* scratch_sorted = alloc_device_array<int32_t>("dense f64 v8 sorted scratch", N);
  uint32_t* scratch_index = alloc_device_array<uint32_t>("dense f64 v8 index scratch", N);

  pgaccel_expr_usm_col no_rhs = {};
  no_rhs.values = nullptr;
  no_rhs.nulls = nullptr;
  no_rhs.type = PGACCEL_VAL_NULL;

  double sum_by_group[GROUP_COUNT] = {0.0, 0.0};
  double min_by_group[GROUP_COUNT] = {0.0, 0.0};
  double max_by_group[GROUP_COUNT] = {0.0, 0.0};
  uint32_t count_by_group[GROUP_COUNT] = {0, 0};
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v8(
      i32_col(groups), f64_col(values, value_nulls), no_rhs, 0, AGG_SUM_COUNT, FILTER_ROWS,
      PRED_BETWEEN, PRED_SOURCE_VALUE, 1, 2.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, bool_col(filter),
      N, 0, GROUP_COUNT, scratch_sum, scratch_min, scratch_max, scratch_count, scratch_start,
      scratch_cursor, GROUP_COUNT, scratch_sorted, scratch_index, N, sum_by_group, min_by_group,
      max_by_group, count_by_group, GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 v8 value source status", status);
  ASSERT_TRUE("resident dense grouped f64 v8 value source selected", selected == 3);
  ASSERT_TRUE("resident dense grouped f64 v8 value source uncertain", uncertain == 0);
  ASSERT_TRUE("resident dense grouped f64 v8 value source group 0 sum", sum_by_group[0] == 2.0);
  ASSERT_TRUE("resident dense grouped f64 v8 value source group 0 count", count_by_group[0] == 1);
  ASSERT_TRUE("resident dense grouped f64 v8 value source group 1 sum", sum_by_group[1] == 7.0);
  ASSERT_TRUE("resident dense grouped f64 v8 value source group 1 count", count_by_group[1] == 2);

  sum_by_group[0] = 0.0;
  sum_by_group[1] = 0.0;
  count_by_group[0] = 0;
  count_by_group[1] = 0;
  selected = static_cast<size_t>(-1);
  uncertain = static_cast<size_t>(-1);
  status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v7(
      i32_col(groups), f64_col(values, value_nulls), f64_col(rhs, rhs_nulls), 0, AGG_SUM_COUNT,
      FILTER_ROWS, PRED_BETWEEN, 1, 20.0, 50.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, bool_col(filter), N,
      0, GROUP_COUNT, scratch_sum, scratch_min, scratch_max, scratch_count, scratch_start,
      scratch_cursor, GROUP_COUNT, scratch_sorted, scratch_index, N, sum_by_group, min_by_group,
      max_by_group, count_by_group, GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 v7 rhs source status", status);
  ASSERT_TRUE("resident dense grouped f64 v7 rhs source selected", selected == 3);
  ASSERT_TRUE("resident dense grouped f64 v7 rhs source uncertain", uncertain == 0);
  ASSERT_TRUE("resident dense grouped f64 v7 rhs source group 0 sum", sum_by_group[0] == 2.0);
  ASSERT_TRUE("resident dense grouped f64 v7 rhs source group 0 count", count_by_group[0] == 1);
  ASSERT_TRUE("resident dense grouped f64 v7 rhs source group 1 sum", sum_by_group[1] == 7.0);
  ASSERT_TRUE("resident dense grouped f64 v7 rhs source group 1 count", count_by_group[1] == 2);

  free_device(groups);
  free_device(values);
  free_device(value_nulls);
  free_device(rhs);
  free_device(rhs_nulls);
  free_device(filter);
  free_device(scratch_sum);
  free_device(scratch_min);
  free_device(scratch_max);
  free_device(scratch_count);
  free_device(scratch_start);
  free_device(scratch_cursor);
  free_device(scratch_sorted);
  free_device(scratch_index);
}

void test_resident_dense_grouped_f64_blocked_sum_count() {
  constexpr size_t N = 270000;
  constexpr int32_t GROUP_COUNT = 64;
  constexpr uint32_t AGG_SUM_COUNT = (1u << 0) | (1u << 3);
  constexpr int32_t FILTER_MEASURE_ONLY = 1;
  constexpr int32_t PRED_BETWEEN = 1;
  constexpr int32_t PRED_SOURCE_VALUE = 0;

  std::vector<int32_t> group_host(N);
  std::vector<double> value_host(N);
  std::vector<uint8_t> value_null_host(N);
  std::vector<double> rhs_host(N);
  std::vector<uint8_t> filter_host(N);
  std::vector<double> expected_sum(GROUP_COUNT, 0.0);
  std::vector<uint32_t> expected_count(GROUP_COUNT, 0);

  for (size_t i = 0; i < N; ++i) {
    const int32_t group = static_cast<int32_t>((i * 17) % GROUP_COUNT);
    const double value = static_cast<double>(i % 997) * 0.5 + 1.0;
    const double rhs = static_cast<double>((i % 13) + 1) * 0.125;
    const bool value_is_null = (i % 19) == 0;
    const bool active = (i % 7) != 0;

    group_host[i] = group;
    value_host[i] = value;
    value_null_host[i] = value_is_null ? 1 : 0;
    rhs_host[i] = rhs;
    filter_host[i] = active ? 1 : 0;

    expected_count[group] += 1;
    if (!value_is_null && active && value >= 125.0 && value <= 360.0) {
      expected_sum[group] += value * rhs;
    }
  }

  int32_t* groups = alloc_device_copy<int32_t>("dense f64 blocked groups", group_host.data(), N);
  double* values = alloc_device_copy<double>("dense f64 blocked values", value_host.data(), N);
  uint8_t* value_nulls =
      alloc_device_copy<uint8_t>("dense f64 blocked value nulls", value_null_host.data(), N);
  double* rhs = alloc_device_copy<double>("dense f64 blocked rhs", rhs_host.data(), N);
  uint8_t* filter = alloc_device_copy<uint8_t>("dense f64 blocked filter", filter_host.data(), N);
  double* scratch_sum = alloc_device_array<double>("dense f64 blocked sum scratch", GROUP_COUNT);
  double* scratch_min = alloc_device_array<double>("dense f64 blocked min scratch", GROUP_COUNT);
  double* scratch_max = alloc_device_array<double>("dense f64 blocked max scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 blocked count scratch", GROUP_COUNT);
  uint32_t* scratch_start =
      alloc_device_array<uint32_t>("dense f64 blocked start scratch", GROUP_COUNT);
  uint32_t* scratch_cursor =
      alloc_device_array<uint32_t>("dense f64 blocked cursor scratch", GROUP_COUNT);
  int32_t* scratch_sorted = alloc_device_array<int32_t>("dense f64 blocked sorted scratch", N);
  uint32_t* scratch_index = alloc_device_array<uint32_t>("dense f64 blocked index scratch", N);

  pgaccel_expr_usm_col no_rhs_nulls = f64_col(rhs);
  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<double> min_by_group(GROUP_COUNT, 0.0);
  std::vector<double> max_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v8(
      i32_col(groups), f64_col(values, value_nulls), no_rhs_nulls, 1, AGG_SUM_COUNT,
      FILTER_MEASURE_ONLY, PRED_BETWEEN, PRED_SOURCE_VALUE, 1, 125.0, 360.0, 0.0, 0.0, 0.0, 0.0,
      0.0, 0.0, bool_col(filter), N, 0, GROUP_COUNT, scratch_sum, scratch_min, scratch_max,
      scratch_count, scratch_start, scratch_cursor, GROUP_COUNT, scratch_sorted, scratch_index, N,
      sum_by_group.data(), min_by_group.data(), max_by_group.data(), count_by_group.data(),
      GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 blocked status", status);
  ASSERT_TRUE("resident dense grouped f64 blocked selected", selected == N);
  ASSERT_TRUE("resident dense grouped f64 blocked uncertain", uncertain == 0);

  bool sums_match = true;
  bool counts_match = true;
  for (int32_t group = 0; group < GROUP_COUNT; ++group) {
    counts_match = counts_match && count_by_group[group] == expected_count[group];
    const double scale =
        std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
    const double relative_error = std::fabs(sum_by_group[group] - expected_sum[group]) / scale;
    sums_match = sums_match && relative_error < 1e-7;
  }
  ASSERT_TRUE("resident dense grouped f64 blocked counts", counts_match);
  ASSERT_TRUE("resident dense grouped f64 blocked sums", sums_match);

  free_device(groups);
  free_device(values);
  free_device(value_nulls);
  free_device(rhs);
  free_device(filter);
  free_device(scratch_sum);
  free_device(scratch_min);
  free_device(scratch_max);
  free_device(scratch_count);
  free_device(scratch_start);
  free_device(scratch_cursor);
  free_device(scratch_sorted);
  free_device(scratch_index);
}

void test_resident_dense_grouped_f64_one_pass_expression_sum_count() {
  constexpr size_t N = 70000;
  constexpr int32_t GROUP_COUNT = 256;
  constexpr uint32_t AGG_SUM_COUNT = (1u << 0) | (1u << 3);
  constexpr int32_t FILTER_MEASURE_ONLY = 1;
  constexpr int32_t PRED_RANGES = 2;
  constexpr int32_t PRED_SOURCE_RHS = 1;

  std::vector<int32_t> group_host(N);
  std::vector<double> value_host(N);
  std::vector<uint8_t> value_null_host(N);
  std::vector<double> rhs_host(N);
  std::vector<uint8_t> rhs_null_host(N);
  std::vector<uint8_t> filter_host(N);
  std::vector<double> expected_sum(GROUP_COUNT, 0.0);
  std::vector<uint32_t> expected_count(GROUP_COUNT, 0);

  for (size_t i = 0; i < N; ++i) {
    const int32_t group = static_cast<int32_t>((i * 11) % GROUP_COUNT);
    const double value = static_cast<double>((i * 17) % 1000) / 4.0 + 0.25;
    const double rhs = static_cast<double>((i * 23) % 200) / 10.0;
    const bool value_is_null = (i % 29) == 0;
    const bool rhs_is_null = (i % 31) == 0;
    const bool active = (i % 5) != 0;

    group_host[i] = group;
    value_host[i] = value;
    value_null_host[i] = value_is_null ? 1 : 0;
    rhs_host[i] = rhs;
    rhs_null_host[i] = rhs_is_null ? 1 : 0;
    filter_host[i] = active ? 1 : 0;

    expected_count[group] += 1;
    if (!value_is_null && !rhs_is_null && active &&
        ((rhs >= 3.0 && rhs <= 7.5) || (rhs >= 12.0 && rhs <= 16.0))) {
      expected_sum[group] += value * rhs;
    }
  }

  const size_t partial_items = ((N + 4096 - 1) / 4096) * GROUP_COUNT;
  int32_t* groups =
      alloc_device_copy<int32_t>("dense f64 one-pass expression groups", group_host.data(), N);
  double* values =
      alloc_device_copy<double>("dense f64 one-pass expression values", value_host.data(), N);
  uint8_t* value_nulls = alloc_device_copy<uint8_t>("dense f64 one-pass expression value nulls",
                                                    value_null_host.data(), N);
  double* rhs = alloc_device_copy<double>("dense f64 one-pass expression rhs", rhs_host.data(), N);
  uint8_t* rhs_nulls = alloc_device_copy<uint8_t>("dense f64 one-pass expression rhs nulls",
                                                  rhs_null_host.data(), N);
  uint8_t* filter =
      alloc_device_copy<uint8_t>("dense f64 one-pass expression filter", filter_host.data(), N);
  double* scratch_sum =
      alloc_device_array<double>("dense f64 one-pass expression sum scratch", GROUP_COUNT);
  double* scratch_min =
      alloc_device_array<double>("dense f64 one-pass expression min scratch", GROUP_COUNT);
  double* scratch_max =
      alloc_device_array<double>("dense f64 one-pass expression max scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 one-pass expression count scratch", GROUP_COUNT);
  uint32_t* scratch_start =
      alloc_device_array<uint32_t>("dense f64 one-pass expression start scratch", GROUP_COUNT);
  uint32_t* scratch_cursor =
      alloc_device_array<uint32_t>("dense f64 one-pass expression cursor scratch", GROUP_COUNT);
  int32_t* scratch_sorted =
      alloc_device_array<int32_t>("dense f64 one-pass expression sorted scratch", N);
  uint32_t* scratch_index =
      alloc_device_array<uint32_t>("dense f64 one-pass expression index scratch", N);
  double* scratch_partial_sum = alloc_device_array<double>(
      "dense f64 one-pass expression partial sum scratch", partial_items);
  double* scratch_partial_min = alloc_device_array<double>(
      "dense f64 one-pass expression partial min scratch", partial_items);
  double* scratch_partial_max = alloc_device_array<double>(
      "dense f64 one-pass expression partial max scratch", partial_items);
  uint32_t* scratch_partial_count = alloc_device_array<uint32_t>(
      "dense f64 one-pass expression partial count scratch", partial_items);

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<double> min_by_group(GROUP_COUNT, 0.0);
  std::vector<double> max_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v9(
      i32_col(groups), f64_col(values, value_nulls), f64_col(rhs, rhs_nulls), 1, AGG_SUM_COUNT,
      FILTER_MEASURE_ONLY, PRED_RANGES, PRED_SOURCE_RHS, 2, 3.0, 7.5, 12.0, 16.0, 0.0, 0.0, 0.0,
      0.0, bool_col(filter), N, 0, GROUP_COUNT, scratch_sum, scratch_min, scratch_max,
      scratch_count, scratch_start, scratch_cursor, GROUP_COUNT, scratch_sorted, scratch_index, N,
      scratch_partial_sum, scratch_partial_min, scratch_partial_max, scratch_partial_count,
      partial_items, sum_by_group.data(), min_by_group.data(), max_by_group.data(),
      count_by_group.data(), GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 one-pass expression status", status);
  ASSERT_TRUE("resident dense grouped f64 one-pass expression selected", selected == N);
  ASSERT_TRUE("resident dense grouped f64 one-pass expression uncertain", uncertain == 0);

  bool sums_match = true;
  bool counts_match = true;
  for (int32_t group = 0; group < GROUP_COUNT; ++group) {
    counts_match = counts_match && count_by_group[group] == expected_count[group];
    const double scale =
        std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
    const double relative_error = std::fabs(sum_by_group[group] - expected_sum[group]) / scale;
    sums_match = sums_match && relative_error < 1e-7;
  }
  ASSERT_TRUE("resident dense grouped f64 one-pass expression counts", counts_match);
  ASSERT_TRUE("resident dense grouped f64 one-pass expression sums", sums_match);

  free_device(groups);
  free_device(values);
  free_device(value_nulls);
  free_device(rhs);
  free_device(rhs_nulls);
  free_device(filter);
  free_device(scratch_sum);
  free_device(scratch_min);
  free_device(scratch_max);
  free_device(scratch_count);
  free_device(scratch_start);
  free_device(scratch_cursor);
  free_device(scratch_sorted);
  free_device(scratch_index);
  free_device(scratch_partial_sum);
  free_device(scratch_partial_min);
  free_device(scratch_partial_max);
  free_device(scratch_partial_count);
}

void run_resident_dense_grouped_f64_simple_sum_count(size_t N, int32_t GROUP_COUNT) {
  std::vector<int32_t> group_host(N);
  std::vector<uint8_t> group_null_host(N);
  std::vector<double> value_host(N);
  std::vector<uint8_t> value_null_host(N);
  std::vector<double> expected_sum(GROUP_COUNT, 0.0);
  std::vector<uint32_t> expected_count(GROUP_COUNT, 0);
  size_t expected_selected = 0;

  for (size_t i = 0; i < N; ++i) {
    const int32_t group = static_cast<int32_t>((i * 13) % GROUP_COUNT);
    const double value = static_cast<double>((i * 37) % 10000) / 8.0 + 0.125;
    const bool group_is_null = (i % 257) == 0;
    const bool value_is_null = (i % 19) == 0;

    group_host[i] = group;
    group_null_host[i] = group_is_null ? 1 : 0;
    value_host[i] = value;
    value_null_host[i] = value_is_null ? 1 : 0;

    if (!group_is_null && !value_is_null) {
      expected_sum[group] += value;
      expected_count[group] += 1;
      expected_selected += 1;
    }
  }

  const size_t partial_items = ((N + 8192 - 1) / 8192) * GROUP_COUNT;
  int32_t* groups =
      alloc_device_copy<int32_t>("dense f64 simple sum count groups", group_host.data(), N);
  uint8_t* group_nulls = alloc_device_copy<uint8_t>("dense f64 simple sum count group nulls",
                                                    group_null_host.data(), N);
  double* values =
      alloc_device_copy<double>("dense f64 simple sum count values", value_host.data(), N);
  uint8_t* value_nulls = alloc_device_copy<uint8_t>("dense f64 simple sum count value nulls",
                                                    value_null_host.data(), N);
  double* scratch_sum =
      alloc_device_array<double>("dense f64 simple sum count sum scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 simple sum count count scratch", GROUP_COUNT);
  double* scratch_partial_sum =
      alloc_device_array<double>("dense f64 simple sum count partial sum scratch", partial_items);
  uint32_t* scratch_partial_count = alloc_device_array<uint32_t>(
      "dense f64 simple sum count partial count scratch", partial_items);

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_simple_sum_count_usm(
      i32_col(groups, group_nulls), f64_col(values, value_nulls), N, 0, GROUP_COUNT, scratch_sum,
      scratch_count, scratch_partial_sum, scratch_partial_count, partial_items, sum_by_group.data(),
      count_by_group.data(), GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 simple sum count status", status);
  ASSERT_TRUE("resident dense grouped f64 simple sum count selected",
              selected == expected_selected);
  ASSERT_TRUE("resident dense grouped f64 simple sum count uncertain", uncertain == 0);

  bool sums_match = true;
  bool counts_match = true;
  for (int32_t group = 0; group < GROUP_COUNT; ++group) {
    counts_match = counts_match && count_by_group[group] == expected_count[group];
    const double scale =
        std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
    const double relative_error = std::fabs(sum_by_group[group] - expected_sum[group]) / scale;
    sums_match = sums_match && relative_error < 1e-12;
  }
  ASSERT_TRUE("resident dense grouped f64 simple sum count counts", counts_match);
  ASSERT_TRUE("resident dense grouped f64 simple sum count sums", sums_match);

  free_device(groups);
  free_device(group_nulls);
  free_device(values);
  free_device(value_nulls);
  free_device(scratch_sum);
  free_device(scratch_count);
  free_device(scratch_partial_sum);
  free_device(scratch_partial_count);
}

void test_resident_dense_grouped_f64_simple_sum_count_256() {
  run_resident_dense_grouped_f64_simple_sum_count(70000, 256);
}

void test_resident_dense_grouped_f64_simple_sum_count_1k() {
  run_resident_dense_grouped_f64_simple_sum_count(100000, 1000);
}

void test_resident_dense_grouped_f64_simple_sum_count_med_card() {
  run_resident_dense_grouped_f64_simple_sum_count(10000, 6313);
  run_resident_dense_grouped_f64_simple_sum_count(100000, 10000);
}

void test_resident_dense_grouped_f64_mul_sum_count_256() {
  constexpr size_t N = 70000;
  constexpr int32_t GROUP_COUNT = 256;
  constexpr int32_t FILTER_NONE = 0;
  constexpr int32_t FILTER_AGGREGATE = 1;
  constexpr int32_t FILTER_MEASURE_ONLY = 2;

  std::vector<int32_t> group_host(N);
  std::vector<uint8_t> group_null_host(N);
  std::vector<double> lhs_host(N);
  std::vector<uint8_t> lhs_null_host(N);
  std::vector<double> rhs_host(N);
  std::vector<uint8_t> rhs_null_host(N);
  std::vector<uint8_t> filter_host(N);
  std::vector<uint8_t> filter_null_host(N);
  std::vector<double> expected_sum_none(GROUP_COUNT, 0.0);
  std::vector<double> expected_sum_aggregate(GROUP_COUNT, 0.0);
  std::vector<double> expected_sum_measure_only(GROUP_COUNT, 0.0);
  std::vector<uint32_t> expected_count_none(GROUP_COUNT, 0);
  std::vector<uint32_t> expected_count_aggregate(GROUP_COUNT, 0);
  std::vector<uint32_t> expected_count_measure_only(GROUP_COUNT, 0);

  for (size_t i = 0; i < N; ++i) {
    const int32_t group = static_cast<int32_t>((i * 17) % GROUP_COUNT);
    const double lhs = static_cast<double>((i * 31) % 10000) / 7.0 + 0.5;
    const double rhs = static_cast<double>((i * 43) % 1000) / 1000.0 + 0.01;
    const bool group_is_null = (i % 263) == 0;
    const bool lhs_is_null = (i % 29) == 0;
    const bool rhs_is_null = (i % 31) == 0;
    const bool filter_is_null = (i % 37) == 0;
    const bool active = (i % 5) == 0;

    group_host[i] = group;
    group_null_host[i] = group_is_null ? 1 : 0;
    lhs_host[i] = lhs;
    lhs_null_host[i] = lhs_is_null ? 1 : 0;
    rhs_host[i] = rhs;
    rhs_null_host[i] = rhs_is_null ? 1 : 0;
    filter_host[i] = active ? 1 : 0;
    filter_null_host[i] = filter_is_null ? 1 : 0;

    if (group_is_null) {
      continue;
    }
    const bool measure_valid = !lhs_is_null && !rhs_is_null;
    const bool filter_passes = active && !filter_is_null;
    expected_count_none[group] += 1;
    expected_count_measure_only[group] += 1;
    if (measure_valid) {
      expected_sum_none[group] += lhs * rhs;
    }
    if (filter_passes) {
      expected_count_aggregate[group] += 1;
      if (measure_valid) {
        expected_sum_aggregate[group] += lhs * rhs;
        expected_sum_measure_only[group] += lhs * rhs;
      }
    }
  }

  const size_t partial_items = ((N + 8192 - 1) / 8192) * GROUP_COUNT;
  int32_t* groups =
      alloc_device_copy<int32_t>("dense f64 mul sum count groups", group_host.data(), N);
  uint8_t* group_nulls =
      alloc_device_copy<uint8_t>("dense f64 mul sum count group nulls", group_null_host.data(), N);
  double* lhs = alloc_device_copy<double>("dense f64 mul sum count lhs", lhs_host.data(), N);
  uint8_t* lhs_nulls =
      alloc_device_copy<uint8_t>("dense f64 mul sum count lhs nulls", lhs_null_host.data(), N);
  double* rhs = alloc_device_copy<double>("dense f64 mul sum count rhs", rhs_host.data(), N);
  uint8_t* rhs_nulls =
      alloc_device_copy<uint8_t>("dense f64 mul sum count rhs nulls", rhs_null_host.data(), N);
  uint8_t* filter =
      alloc_device_copy<uint8_t>("dense f64 mul sum count filter", filter_host.data(), N);
  uint8_t* filter_nulls = alloc_device_copy<uint8_t>("dense f64 mul sum count filter nulls",
                                                     filter_null_host.data(), N);
  double* scratch_sum =
      alloc_device_array<double>("dense f64 mul sum count sum scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 mul sum count count scratch", GROUP_COUNT);
  double* scratch_partial_sum =
      alloc_device_array<double>("dense f64 mul sum count partial sum scratch", partial_items);
  uint32_t* scratch_partial_count =
      alloc_device_array<uint32_t>("dense f64 mul sum count partial count scratch", partial_items);

  auto run_case = [&](const char* label, int32_t filter_mode, pgaccel_expr_usm_col filter_col,
                      const std::vector<double>& expected_sum,
                      const std::vector<uint32_t>& expected_count) {
    std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
    std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
    size_t selected = static_cast<size_t>(-1);
    size_t uncertain = static_cast<size_t>(-1);
    pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_mul_sum_count_usm(
        i32_col(groups, group_nulls), f64_col(lhs, lhs_nulls), f64_col(rhs, rhs_nulls), filter_col,
        filter_mode, N, 0, GROUP_COUNT, scratch_sum, scratch_count, scratch_partial_sum,
        scratch_partial_count, partial_items, sum_by_group.data(), count_by_group.data(),
        GROUP_COUNT, &selected, &uncertain);

    ASSERT_STATUS_OK(label, status);
    size_t expected_selected = 0;
    for (uint32_t count : expected_count) {
      expected_selected += count;
    }
    ASSERT_TRUE(label, selected == expected_selected);
    ASSERT_TRUE(label, uncertain == 0);
    bool sums_match = true;
    bool counts_match = true;
    for (int32_t group = 0; group < GROUP_COUNT; ++group) {
      counts_match = counts_match && count_by_group[group] == expected_count[group];
      const double scale =
          std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
      const double relative_error = std::fabs(sum_by_group[group] - expected_sum[group]) / scale;
      sums_match = sums_match && relative_error < 1e-12;
    }
    ASSERT_TRUE(label, counts_match);
    ASSERT_TRUE(label, sums_match);
  };

  run_case("resident dense grouped f64 mul sum count none", FILTER_NONE, bool_col(nullptr),
           expected_sum_none, expected_count_none);
  run_case("resident dense grouped f64 mul sum count aggregate filter", FILTER_AGGREGATE,
           bool_col(filter, filter_nulls), expected_sum_aggregate, expected_count_aggregate);
  run_case("resident dense grouped f64 mul sum count measure filter", FILTER_MEASURE_ONLY,
           bool_col(filter, filter_nulls), expected_sum_measure_only, expected_count_measure_only);

  free_device(groups);
  free_device(group_nulls);
  free_device(lhs);
  free_device(lhs_nulls);
  free_device(rhs);
  free_device(rhs_nulls);
  free_device(filter);
  free_device(filter_nulls);
  free_device(scratch_sum);
  free_device(scratch_count);
  free_device(scratch_partial_sum);
  free_device(scratch_partial_count);
}

void test_resident_dense_grouped_f64_pred_sum_count_ranges_256() {
  constexpr size_t N = 70000;
  constexpr int32_t GROUP_COUNT = 256;
  constexpr int32_t MEASURE_OP_MUL = 1;
  constexpr int32_t FILTER_MEASURE_ONLY = 1;
  constexpr int32_t PREDICATE_SOURCE_RHS = 1;
  constexpr int32_t PREDICATE_RANGES = 2;

  std::vector<int32_t> group_host(N);
  std::vector<uint8_t> group_null_host(N);
  std::vector<double> lhs_host(N);
  std::vector<uint8_t> lhs_null_host(N);
  std::vector<double> rhs_host(N);
  std::vector<uint8_t> rhs_null_host(N);
  std::vector<uint8_t> filter_host(N);
  std::vector<uint8_t> filter_null_host(N);
  std::vector<double> expected_sum(GROUP_COUNT, 0.0);
  std::vector<uint32_t> expected_count(GROUP_COUNT, 0);

  auto rhs_in_ranges = [](double rhs) {
    return (rhs >= 0.0 && rhs <= 0.10) || (rhs >= 0.25 && rhs <= 0.30) ||
           (rhs >= 0.45 && rhs <= 0.55);
  };

  for (size_t i = 0; i < N; ++i) {
    const int32_t group = static_cast<int32_t>((i * 19) % GROUP_COUNT);
    const double lhs = static_cast<double>((i * 31) % 10000) / 11.0 + 0.25;
    const double rhs = static_cast<double>((i * 43) % 1000) / 1000.0;
    const bool group_is_null = (i % 263) == 0;
    const bool lhs_is_null = (i % 29) == 0;
    const bool rhs_is_null = (i % 31) == 0;
    const bool filter_is_null = (i % 37) == 0;
    const bool active = (i % 5) != 1;

    group_host[i] = group;
    group_null_host[i] = group_is_null ? 1 : 0;
    lhs_host[i] = lhs;
    lhs_null_host[i] = lhs_is_null ? 1 : 0;
    rhs_host[i] = rhs;
    rhs_null_host[i] = rhs_is_null ? 1 : 0;
    filter_host[i] = active ? 1 : 0;
    filter_null_host[i] = filter_is_null ? 1 : 0;

    if (group_is_null) {
      continue;
    }
    expected_count[group] += 1;
    if (!lhs_is_null && !rhs_is_null && active && !filter_is_null && rhs_in_ranges(rhs)) {
      expected_sum[group] += lhs * rhs;
    }
  }

  const size_t partial_items = ((N + 8192 - 1) / 8192) * GROUP_COUNT;
  int32_t* groups =
      alloc_device_copy<int32_t>("dense f64 pred sum count groups", group_host.data(), N);
  uint8_t* group_nulls = alloc_device_copy<uint8_t>("dense f64 pred sum count group nulls",
                                                    group_null_host.data(), N);
  double* lhs = alloc_device_copy<double>("dense f64 pred sum count lhs", lhs_host.data(), N);
  uint8_t* lhs_nulls =
      alloc_device_copy<uint8_t>("dense f64 pred sum count lhs nulls", lhs_null_host.data(), N);
  double* rhs = alloc_device_copy<double>("dense f64 pred sum count rhs", rhs_host.data(), N);
  uint8_t* rhs_nulls =
      alloc_device_copy<uint8_t>("dense f64 pred sum count rhs nulls", rhs_null_host.data(), N);
  uint8_t* filter =
      alloc_device_copy<uint8_t>("dense f64 pred sum count filter", filter_host.data(), N);
  uint8_t* filter_nulls = alloc_device_copy<uint8_t>("dense f64 pred sum count filter nulls",
                                                     filter_null_host.data(), N);
  double* scratch_sum =
      alloc_device_array<double>("dense f64 pred sum count sum scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 pred sum count count scratch", GROUP_COUNT);
  double* scratch_partial_sum =
      alloc_device_array<double>("dense f64 pred sum count partial sum scratch", partial_items);
  uint32_t* scratch_partial_count =
      alloc_device_array<uint32_t>("dense f64 pred sum count partial count scratch", partial_items);

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_pred_sum_count_usm(
      i32_col(groups, group_nulls), f64_col(lhs, lhs_nulls), f64_col(rhs, rhs_nulls),
      bool_col(filter, filter_nulls), MEASURE_OP_MUL, FILTER_MEASURE_ONLY, PREDICATE_SOURCE_RHS,
      PREDICATE_RANGES, 3, 0.0, 0.10, 0.25, 0.30, 0.45, 0.55, 0.0, 0.0, N, 0, GROUP_COUNT,
      scratch_sum, scratch_count, scratch_partial_sum, scratch_partial_count, partial_items,
      sum_by_group.data(), count_by_group.data(), GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 predicate sum count status", status);
  size_t expected_selected = 0;
  for (uint32_t count : expected_count) {
    expected_selected += count;
  }
  ASSERT_TRUE("resident dense grouped f64 predicate sum count selected",
              selected == expected_selected);
  ASSERT_TRUE("resident dense grouped f64 predicate sum count uncertain", uncertain == 0);

  bool sums_match = true;
  bool counts_match = true;
  for (int32_t group = 0; group < GROUP_COUNT; ++group) {
    counts_match = counts_match && count_by_group[group] == expected_count[group];
    const double scale =
        std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
    const double relative_error = std::fabs(sum_by_group[group] - expected_sum[group]) / scale;
    sums_match = sums_match && relative_error < 1e-12;
  }
  ASSERT_TRUE("resident dense grouped f64 predicate sum count counts", counts_match);
  ASSERT_TRUE("resident dense grouped f64 predicate sum count sums", sums_match);

  free_device(groups);
  free_device(group_nulls);
  free_device(lhs);
  free_device(lhs_nulls);
  free_device(rhs);
  free_device(rhs_nulls);
  free_device(filter);
  free_device(filter_nulls);
  free_device(scratch_sum);
  free_device(scratch_count);
  free_device(scratch_partial_sum);
  free_device(scratch_partial_count);
}

void test_resident_dense_grouped_f64_stats_pair_1k() {
  constexpr size_t N = 100000;
  constexpr int32_t GROUP_COUNT = 1000;

  std::vector<int32_t> group_host(N);
  std::vector<uint8_t> group_null_host(N);
  std::vector<double> value_host(N);
  std::vector<uint8_t> value_null_host(N);
  std::vector<double> rhs_host(N);
  std::vector<uint8_t> rhs_null_host(N);
  std::vector<double> expected_sum(GROUP_COUNT, 0.0);
  std::vector<double> expected_sumsq(GROUP_COUNT, 0.0);
  std::vector<uint32_t> expected_count(GROUP_COUNT, 0);
  std::vector<double> expected_rhs_sum(GROUP_COUNT, 0.0);
  std::vector<uint32_t> expected_rhs_count(GROUP_COUNT, 0);
  size_t expected_selected = 0;

  for (size_t i = 0; i < N; ++i) {
    const int32_t group = static_cast<int32_t>(i % GROUP_COUNT);
    const double value = static_cast<double>((i * 31) % 10000) / 13.0 - 100.0;
    const double rhs = static_cast<double>((i * 43) % 7000) / 17.0 + 0.25;
    const bool group_is_null = (i % 9973) == 0;
    const bool value_is_null = (i % 101) == 0;
    const bool rhs_is_null = (i % 113) == 0;

    group_host[i] = group;
    group_null_host[i] = group_is_null ? 1 : 0;
    value_host[i] = value;
    value_null_host[i] = value_is_null ? 1 : 0;
    rhs_host[i] = rhs;
    rhs_null_host[i] = rhs_is_null ? 1 : 0;

    if (group_is_null)
      continue;
    if (!value_is_null) {
      expected_sum[group] += value;
      expected_sumsq[group] += value * value;
      expected_count[group] += 1;
      expected_selected += 1;
    }
    if (!rhs_is_null) {
      expected_rhs_sum[group] += rhs;
      expected_rhs_count[group] += 1;
    }
  }

  int32_t* groups = alloc_device_copy<int32_t>("dense f64 stats pair groups", group_host.data(), N);
  uint8_t* group_nulls =
      alloc_device_copy<uint8_t>("dense f64 stats pair group nulls", group_null_host.data(), N);
  double* values = alloc_device_copy<double>("dense f64 stats pair values", value_host.data(), N);
  uint8_t* value_nulls =
      alloc_device_copy<uint8_t>("dense f64 stats pair value nulls", value_null_host.data(), N);
  double* rhs = alloc_device_copy<double>("dense f64 stats pair rhs", rhs_host.data(), N);
  uint8_t* rhs_nulls =
      alloc_device_copy<uint8_t>("dense f64 stats pair rhs nulls", rhs_null_host.data(), N);
  double* scratch_sum = alloc_device_array<double>("dense f64 stats pair sum scratch", GROUP_COUNT);
  double* scratch_sumsq =
      alloc_device_array<double>("dense f64 stats pair sumsq scratch", GROUP_COUNT);
  double* scratch_rhs_sum =
      alloc_device_array<double>("dense f64 stats pair rhs sum scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 stats pair count scratch", GROUP_COUNT);
  uint32_t* scratch_group_start =
      alloc_device_array<uint32_t>("dense f64 stats pair group start scratch", GROUP_COUNT);
  uint32_t* scratch_group_len =
      alloc_device_array<uint32_t>("dense f64 stats pair group len scratch", GROUP_COUNT);
  int32_t* scratch_sorted_group =
      alloc_device_array<int32_t>("dense f64 stats pair sorted group scratch", N);
  uint32_t* scratch_row_index =
      alloc_device_array<uint32_t>("dense f64 stats pair row index scratch", N);

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<double> sumsq_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  std::vector<double> rhs_sum_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> rhs_count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_stats_pair_usm(
      i32_col(groups, group_nulls), f64_col(values, value_nulls), f64_col(rhs, rhs_nulls), N, 0,
      GROUP_COUNT, scratch_sum, scratch_sumsq, scratch_rhs_sum, scratch_count, scratch_group_start,
      scratch_group_len, GROUP_COUNT, scratch_sorted_group, scratch_row_index, N,
      sum_by_group.data(), sumsq_by_group.data(), count_by_group.data(), rhs_sum_by_group.data(),
      rhs_count_by_group.data(), GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 stats pair status", status);
  ASSERT_TRUE("resident dense grouped f64 stats pair selected", selected == expected_selected);
  ASSERT_TRUE("resident dense grouped f64 stats pair uncertain", uncertain == 0);

  bool sums_match = true;
  bool sumsq_match = true;
  bool counts_match = true;
  bool rhs_sums_match = true;
  bool rhs_counts_match = true;
  for (int32_t group = 0; group < GROUP_COUNT; ++group) {
    counts_match = counts_match && count_by_group[group] == expected_count[group];
    rhs_counts_match = rhs_counts_match && rhs_count_by_group[group] == expected_rhs_count[group];
    const double sum_scale =
        std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
    const double sumsq_scale =
        std::fabs(expected_sumsq[group]) > 1.0 ? std::fabs(expected_sumsq[group]) : 1.0;
    const double rhs_scale =
        std::fabs(expected_rhs_sum[group]) > 1.0 ? std::fabs(expected_rhs_sum[group]) : 1.0;
    sums_match =
        sums_match && std::fabs(sum_by_group[group] - expected_sum[group]) / sum_scale < 1e-10;
    sumsq_match = sumsq_match &&
                  std::fabs(sumsq_by_group[group] - expected_sumsq[group]) / sumsq_scale < 1e-10;
    rhs_sums_match =
        rhs_sums_match &&
        std::fabs(rhs_sum_by_group[group] - expected_rhs_sum[group]) / rhs_scale < 1e-10;
  }
  ASSERT_TRUE("resident dense grouped f64 stats pair counts", counts_match);
  ASSERT_TRUE("resident dense grouped f64 stats pair rhs counts", rhs_counts_match);
  ASSERT_TRUE("resident dense grouped f64 stats pair sums", sums_match);
  ASSERT_TRUE("resident dense grouped f64 stats pair sumsq", sumsq_match);
  ASSERT_TRUE("resident dense grouped f64 stats pair rhs sums", rhs_sums_match);

  free_device(groups);
  free_device(group_nulls);
  free_device(values);
  free_device(value_nulls);
  free_device(rhs);
  free_device(rhs_nulls);
  free_device(scratch_sum);
  free_device(scratch_sumsq);
  free_device(scratch_rhs_sum);
  free_device(scratch_count);
  free_device(scratch_group_start);
  free_device(scratch_group_len);
  free_device(scratch_sorted_group);
  free_device(scratch_row_index);
}

void test_resident_star_dim_group_project_nan_semantics() {
  constexpr size_t N = 4;
  const double nan = std::numeric_limits<double>::quiet_NaN();
  int32_t* fact_keys = alloc_shared_array<int32_t>("star project fact keys", N);
  double* values = alloc_shared_array<double>("star project values", N);
  uint8_t* dim_match = alloc_shared_array<uint8_t>("star project dim match", N);
  int32_t* dim_groups = alloc_shared_array<int32_t>("star project dim groups", N);
  int32_t* out_groups = alloc_shared_array<int32_t>("star project output groups", N);

  fact_keys[0] = 0;
  fact_keys[1] = 1;
  fact_keys[2] = 2;
  fact_keys[3] = 3;
  values[0] = nan;
  values[1] = 1.0;
  values[2] = 2.0;
  values[3] = nan;
  for (size_t i = 0; i < N; ++i) {
    dim_match[i] = 1;
    dim_groups[i] = static_cast<int32_t>(10 + i);
    out_groups[i] = -99;
  }

  pgaccel_status status = pgaccel_expr_template_resident_star_dim_group_project_f64_usm(
      i32_col(fact_keys), f64_col(values), N, dim_match, dim_groups, N, PGACCEL_EXPR_OP_EQ, nan,
      out_groups, N);
  ASSERT_STATUS_OK("resident star project NaN equality status", status);
  ASSERT_TRUE("resident star project PG NaN equality row 0", out_groups[0] == 10);
  ASSERT_TRUE("resident star project finite row rejected by NaN equality", out_groups[1] == -1);
  ASSERT_TRUE("resident star project finite row 2 rejected by NaN equality", out_groups[2] == -1);
  ASSERT_TRUE("resident star project PG NaN equality row 3", out_groups[3] == 13);

  free_shared(fact_keys);
  free_shared(values);
  free_shared(dim_match);
  free_shared(dim_groups);
  free_shared(out_groups);
}

void test_resident_star_dim_grouped_f64_sum_count_fused() {
  constexpr size_t N = 9000;
  constexpr size_t DIM_KEY_COUNT = 8;
  constexpr int32_t GROUP_COUNT = 4;
  constexpr size_t PARTIAL_ITEMS = ((N + 8191) / 8192) * GROUP_COUNT;
  constexpr double VALUE_THRESHOLD = 5.0;

  std::vector<int32_t> fact_key_host(N);
  std::vector<uint8_t> fact_key_null_host(N, 0);
  std::vector<double> value_host(N);
  std::vector<uint8_t> value_null_host(N, 0);
  std::vector<uint8_t> dim_match_host(DIM_KEY_COUNT, 0);
  std::vector<int32_t> dim_group_host(DIM_KEY_COUNT, -1);
  std::vector<double> expected_sum(GROUP_COUNT, 0.0);
  std::vector<uint32_t> expected_count(GROUP_COUNT, 0);
  size_t expected_selected = 0;

  dim_match_host[1] = 1;
  dim_group_host[1] = 0;
  dim_match_host[2] = 1;
  dim_group_host[2] = 1;
  dim_match_host[4] = 1;
  dim_group_host[4] = 2;
  dim_match_host[5] = 1;
  dim_group_host[5] = 3;
  dim_match_host[7] = 1;
  dim_group_host[7] = 1;

  for (size_t row = 0; row < N; ++row) {
    fact_key_host[row] = static_cast<int32_t>(row % 10) - 1;
    value_host[row] = static_cast<double>(row % 13) + 0.5;
    if (row % 257 == 0) {
      fact_key_null_host[row] = 1;
    }
    if (row % 263 == 0) {
      value_null_host[row] = 1;
    }

    if (fact_key_null_host[row] != 0 || value_null_host[row] != 0 ||
        !(value_host[row] > VALUE_THRESHOLD) || fact_key_host[row] < 0 ||
        static_cast<size_t>(fact_key_host[row]) >= DIM_KEY_COUNT) {
      continue;
    }
    const size_t key_idx = static_cast<size_t>(fact_key_host[row]);
    if (dim_match_host[key_idx] == 0 || dim_group_host[key_idx] < 0)
      continue;
    const int32_t group = dim_group_host[key_idx];
    expected_sum[group] += value_host[row];
    expected_count[group] += 1;
    expected_selected += 1;
  }

  int32_t* fact_keys =
      alloc_device_copy<int32_t>("fused star groupagg fact keys", fact_key_host.data(), N);
  uint8_t* fact_key_nulls = alloc_device_copy<uint8_t>(
      "fused star groupagg fact key nulls", fact_key_null_host.data(), N);
  double* values =
      alloc_device_copy<double>("fused star groupagg values", value_host.data(), N);
  uint8_t* value_nulls =
      alloc_device_copy<uint8_t>("fused star groupagg value nulls", value_null_host.data(), N);
  uint8_t* dim_match = alloc_device_copy<uint8_t>("fused star groupagg dim match",
                                                  dim_match_host.data(), DIM_KEY_COUNT);
  int32_t* dim_groups = alloc_device_copy<int32_t>("fused star groupagg dim groups",
                                                   dim_group_host.data(), DIM_KEY_COUNT);
  double* scratch_sum =
      alloc_device_array<double>("fused star groupagg sum scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("fused star groupagg count scratch", GROUP_COUNT);
  double* scratch_partial_sum =
      alloc_device_array<double>("fused star groupagg partial sum scratch", PARTIAL_ITEMS);
  uint32_t* scratch_partial_count =
      alloc_device_array<uint32_t>("fused star groupagg partial count scratch", PARTIAL_ITEMS);

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_star_dim_grouped_f64_sum_count_usm(
      i32_col(fact_keys, fact_key_nulls), f64_col(values, value_nulls), N, dim_match, dim_groups,
      DIM_KEY_COUNT, PGACCEL_EXPR_OP_GT, VALUE_THRESHOLD, GROUP_COUNT, scratch_sum, scratch_count,
      scratch_partial_sum, scratch_partial_count, PARTIAL_ITEMS, sum_by_group.data(),
      count_by_group.data(), GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident fused star grouped f64 sum count status", status);
  ASSERT_TRUE("resident fused star grouped f64 sum count selected",
              selected == expected_selected);
  ASSERT_TRUE("resident fused star grouped f64 sum count uncertain", uncertain == 0);

  bool counts_match = true;
  bool sums_match = true;
  for (int32_t group = 0; group < GROUP_COUNT; ++group) {
    counts_match = counts_match && count_by_group[group] == expected_count[group];
    const double scale =
        std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
    sums_match = sums_match && std::fabs(sum_by_group[group] - expected_sum[group]) / scale < 1e-12;
  }
  ASSERT_TRUE("resident fused star grouped f64 sum count counts", counts_match);
  ASSERT_TRUE("resident fused star grouped f64 sum count sums", sums_match);

  free_device(fact_keys);
  free_device(fact_key_nulls);
  free_device(values);
  free_device(value_nulls);
  free_device(dim_match);
  free_device(dim_groups);
  free_device(scratch_sum);
  free_device(scratch_count);
  free_device(scratch_partial_sum);
  free_device(scratch_partial_count);
}

void test_resident_dense_grouped_f64_blocked_min_max_avg() {
  constexpr size_t N = 270000;
  constexpr int32_t GROUP_COUNT = 64;
  constexpr uint32_t AGG_ALL = (1u << 0) | (1u << 1) | (1u << 2) | (1u << 3);
  constexpr int32_t FILTER_ROWS = 0;
  constexpr int32_t PRED_BETWEEN = 1;
  constexpr int32_t PRED_SOURCE_VALUE = 0;

  std::vector<int32_t> group_host(N);
  std::vector<double> value_host(N);
  std::vector<uint8_t> value_null_host(N);
  std::vector<uint8_t> filter_host(N);
  std::vector<double> expected_sum(GROUP_COUNT, 0.0);
  std::vector<double> expected_min(GROUP_COUNT, std::numeric_limits<double>::max());
  std::vector<double> expected_max(GROUP_COUNT, std::numeric_limits<double>::lowest());
  std::vector<uint32_t> expected_count(GROUP_COUNT, 0);

  size_t expected_selected = 0;
  for (size_t i = 0; i < N; ++i) {
    const int32_t group = static_cast<int32_t>((i * 29) % GROUP_COUNT);
    const double value = static_cast<double>((i * 37) % 10000) / 10.0;
    const bool value_is_null = (i % 23) == 0;
    const bool active = (i % 5) != 0;

    group_host[i] = group;
    value_host[i] = value;
    value_null_host[i] = value_is_null ? 1 : 0;
    filter_host[i] = active ? 1 : 0;

    if (!value_is_null && active && value >= 100.0 && value <= 700.0) {
      expected_sum[group] += value;
      expected_min[group] = std::min(expected_min[group], value);
      expected_max[group] = std::max(expected_max[group], value);
      expected_count[group] += 1;
      expected_selected += 1;
    }
  }

  int32_t* groups =
      alloc_device_copy<int32_t>("dense f64 blocked minmax groups", group_host.data(), N);
  double* values =
      alloc_device_copy<double>("dense f64 blocked minmax values", value_host.data(), N);
  uint8_t* value_nulls =
      alloc_device_copy<uint8_t>("dense f64 blocked minmax value nulls", value_null_host.data(), N);
  uint8_t* filter =
      alloc_device_copy<uint8_t>("dense f64 blocked minmax filter", filter_host.data(), N);
  double* scratch_sum =
      alloc_device_array<double>("dense f64 blocked minmax sum scratch", GROUP_COUNT);
  double* scratch_min =
      alloc_device_array<double>("dense f64 blocked minmax min scratch", GROUP_COUNT);
  double* scratch_max =
      alloc_device_array<double>("dense f64 blocked minmax max scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 blocked minmax count scratch", GROUP_COUNT);
  uint32_t* scratch_start =
      alloc_device_array<uint32_t>("dense f64 blocked minmax start scratch", GROUP_COUNT);
  uint32_t* scratch_cursor =
      alloc_device_array<uint32_t>("dense f64 blocked minmax cursor scratch", GROUP_COUNT);
  int32_t* scratch_sorted =
      alloc_device_array<int32_t>("dense f64 blocked minmax sorted scratch", N);
  uint32_t* scratch_index =
      alloc_device_array<uint32_t>("dense f64 blocked minmax index scratch", N);

  pgaccel_expr_usm_col no_rhs = {};
  no_rhs.values = nullptr;
  no_rhs.nulls = nullptr;
  no_rhs.type = PGACCEL_VAL_NULL;

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<double> min_by_group(GROUP_COUNT, 0.0);
  std::vector<double> max_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v8(
      i32_col(groups), f64_col(values, value_nulls), no_rhs, 0, AGG_ALL, FILTER_ROWS, PRED_BETWEEN,
      PRED_SOURCE_VALUE, 1, 100.0, 700.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, bool_col(filter), N, 0,
      GROUP_COUNT, scratch_sum, scratch_min, scratch_max, scratch_count, scratch_start,
      scratch_cursor, GROUP_COUNT, scratch_sorted, scratch_index, N, sum_by_group.data(),
      min_by_group.data(), max_by_group.data(), count_by_group.data(), GROUP_COUNT, &selected,
      &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 blocked minmax status", status);
  ASSERT_TRUE("resident dense grouped f64 blocked minmax selected", selected == expected_selected);
  ASSERT_TRUE("resident dense grouped f64 blocked minmax uncertain", uncertain == 0);

  bool sums_match = true;
  bool counts_match = true;
  bool mins_match = true;
  bool maxes_match = true;
  for (int32_t group = 0; group < GROUP_COUNT; ++group) {
    counts_match = counts_match && count_by_group[group] == expected_count[group];
    const double scale =
        std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
    const double relative_error = std::fabs(sum_by_group[group] - expected_sum[group]) / scale;
    sums_match = sums_match && relative_error < 1e-7;
    if (expected_count[group] != 0) {
      mins_match = mins_match && std::fabs(min_by_group[group] - expected_min[group]) < 1e-12;
      maxes_match = maxes_match && std::fabs(max_by_group[group] - expected_max[group]) < 1e-12;
    }
  }
  ASSERT_TRUE("resident dense grouped f64 blocked minmax counts", counts_match);
  ASSERT_TRUE("resident dense grouped f64 blocked minmax sums", sums_match);
  ASSERT_TRUE("resident dense grouped f64 blocked minmax mins", mins_match);
  ASSERT_TRUE("resident dense grouped f64 blocked minmax maxes", maxes_match);

  free_device(groups);
  free_device(values);
  free_device(value_nulls);
  free_device(filter);
  free_device(scratch_sum);
  free_device(scratch_min);
  free_device(scratch_max);
  free_device(scratch_count);
  free_device(scratch_start);
  free_device(scratch_cursor);
  free_device(scratch_sorted);
  free_device(scratch_index);
}

void test_resident_dense_grouped_f64_blocked_simple_min_max_avg() {
  constexpr size_t N = 270000;
  constexpr int32_t GROUP_MIN = -7;
  constexpr int32_t GROUP_COUNT = 101;
  constexpr uint32_t AGG_ALL = (1u << 0) | (1u << 1) | (1u << 2) | (1u << 3);
  constexpr int32_t FILTER_ROWS = 0;
  constexpr int32_t PRED_BOOL_ONLY = 0;
  constexpr int32_t PRED_SOURCE_VALUE = 0;

  std::vector<int32_t> group_host(N);
  std::vector<double> value_host(N);
  std::vector<uint8_t> value_null_host(N);
  std::vector<double> expected_sum(GROUP_COUNT, 0.0);
  std::vector<double> expected_min(GROUP_COUNT, std::numeric_limits<double>::max());
  std::vector<double> expected_max(GROUP_COUNT, std::numeric_limits<double>::lowest());
  std::vector<uint32_t> expected_count(GROUP_COUNT, 0);

  size_t expected_selected = 0;
  for (size_t i = 0; i < N; ++i) {
    const int32_t group_offset = static_cast<int32_t>((i * 37) % GROUP_COUNT);
    const int32_t group = GROUP_MIN + group_offset;
    const double value = static_cast<double>((i * 97) % 10000) / 13.0;
    const bool value_is_null = (i % 31) == 0;

    group_host[i] = group;
    value_host[i] = value;
    value_null_host[i] = value_is_null ? 1 : 0;

    if (!value_is_null) {
      expected_sum[group_offset] += value;
      expected_min[group_offset] = std::min(expected_min[group_offset], value);
      expected_max[group_offset] = std::max(expected_max[group_offset], value);
      expected_count[group_offset] += 1;
      expected_selected += 1;
    }
  }

  int32_t* groups =
      alloc_device_copy<int32_t>("dense f64 blocked simple minmax groups", group_host.data(), N);
  double* values =
      alloc_device_copy<double>("dense f64 blocked simple minmax values", value_host.data(), N);
  uint8_t* value_nulls = alloc_device_copy<uint8_t>("dense f64 blocked simple minmax value nulls",
                                                    value_null_host.data(), N);
  double* scratch_sum =
      alloc_device_array<double>("dense f64 blocked simple minmax sum scratch", GROUP_COUNT);
  double* scratch_min =
      alloc_device_array<double>("dense f64 blocked simple minmax min scratch", GROUP_COUNT);
  double* scratch_max =
      alloc_device_array<double>("dense f64 blocked simple minmax max scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 blocked simple minmax count scratch", GROUP_COUNT);
  uint32_t* scratch_start =
      alloc_device_array<uint32_t>("dense f64 blocked simple minmax start scratch", GROUP_COUNT);
  uint32_t* scratch_cursor =
      alloc_device_array<uint32_t>("dense f64 blocked simple minmax cursor scratch", GROUP_COUNT);
  int32_t* scratch_sorted =
      alloc_device_array<int32_t>("dense f64 blocked simple minmax sorted scratch", N);
  uint32_t* scratch_index =
      alloc_device_array<uint32_t>("dense f64 blocked simple minmax index scratch", N);

  pgaccel_expr_usm_col no_rhs = {};
  no_rhs.values = nullptr;
  no_rhs.nulls = nullptr;
  no_rhs.type = PGACCEL_VAL_NULL;
  pgaccel_expr_usm_col no_filter = {};
  no_filter.values = nullptr;
  no_filter.nulls = nullptr;
  no_filter.type = PGACCEL_VAL_NULL;

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<double> min_by_group(GROUP_COUNT, 0.0);
  std::vector<double> max_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v8(
      i32_col(groups), f64_col(values, value_nulls), no_rhs, 0, AGG_ALL, FILTER_ROWS,
      PRED_BOOL_ONLY, PRED_SOURCE_VALUE, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, no_filter, N,
      GROUP_MIN, GROUP_COUNT, scratch_sum, scratch_min, scratch_max, scratch_count, scratch_start,
      scratch_cursor, GROUP_COUNT, scratch_sorted, scratch_index, N, sum_by_group.data(),
      min_by_group.data(), max_by_group.data(), count_by_group.data(), GROUP_COUNT, &selected,
      &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 blocked simple minmax status", status);
  ASSERT_TRUE("resident dense grouped f64 blocked simple minmax selected",
              selected == expected_selected);
  ASSERT_TRUE("resident dense grouped f64 blocked simple minmax uncertain", uncertain == 0);

  bool sums_match = true;
  bool counts_match = true;
  bool mins_match = true;
  bool maxes_match = true;
  for (int32_t group = 0; group < GROUP_COUNT; ++group) {
    counts_match = counts_match && count_by_group[group] == expected_count[group];
    const double scale =
        std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
    const double relative_error = std::fabs(sum_by_group[group] - expected_sum[group]) / scale;
    sums_match = sums_match && relative_error < 1e-7;
    if (expected_count[group] != 0) {
      mins_match = mins_match && std::fabs(min_by_group[group] - expected_min[group]) < 1e-12;
      maxes_match = maxes_match && std::fabs(max_by_group[group] - expected_max[group]) < 1e-12;
    }
  }
  ASSERT_TRUE("resident dense grouped f64 blocked simple minmax counts", counts_match);
  ASSERT_TRUE("resident dense grouped f64 blocked simple minmax sums", sums_match);
  ASSERT_TRUE("resident dense grouped f64 blocked simple minmax mins", mins_match);
  ASSERT_TRUE("resident dense grouped f64 blocked simple minmax maxes", maxes_match);

  free_device(groups);
  free_device(values);
  free_device(value_nulls);
  free_device(scratch_sum);
  free_device(scratch_min);
  free_device(scratch_max);
  free_device(scratch_count);
  free_device(scratch_start);
  free_device(scratch_cursor);
  free_device(scratch_sorted);
  free_device(scratch_index);
}

void test_resident_dense_grouped_f64_sort_segment() {
  constexpr size_t N = 8;
  constexpr int32_t GROUP_COUNT = 5000;
  const int32_t group_host[N] = {10, 4999, 10, 12, 10, 4999, 12, 12};
  const double value_host[N] = {1.0, 2.5, 3.0, 4.0, 5.0, 6.5, 8.0, 16.0};
  const uint8_t filter_host[N] = {1, 1, 0, 1, 1, 0, 1, 0};

  int32_t* groups = alloc_device_copy<int32_t>("dense f64 sort groups", group_host, N);
  double* values = alloc_device_copy<double>("dense f64 sort values", value_host, N);
  uint8_t* filter = alloc_device_copy<uint8_t>("dense f64 sort filter", filter_host, N);
  double* scratch_sum = alloc_device_array<double>("dense f64 sort sum scratch", GROUP_COUNT);
  double* scratch_min = alloc_device_array<double>("dense f64 sort min scratch", GROUP_COUNT);
  double* scratch_max = alloc_device_array<double>("dense f64 sort max scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 sort count scratch", GROUP_COUNT);
  uint32_t* scratch_start =
      alloc_device_array<uint32_t>("dense f64 sort start scratch", GROUP_COUNT);
  uint32_t* scratch_cursor =
      alloc_device_array<uint32_t>("dense f64 sort cursor scratch", GROUP_COUNT);
  int32_t* scratch_sorted = alloc_device_array<int32_t>("dense f64 sort sorted scratch", N);
  uint32_t* scratch_index = alloc_device_array<uint32_t>("dense f64 sort index scratch", N);

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<double> min_by_group(GROUP_COUNT, 0.0);
  std::vector<double> max_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v2(
      i32_col(groups), f64_col(values), bool_col(filter), N, 0, GROUP_COUNT, scratch_sum,
      scratch_min, scratch_max, scratch_count, scratch_start, scratch_cursor, GROUP_COUNT,
      scratch_sorted, scratch_index, N, sum_by_group.data(), min_by_group.data(),
      max_by_group.data(), count_by_group.data(), GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 sort status", status);
  ASSERT_TRUE("resident dense grouped f64 sort selected", selected == 5);
  ASSERT_TRUE("resident dense grouped f64 sort uncertain", uncertain == 0);
  ASSERT_TRUE("resident dense grouped f64 sort group 10 sum", sum_by_group[10] == 6.0);
  ASSERT_TRUE("resident dense grouped f64 sort group 10 min", min_by_group[10] == 1.0);
  ASSERT_TRUE("resident dense grouped f64 sort group 10 max", max_by_group[10] == 5.0);
  ASSERT_TRUE("resident dense grouped f64 sort group 10 count", count_by_group[10] == 2);
  ASSERT_TRUE("resident dense grouped f64 sort group 12 sum", sum_by_group[12] == 12.0);
  ASSERT_TRUE("resident dense grouped f64 sort group 12 min", min_by_group[12] == 4.0);
  ASSERT_TRUE("resident dense grouped f64 sort group 12 max", max_by_group[12] == 8.0);
  ASSERT_TRUE("resident dense grouped f64 sort group 12 count", count_by_group[12] == 2);
  ASSERT_TRUE("resident dense grouped f64 sort group 4999 sum", sum_by_group[4999] == 2.5);
  ASSERT_TRUE("resident dense grouped f64 sort group 4999 min", min_by_group[4999] == 2.5);
  ASSERT_TRUE("resident dense grouped f64 sort group 4999 max", max_by_group[4999] == 2.5);
  ASSERT_TRUE("resident dense grouped f64 sort group 4999 count", count_by_group[4999] == 1);

  free_device(groups);
  free_device(values);
  free_device(filter);
  free_device(scratch_sum);
  free_device(scratch_min);
  free_device(scratch_max);
  free_device(scratch_count);
  free_device(scratch_start);
  free_device(scratch_cursor);
  free_device(scratch_sorted);
  free_device(scratch_index);
}

void test_resident_dense_grouped_f64_true_sort_segment() {
  constexpr size_t N = 65536;
  constexpr int32_t GROUP_COUNT = 5000;
  std::vector<int32_t> group_host(N);
  std::vector<double> value_host(N);
  std::vector<uint8_t> filter_host(N);
  for (size_t i = 0; i < N; ++i) {
    switch (i % 4) {
      case 0:
        group_host[i] = 10;
        value_host[i] = 1.0;
        filter_host[i] = 1;
        break;
      case 1:
        group_host[i] = 12;
        value_host[i] = 2.0;
        filter_host[i] = 1;
        break;
      case 2:
        group_host[i] = 4999;
        value_host[i] = 3.0;
        filter_host[i] = 1;
        break;
      default:
        group_host[i] = 10;
        value_host[i] = 100.0;
        filter_host[i] = 0;
        break;
    }
  }

  int32_t* groups = alloc_device_copy<int32_t>("dense f64 true sort groups", group_host.data(), N);
  double* values = alloc_device_copy<double>("dense f64 true sort values", value_host.data(), N);
  uint8_t* filter = alloc_device_copy<uint8_t>("dense f64 true sort filter", filter_host.data(), N);
  double* scratch_sum = alloc_device_array<double>("dense f64 true sort sum scratch", GROUP_COUNT);
  double* scratch_min = alloc_device_array<double>("dense f64 true sort min scratch", GROUP_COUNT);
  double* scratch_max = alloc_device_array<double>("dense f64 true sort max scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 true sort count scratch", GROUP_COUNT);
  uint32_t* scratch_start =
      alloc_device_array<uint32_t>("dense f64 true sort start scratch", GROUP_COUNT);
  uint32_t* scratch_cursor =
      alloc_device_array<uint32_t>("dense f64 true sort cursor scratch", GROUP_COUNT);
  int32_t* scratch_sorted = alloc_device_array<int32_t>("dense f64 true sort sorted scratch", N);
  uint32_t* scratch_index = alloc_device_array<uint32_t>("dense f64 true sort index scratch", N);

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<double> min_by_group(GROUP_COUNT, 0.0);
  std::vector<double> max_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v2(
      i32_col(groups), f64_col(values), bool_col(filter), N, 0, GROUP_COUNT, scratch_sum,
      scratch_min, scratch_max, scratch_count, scratch_start, scratch_cursor, GROUP_COUNT,
      scratch_sorted, scratch_index, N, sum_by_group.data(), min_by_group.data(),
      max_by_group.data(), count_by_group.data(), GROUP_COUNT, &selected, &uncertain);

  constexpr uint32_t EXPECTED_PER_GROUP = static_cast<uint32_t>(N / 4);
  ASSERT_STATUS_OK("resident dense grouped f64 true sort status", status);
  ASSERT_TRUE("resident dense grouped f64 true sort selected", selected == N * 3 / 4);
  ASSERT_TRUE("resident dense grouped f64 true sort uncertain", uncertain == 0);
  ASSERT_TRUE("resident dense grouped f64 true sort group 10 sum",
              sum_by_group[10] == static_cast<double>(EXPECTED_PER_GROUP));
  ASSERT_TRUE("resident dense grouped f64 true sort group 10 min", min_by_group[10] == 1.0);
  ASSERT_TRUE("resident dense grouped f64 true sort group 10 max", max_by_group[10] == 1.0);
  ASSERT_TRUE("resident dense grouped f64 true sort group 10 count",
              count_by_group[10] == EXPECTED_PER_GROUP);
  ASSERT_TRUE("resident dense grouped f64 true sort group 12 sum",
              sum_by_group[12] == static_cast<double>(EXPECTED_PER_GROUP * 2));
  ASSERT_TRUE("resident dense grouped f64 true sort group 12 min", min_by_group[12] == 2.0);
  ASSERT_TRUE("resident dense grouped f64 true sort group 12 max", max_by_group[12] == 2.0);
  ASSERT_TRUE("resident dense grouped f64 true sort group 12 count",
              count_by_group[12] == EXPECTED_PER_GROUP);
  ASSERT_TRUE("resident dense grouped f64 true sort group 4999 sum",
              sum_by_group[4999] == static_cast<double>(EXPECTED_PER_GROUP * 3));
  ASSERT_TRUE("resident dense grouped f64 true sort group 4999 min", min_by_group[4999] == 3.0);
  ASSERT_TRUE("resident dense grouped f64 true sort group 4999 max", max_by_group[4999] == 3.0);
  ASSERT_TRUE("resident dense grouped f64 true sort group 4999 count",
              count_by_group[4999] == EXPECTED_PER_GROUP);

  free_device(groups);
  free_device(values);
  free_device(filter);
  free_device(scratch_sum);
  free_device(scratch_min);
  free_device(scratch_max);
  free_device(scratch_count);
  free_device(scratch_start);
  free_device(scratch_cursor);
  free_device(scratch_sorted);
  free_device(scratch_index);
}

void test_resident_dense_grouped_f64_sort_segment_sum_count_sparse() {
  constexpr size_t N = 10000;
  constexpr int32_t GROUP_COUNT = 6313;
  constexpr uint32_t AGG_SUM_COUNT = (1u << 0) | (1u << 3);
  constexpr int32_t FILTER_ROWS = 0;
  constexpr int32_t PRED_BOOL_ONLY = 0;
  constexpr int32_t PRED_SOURCE_VALUE = 0;

  std::vector<int32_t> group_host(N);
  std::vector<double> value_host(N);
  std::vector<double> expected_sum(GROUP_COUNT, 0.0);
  std::vector<uint32_t> expected_count(GROUP_COUNT, 0);
  for (size_t i = 0; i < N; ++i) {
    const int32_t group = static_cast<int32_t>((i * 37 + i / 7) % GROUP_COUNT);
    const double value = static_cast<double>((i * 53) % 10000) / 7.0 + 0.5;
    group_host[i] = group;
    value_host[i] = value;
    expected_sum[group] += value;
    expected_count[group] += 1;
  }

  int32_t* groups =
      alloc_device_copy<int32_t>("dense f64 sparse sort sum/count groups", group_host.data(), N);
  double* values =
      alloc_device_copy<double>("dense f64 sparse sort sum/count values", value_host.data(), N);
  double* scratch_sum =
      alloc_device_array<double>("dense f64 sparse sort sum/count sum scratch", GROUP_COUNT);
  double* scratch_min =
      alloc_device_array<double>("dense f64 sparse sort sum/count min scratch", GROUP_COUNT);
  double* scratch_max =
      alloc_device_array<double>("dense f64 sparse sort sum/count max scratch", GROUP_COUNT);
  uint32_t* scratch_count =
      alloc_device_array<uint32_t>("dense f64 sparse sort sum/count count scratch", GROUP_COUNT);
  uint32_t* scratch_start =
      alloc_device_array<uint32_t>("dense f64 sparse sort sum/count start scratch", GROUP_COUNT);
  uint32_t* scratch_cursor =
      alloc_device_array<uint32_t>("dense f64 sparse sort sum/count cursor scratch", GROUP_COUNT);
  int32_t* scratch_sorted =
      alloc_device_array<int32_t>("dense f64 sparse sort sum/count sorted scratch", N);
  uint32_t* scratch_index =
      alloc_device_array<uint32_t>("dense f64 sparse sort sum/count index scratch", N);

  pgaccel_expr_usm_col no_rhs = {};
  no_rhs.values = nullptr;
  no_rhs.nulls = nullptr;
  no_rhs.type = PGACCEL_VAL_NULL;
  pgaccel_expr_usm_col no_filter = no_rhs;

  std::vector<double> sum_by_group(GROUP_COUNT, 0.0);
  std::vector<double> min_by_group(GROUP_COUNT, 0.0);
  std::vector<double> max_by_group(GROUP_COUNT, 0.0);
  std::vector<uint32_t> count_by_group(GROUP_COUNT, 0);
  size_t selected = static_cast<size_t>(-1);
  size_t uncertain = static_cast<size_t>(-1);
  pgaccel_status status = pgaccel_expr_template_resident_dense_grouped_f64_usm_v9(
      i32_col(groups), f64_col(values), no_rhs, 0, AGG_SUM_COUNT, FILTER_ROWS, PRED_BOOL_ONLY,
      PRED_SOURCE_VALUE, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, no_filter, N, 0,
      GROUP_COUNT, scratch_sum, scratch_min, scratch_max, scratch_count, scratch_start,
      scratch_cursor, GROUP_COUNT, scratch_sorted, scratch_index, N, nullptr, nullptr, nullptr,
      nullptr, 0, sum_by_group.data(), min_by_group.data(), max_by_group.data(),
      count_by_group.data(), GROUP_COUNT, &selected, &uncertain);

  ASSERT_STATUS_OK("resident dense grouped f64 sparse sort sum/count status", status);
  ASSERT_TRUE("resident dense grouped f64 sparse sort sum/count selected", selected == N);
  ASSERT_TRUE("resident dense grouped f64 sparse sort sum/count uncertain", uncertain == 0);

  bool sums_match = true;
  bool counts_match = true;
  for (int32_t group = 0; group < GROUP_COUNT; ++group) {
    counts_match = counts_match && count_by_group[group] == expected_count[group];
    const double scale =
        std::fabs(expected_sum[group]) > 1.0 ? std::fabs(expected_sum[group]) : 1.0;
    const double relative_error = std::fabs(sum_by_group[group] - expected_sum[group]) / scale;
    sums_match = sums_match && relative_error < 1e-12;
  }
  ASSERT_TRUE("resident dense grouped f64 sparse sort sum/count counts", counts_match);
  ASSERT_TRUE("resident dense grouped f64 sparse sort sum/count sums", sums_match);

  free_device(groups);
  free_device(values);
  free_device(scratch_sum);
  free_device(scratch_min);
  free_device(scratch_max);
  free_device(scratch_count);
  free_device(scratch_start);
  free_device(scratch_cursor);
  free_device(scratch_sorted);
  free_device(scratch_index);
}

}  // namespace

int main() {
  pgaccel_status status = pgaccel_init();
  ASSERT_STATUS_OK("pgaccel_init", status);
  if (status != PGACCEL_OK)
    return 1;

  test_range_filters();
  test_date_membership_and_nulls();
  test_device_resident_membership();
  test_resident_sort_kv_i32_device_radix();
  test_q2_grouped_revenue();
  test_q3_grouped_revenue();
  test_q4_grouped_profit();
  test_resident_dense_grouped_f64();
  test_resident_dense_grouped_f64_v8_predicate_sources();
  test_resident_dense_grouped_f64_blocked_sum_count();
  test_resident_dense_grouped_f64_simple_sum_count_256();
  test_resident_dense_grouped_f64_simple_sum_count_1k();
  test_resident_dense_grouped_f64_simple_sum_count_med_card();
  test_resident_dense_grouped_f64_mul_sum_count_256();
  test_resident_dense_grouped_f64_pred_sum_count_ranges_256();
  test_resident_dense_grouped_f64_stats_pair_1k();
  test_resident_star_dim_group_project_nan_semantics();
  test_resident_star_dim_grouped_f64_sum_count_fused();
  test_resident_dense_grouped_f64_one_pass_expression_sum_count();
  test_resident_dense_grouped_f64_blocked_min_max_avg();
  test_resident_dense_grouped_f64_blocked_simple_min_max_avg();
  test_resident_dense_grouped_f64_sort_segment();
  test_resident_dense_grouped_f64_true_sort_segment();
  test_resident_dense_grouped_f64_sort_segment_sum_count_sparse();

  std::printf("test_olap_ssbm: %d passed, %d failed\n", g_pass, g_fail);
  return g_fail == 0 ? 0 : 1;
}
