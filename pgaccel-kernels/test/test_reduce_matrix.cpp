#include <sys/wait.h>
#include <unistd.h>

#include <algorithm>
#include <array>
#include <cerrno>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <numeric>
#include <type_traits>
#include <utility>
#include <vector>

#include "pgaccel_ffi.h"

namespace {

int failures = 0;

void check(bool condition, const char* label) {
  if (!condition) {
    std::fprintf(stderr, "FAIL %s\n", label);
    ++failures;
  }
}

bool ok(pgaccel_status status, const char* label) {
  if (status != PGACCEL_OK) {
    std::fprintf(stderr, "FAIL %s: status=%d\n", label, static_cast<int>(status));
    ++failures;
    return false;
  }
  return true;
}

template <typename T>
bool equal_value(T actual, T expected) {
  if constexpr (std::is_floating_point_v<T>) {
    const T scale = std::max<T>(T{1}, std::fabs(expected));
    return std::fabs(actual - expected) <= scale * T{1.0e-5};
  }
  return actual == expected;
}

template <typename T>
void check_value(T actual, T expected, const char* label) {
  check(equal_value(actual, expected), label);
}

template <typename T>
struct ReduceFns {
  pgaccel_status (*sum)(const T*, size_t, T*);
  pgaccel_status (*min)(const T*, size_t, T*);
  pgaccel_status (*max)(const T*, size_t, T*);
  pgaccel_status (*multi)(const T*, size_t, T*, T*, T*, int64_t*);
  pgaccel_status (*masked)(const T*, const uint8_t*, const uint8_t*, size_t, T*, T*, T*, int64_t*);
};

template <typename T>
void test_numeric_reductions(const char* name, const ReduceFns<T>& fns) {
  constexpr size_t count = 4096;
  std::vector<T> data(count);
  for (size_t i = 0; i < count; ++i)
    data[i] = static_cast<T>(static_cast<int>(i % 9) - 4);

  const T expected_sum = std::accumulate(data.begin(), data.end(), T{0});
  const T expected_min = *std::min_element(data.begin(), data.end());
  const T expected_max = *std::max_element(data.begin(), data.end());

  T sum = T{};
  T min = T{};
  T max = T{};
  char label[96];
  std::snprintf(label, sizeof(label), "%s sum status", name);
  if (ok(fns.sum(data.data(), count, &sum), label)) {
    std::snprintf(label, sizeof(label), "%s sum value", name);
    check_value(sum, expected_sum, label);
  }
  std::snprintf(label, sizeof(label), "%s min status", name);
  if (ok(fns.min(data.data(), count, &min), label)) {
    std::snprintf(label, sizeof(label), "%s min value", name);
    check_value(min, expected_min, label);
  }
  std::snprintf(label, sizeof(label), "%s max status", name);
  if (ok(fns.max(data.data(), count, &max), label)) {
    std::snprintf(label, sizeof(label), "%s max value", name);
    check_value(max, expected_max, label);
  }

  int64_t reduced_count = -1;
  sum = min = max = T{};
  std::snprintf(label, sizeof(label), "%s multi status", name);
  if (ok(fns.multi(data.data(), count, &sum, &min, &max, &reduced_count), label)) {
    std::snprintf(label, sizeof(label), "%s multi sum", name);
    check_value(sum, expected_sum, label);
    std::snprintf(label, sizeof(label), "%s multi min", name);
    check_value(min, expected_min, label);
    std::snprintf(label, sizeof(label), "%s multi max", name);
    check_value(max, expected_max, label);
    std::snprintf(label, sizeof(label), "%s multi count", name);
    check(reduced_count == static_cast<int64_t>(count), label);
  }

  std::vector<uint8_t> nulls(count, 0);
  std::vector<uint8_t> selected(count, 0);
  T masked_sum = T{0};
  T masked_min = std::numeric_limits<T>::max();
  T masked_max = std::numeric_limits<T>::lowest();
  int64_t masked_count = 0;
  for (size_t i = 0; i < count; ++i) {
    nulls[i] = static_cast<uint8_t>((i % 7) == 0);
    selected[i] = static_cast<uint8_t>((i % 3) != 0);
    if (nulls[i] == 0 && selected[i] != 0) {
      masked_sum += data[i];
      masked_min = std::min(masked_min, data[i]);
      masked_max = std::max(masked_max, data[i]);
      ++masked_count;
    }
  }

  sum = T{123};
  min = T{456};
  max = T{789};
  reduced_count = -123;
  std::snprintf(label, sizeof(label), "%s masked status", name);
  const uint64_t dispatches_before = pgaccel_gpu_exec_count();
  const pgaccel_status masked_status = fns.masked(data.data(), nulls.data(), selected.data(), count,
                                                  &sum, &min, &max, &reduced_count);
  if constexpr (std::is_same_v<T, double>) {
    const pgaccel_platform_caps caps = pgaccel_get_caps();
    if (std::strcmp(caps.backend_name, "metal") == 0) {
      check(masked_status == PGACCEL_UNSUPPORTED, "f64 masked Metal quarantine status");
      check(sum == T{123} && min == T{456} && max == T{789} && reduced_count == -123,
            "f64 masked Metal quarantine preserves outputs");
      check(pgaccel_gpu_exec_count() == dispatches_before,
            "f64 masked Metal quarantine does not dispatch");
      return;
    }
  }
  if (ok(masked_status, label)) {
    std::snprintf(label, sizeof(label), "%s masked sum", name);
    check_value(sum, masked_sum, label);
    std::snprintf(label, sizeof(label), "%s masked min", name);
    check_value(min, masked_min, label);
    std::snprintf(label, sizeof(label), "%s masked max", name);
    check_value(max, masked_max, label);
    std::snprintf(label, sizeof(label), "%s masked count", name);
    check(reduced_count == masked_count, label);
  }
}

template <typename T>
void test_numeric_argument_contracts(const char* name, const ReduceFns<T>& fns) {
  const T input = T{7};
  T value = T{99};
  char label[96];
  const std::array<std::pair<const char*, pgaccel_status (*)(const T*, size_t, T*)>, 3> simple = {
      std::pair{"sum", fns.sum}, std::pair{"min", fns.min}, std::pair{"max", fns.max}};
  for (const auto& [operation, function] : simple) {
    std::snprintf(label, sizeof(label), "%s %s rejects null output", name, operation);
    check(function(&input, 1, nullptr) == PGACCEL_ERROR, label);
    value = T{99};
    std::snprintf(label, sizeof(label), "%s %s empty identity", name, operation);
    check(function(nullptr, 0, &value) == PGACCEL_OK && value == T{0}, label);
    std::snprintf(label, sizeof(label), "%s %s rejects null input", name, operation);
    check(function(nullptr, 1, &value) == PGACCEL_ERROR, label);
  }

  T sum = T{99};
  T min = T{99};
  T max = T{99};
  int64_t count = 99;
  std::snprintf(label, sizeof(label), "%s multi rejects null output", name);
  check(fns.multi(&input, 1, nullptr, &min, &max, &count) == PGACCEL_ERROR, label);
  std::snprintf(label, sizeof(label), "%s multi empty identity", name);
  check(fns.multi(nullptr, 0, &sum, &min, &max, &count) == PGACCEL_OK && sum == T{0} &&
            min == T{0} && max == T{0} && count == 0,
        label);
  std::snprintf(label, sizeof(label), "%s multi rejects null input", name);
  check(fns.multi(nullptr, 1, &sum, &min, &max, &count) == PGACCEL_ERROR, label);

  std::snprintf(label, sizeof(label), "%s masked rejects null output", name);
  check(fns.masked(&input, nullptr, nullptr, 1, &sum, nullptr, &max, &count) == PGACCEL_ERROR,
        label);
  std::snprintf(label, sizeof(label), "%s masked empty identity", name);
  check(fns.masked(nullptr, nullptr, nullptr, 0, &sum, &min, &max, &count) == PGACCEL_OK &&
            sum == T{0} && min == T{0} && max == T{0} && count == 0,
        label);
  std::snprintf(label, sizeof(label), "%s masked rejects null input", name);
  check(fns.masked(nullptr, nullptr, nullptr, 1, &sum, &min, &max, &count) == PGACCEL_ERROR, label);
}

void test_mask_combinations_and_empty_selection() {
  constexpr size_t count = 513;
  std::vector<float> values(count);
  std::vector<uint8_t> nulls(count, 0);
  std::vector<uint8_t> selection(count, 1);
  for (size_t i = 0; i < count; ++i) {
    values[i] = static_cast<float>(static_cast<int>(i % 11) - 5);
    nulls[i] = static_cast<uint8_t>((i % 5) == 0);
    selection[i] = static_cast<uint8_t>((i % 7) != 0);
  }

  auto run_case = [&](const char* label, const uint8_t* null_mask, const uint8_t* select_mask) {
    float expected_sum = 0.0f;
    float expected_min = std::numeric_limits<float>::max();
    float expected_max = std::numeric_limits<float>::lowest();
    int64_t expected_count = 0;
    for (size_t i = 0; i < count; ++i) {
      if ((null_mask == nullptr || null_mask[i] == 0) &&
          (select_mask == nullptr || select_mask[i] != 0)) {
        expected_sum += values[i];
        expected_min = std::min(expected_min, values[i]);
        expected_max = std::max(expected_max, values[i]);
        ++expected_count;
      }
    }

    float sum = 99.0f;
    float min = 99.0f;
    float max = 99.0f;
    int64_t reduced_count = -1;
    if (ok(pgaccel_reduce_multi_masked_f32(values.data(), null_mask, select_mask, count, &sum, &min,
                                           &max, &reduced_count),
           label)) {
      check_value(sum, expected_sum, "masked optional-mask sum");
      check_value(min, expected_min, "masked optional-mask min");
      check_value(max, expected_max, "masked optional-mask max");
      check(reduced_count == expected_count, "masked optional-mask count");
    }
  };

  run_case("masked no optional masks", nullptr, nullptr);
  run_case("masked null mask only", nulls.data(), nullptr);
  run_case("masked selection only", nullptr, selection.data());

  std::fill(selection.begin(), selection.end(), uint8_t{0});
  float sum = 99.0f;
  float min = 99.0f;
  float max = 99.0f;
  int64_t reduced_count = -1;
  if (ok(pgaccel_reduce_multi_masked_f32(values.data(), nullptr, selection.data(), count, &sum,
                                         &min, &max, &reduced_count),
         "masked all-filtered status")) {
    check(sum == 0.0f && min == 0.0f && max == 0.0f && reduced_count == 0,
          "masked all-filtered identity");
  }
}

void test_postgres_float_ordering() {
  const double nan = std::numeric_limits<double>::quiet_NaN();
  const std::vector<double> f64 = {-std::numeric_limits<double>::infinity(), -0.0, 0.0, 17.0,
                                   std::numeric_limits<double>::infinity(),  nan};
  double min = 0.0;
  double max = 0.0;
  if (ok(pgaccel_reduce_min_f64(f64.data(), f64.size(), &min), "f64 IEEE min status"))
    check(std::isinf(min) && min < 0.0, "f64 IEEE min value");
  if (ok(pgaccel_reduce_max_f64(f64.data(), f64.size(), &max), "f64 IEEE max status"))
    check(std::isnan(max), "f64 PostgreSQL NaN sorts last");

  const std::vector<float> f32 = {-std::numeric_limits<float>::infinity(),
                                  -0.0f,
                                  0.0f,
                                  17.0f,
                                  std::numeric_limits<float>::infinity(),
                                  std::numeric_limits<float>::quiet_NaN()};
  float sum32 = 0.0f;
  float min32 = 0.0f;
  float max32 = 0.0f;
  int64_t count32 = 0;
  if (ok(pgaccel_reduce_multi_f32(f32.data(), f32.size(), &sum32, &min32, &max32, &count32),
         "f32 multi IEEE status")) {
    check(std::isnan(sum32), "f32 multi IEEE sum");
    check(std::isinf(min32) && min32 < 0.0f, "f32 multi IEEE min");
    check(std::isnan(max32), "f32 multi PostgreSQL NaN max");
    check(count32 == static_cast<int64_t>(f32.size()), "f32 multi IEEE count");
  }

  const std::array<float, 2> f32_nan_first = {std::numeric_limits<float>::quiet_NaN(), 17.0f};
  if (ok(pgaccel_reduce_multi_f32(f32_nan_first.data(), f32_nan_first.size(), &sum32, &min32,
                                  &max32, &count32),
         "f32 multi leading NaN status")) {
    check(std::isnan(sum32), "f32 multi leading NaN sum");
    check(min32 == 17.0f, "f32 multi leading NaN min");
    check(std::isnan(max32), "f32 multi leading NaN max");
    check(count32 == static_cast<int64_t>(f32_nan_first.size()), "f32 multi leading NaN count");
  }

  double sum64 = 0.0;
  int64_t count64 = 0;
  if (ok(pgaccel_reduce_multi_f64(f64.data(), f64.size(), &sum64, &min, &max, &count64),
         "f64 multi IEEE status")) {
    check(std::isnan(sum64), "f64 multi IEEE sum");
    check(std::isinf(min) && min < 0.0, "f64 multi IEEE min");
    check(std::isnan(max), "f64 multi PostgreSQL NaN max");
    check(count64 == static_cast<int64_t>(f64.size()), "f64 multi IEEE count");
  }
}

void test_reduce_count_and_stats() {
  constexpr size_t count = 4096;
  std::vector<uint8_t> mask(count);
  for (size_t i = 0; i < count; ++i)
    mask[i] = static_cast<uint8_t>((i % 5) != 0);
  size_t result = 0;
  if (ok(pgaccel_reduce_count(mask.data(), count, &result), "reduce count status"))
    check(result == std::count(mask.begin(), mask.end(), uint8_t{1}), "reduce count value");

  std::vector<float> f32(count);
  std::vector<double> f64(count);
  double expected_sum = 0.0;
  double expected_sq = 0.0;
  for (size_t i = 0; i < count; ++i) {
    const double value = static_cast<double>(static_cast<int>(i % 5) - 2);
    f32[i] = static_cast<float>(value);
    f64[i] = value;
    expected_sum += value;
    expected_sq += value * value;
  }

  double sum_sq = 0.0;
  if (ok(pgaccel_reduce_sum_sq_f32(f32.data(), count, &sum_sq), "sum_sq f32 status"))
    check_value(sum_sq, expected_sq, "sum_sq f32 value");
  if (ok(pgaccel_reduce_sum_sq_f64(f64.data(), count, &sum_sq), "sum_sq f64 status"))
    check_value(sum_sq, expected_sq, "sum_sq f64 value");

  uint64_t stats_count = 0;
  double stats_sum = 0.0;
  double stats_sq = 0.0;
  if (ok(pgaccel_reduce_stats_f32(f32.data(), count, &stats_count, &stats_sum, &stats_sq),
         "stats f32 status")) {
    check(stats_count == count, "stats f32 count");
    check_value(stats_sum, expected_sum, "stats f32 sum");
    check_value(stats_sq, expected_sq, "stats f32 sum_sq");
  }
  if (ok(pgaccel_reduce_stats_f64(f64.data(), count, &stats_count, &stats_sum, &stats_sq),
         "stats f64 status")) {
    check(stats_count == count, "stats f64 count");
    check_value(stats_sum, expected_sum, "stats f64 sum");
    check_value(stats_sq, expected_sq, "stats f64 sum_sq");
  }
}

void test_count_and_stats_argument_contracts() {
  const uint8_t mask = 1;
  size_t count_result = 99;
  check(pgaccel_reduce_count(&mask, 1, nullptr) == PGACCEL_ERROR,
        "reduce count rejects null output");
  check(pgaccel_reduce_count(nullptr, 0, &count_result) == PGACCEL_OK && count_result == 0,
        "reduce count empty identity");
  check(pgaccel_reduce_count(nullptr, 1, &count_result) == PGACCEL_ERROR,
        "reduce count rejects null input");

  const float f32 = 2.0f;
  const double f64 = 2.0;
  double value = 99.0;
  check(pgaccel_reduce_sum_sq_f32(&f32, 1, nullptr) == PGACCEL_ERROR,
        "sum_sq f32 rejects null output");
  check(pgaccel_reduce_sum_sq_f32(nullptr, 0, &value) == PGACCEL_OK && value == 0.0,
        "sum_sq f32 empty identity");
  check(pgaccel_reduce_sum_sq_f32(nullptr, 1, &value) == PGACCEL_ERROR,
        "sum_sq f32 rejects null input");
  check(pgaccel_reduce_sum_sq_f64(&f64, 1, nullptr) == PGACCEL_ERROR,
        "sum_sq f64 rejects null output");
  check(pgaccel_reduce_sum_sq_f64(nullptr, 0, &value) == PGACCEL_OK && value == 0.0,
        "sum_sq f64 empty identity");
  check(pgaccel_reduce_sum_sq_f64(nullptr, 1, &value) == PGACCEL_ERROR,
        "sum_sq f64 rejects null input");

  uint64_t stats_count = 99;
  double sum = 99.0;
  double sum_sq = 99.0;
  check(pgaccel_reduce_stats_f32(&f32, 1, nullptr, &sum, &sum_sq) == PGACCEL_ERROR,
        "stats f32 rejects null output");
  check(pgaccel_reduce_stats_f32(nullptr, 0, &stats_count, &sum, &sum_sq) == PGACCEL_OK &&
            stats_count == 0 && sum == 0.0 && sum_sq == 0.0,
        "stats f32 empty identity");
  check(pgaccel_reduce_stats_f32(nullptr, 1, &stats_count, &sum, &sum_sq) == PGACCEL_ERROR,
        "stats f32 rejects null input");
  check(pgaccel_reduce_stats_f64(&f64, 1, &stats_count, nullptr, &sum_sq) == PGACCEL_ERROR,
        "stats f64 rejects null output");
  check(pgaccel_reduce_stats_f64(nullptr, 0, &stats_count, &sum, &sum_sq) == PGACCEL_OK &&
            stats_count == 0 && sum == 0.0 && sum_sq == 0.0,
        "stats f64 empty identity");
  check(pgaccel_reduce_stats_f64(nullptr, 1, &stats_count, &sum, &sum_sq) == PGACCEL_ERROR,
        "stats f64 rejects null input");
}

template <typename T>
void test_bit_width(const char* name, pgaccel_status (*and_fn)(const T*, size_t, T*),
                    pgaccel_status (*or_fn)(const T*, size_t, T*),
                    pgaccel_status (*xor_fn)(const T*, size_t, T*)) {
  const std::vector<T> data = {static_cast<T>(0x55), static_cast<T>(0x3c), static_cast<T>(0x0f),
                               static_cast<T>(0x66)};
  T expected_and = data[0];
  T expected_or = data[0];
  T expected_xor = data[0];
  for (size_t i = 1; i < data.size(); ++i) {
    expected_and = static_cast<T>(expected_and & data[i]);
    expected_or = static_cast<T>(expected_or | data[i]);
    expected_xor = static_cast<T>(expected_xor ^ data[i]);
  }
  T result = T{};
  char label[96];
  std::snprintf(label, sizeof(label), "%s bit_and status", name);
  if (ok(and_fn(data.data(), data.size(), &result), label)) {
    std::snprintf(label, sizeof(label), "%s bit_and value", name);
    check(result == expected_and, label);
  }
  std::snprintf(label, sizeof(label), "%s bit_or status", name);
  if (ok(or_fn(data.data(), data.size(), &result), label)) {
    std::snprintf(label, sizeof(label), "%s bit_or value", name);
    check(result == expected_or, label);
  }
  std::snprintf(label, sizeof(label), "%s bit_xor status", name);
  if (ok(xor_fn(data.data(), data.size(), &result), label)) {
    std::snprintf(label, sizeof(label), "%s bit_xor value", name);
    check(result == expected_xor, label);
  }
}

void test_bool_and_bit_reductions() {
  const uint8_t bools[] = {1, 1, 0, 1};
  uint8_t result = 0xff;
  if (ok(pgaccel_reduce_bool_and(bools, 4, &result), "bool_and status"))
    check(result == 0, "bool_and value");
  if (ok(pgaccel_reduce_bool_or(bools, 4, &result), "bool_or status"))
    check(result == 1, "bool_or value");

  test_bit_width<int16_t>("i16", pgaccel_reduce_bit_and_i16, pgaccel_reduce_bit_or_i16,
                          pgaccel_reduce_bit_xor_i16);
  test_bit_width<int32_t>("i32", pgaccel_reduce_bit_and_i32, pgaccel_reduce_bit_or_i32,
                          pgaccel_reduce_bit_xor_i32);
  test_bit_width<int64_t>("i64", pgaccel_reduce_bit_and_i64, pgaccel_reduce_bit_or_i64,
                          pgaccel_reduce_bit_xor_i64);
}

template <typename T>
void test_bit_argument_contracts(const char* name, pgaccel_status (*and_fn)(const T*, size_t, T*),
                                 pgaccel_status (*or_fn)(const T*, size_t, T*),
                                 pgaccel_status (*xor_fn)(const T*, size_t, T*)) {
  const T input = T{1};
  T result = T{99};
  char label[96];

  std::snprintf(label, sizeof(label), "%s bit_and rejects null output", name);
  check(and_fn(&input, 1, nullptr) == PGACCEL_ERROR, label);
  std::snprintf(label, sizeof(label), "%s bit_and empty identity", name);
  check(and_fn(nullptr, 0, &result) == PGACCEL_OK && result == static_cast<T>(~T{0}), label);
  std::snprintf(label, sizeof(label), "%s bit_and rejects null input", name);
  check(and_fn(nullptr, 1, &result) == PGACCEL_ERROR, label);

  std::snprintf(label, sizeof(label), "%s bit_or rejects null output", name);
  check(or_fn(&input, 1, nullptr) == PGACCEL_ERROR, label);
  std::snprintf(label, sizeof(label), "%s bit_or empty identity", name);
  check(or_fn(nullptr, 0, &result) == PGACCEL_OK && result == T{0}, label);
  std::snprintf(label, sizeof(label), "%s bit_or rejects null input", name);
  check(or_fn(nullptr, 1, &result) == PGACCEL_ERROR, label);

  std::snprintf(label, sizeof(label), "%s bit_xor rejects null output", name);
  check(xor_fn(&input, 1, nullptr) == PGACCEL_ERROR, label);
  std::snprintf(label, sizeof(label), "%s bit_xor empty identity", name);
  check(xor_fn(nullptr, 0, &result) == PGACCEL_OK && result == T{0}, label);
  std::snprintf(label, sizeof(label), "%s bit_xor rejects null input", name);
  check(xor_fn(nullptr, 1, &result) == PGACCEL_ERROR, label);
}

void test_bool_and_bit_argument_contracts() {
  const uint8_t boolean = 1;
  uint8_t bool_result = 99;
  check(pgaccel_reduce_bool_and(&boolean, 1, nullptr) == PGACCEL_ERROR,
        "bool_and rejects null output");
  check(pgaccel_reduce_bool_and(nullptr, 0, &bool_result) == PGACCEL_OK && bool_result == 1,
        "bool_and empty identity");
  check(pgaccel_reduce_bool_and(nullptr, 1, &bool_result) == PGACCEL_ERROR,
        "bool_and rejects null input");
  check(pgaccel_reduce_bool_or(&boolean, 1, nullptr) == PGACCEL_ERROR,
        "bool_or rejects null output");
  check(pgaccel_reduce_bool_or(nullptr, 0, &bool_result) == PGACCEL_OK && bool_result == 0,
        "bool_or empty identity");
  check(pgaccel_reduce_bool_or(nullptr, 1, &bool_result) == PGACCEL_ERROR,
        "bool_or rejects null input");

  test_bit_argument_contracts<int16_t>("i16", pgaccel_reduce_bit_and_i16, pgaccel_reduce_bit_or_i16,
                                       pgaccel_reduce_bit_xor_i16);
  test_bit_argument_contracts<int32_t>("i32", pgaccel_reduce_bit_and_i32, pgaccel_reduce_bit_or_i32,
                                       pgaccel_reduce_bit_xor_i32);
  test_bit_argument_contracts<int64_t>("i64", pgaccel_reduce_bit_and_i64, pgaccel_reduce_bit_or_i64,
                                       pgaccel_reduce_bit_xor_i64);
}

bool run_no_device_child(const char* executable) {
  const pid_t child = fork();
  if (child < 0) {
    std::fprintf(stderr, "FAIL fork no-device reduce matrix: errno=%d\n", errno);
    return false;
  }
  if (child == 0) {
    const char* visibility_mask = std::getenv("PGACCEL_TEST_NO_DEVICE_MASK");
    setenv("ACPP_VISIBILITY_MASK", visibility_mask != nullptr ? visibility_mask : "cuda", 1);
    setenv("PGACCEL_TEST_NO_DEVICE", "1", 1);
    execl(executable, executable, static_cast<char*>(nullptr));
    std::fprintf(stderr, "FAIL exec no-device reduce matrix: errno=%d\n", errno);
    _exit(127);
  }

  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
    std::fprintf(stderr, "FAIL no-device reduce matrix child: status=%d errno=%d\n", status, errno);
    return false;
  }
  return true;
}

template <typename T>
void test_bit_no_device(const char* name, pgaccel_status (*and_fn)(const T*, size_t, T*),
                        pgaccel_status (*or_fn)(const T*, size_t, T*),
                        pgaccel_status (*xor_fn)(const T*, size_t, T*)) {
  const T bit = 1;
  T bit_result = 0;
  char label[96];
  std::snprintf(label, sizeof(label), "%s bit_and reports no device", name);
  check(and_fn(&bit, 1, &bit_result) == PGACCEL_ERROR_NO_DEVICE, label);
  std::snprintf(label, sizeof(label), "%s bit_or reports no device", name);
  check(or_fn(&bit, 1, &bit_result) == PGACCEL_ERROR_NO_DEVICE, label);
  std::snprintf(label, sizeof(label), "%s bit_xor reports no device", name);
  check(xor_fn(&bit, 1, &bit_result) == PGACCEL_ERROR_NO_DEVICE, label);
}

template <typename T>
void test_numeric_no_device(const char* name, const ReduceFns<T>& fns) {
  const T input = T{2};
  T sum = T{};
  T min = T{};
  T max = T{};
  int64_t count = 0;
  char label[96];

  const std::array<std::pair<const char*, pgaccel_status (*)(const T*, size_t, T*)>, 3> simple = {
      std::pair{"sum", fns.sum}, std::pair{"min", fns.min}, std::pair{"max", fns.max}};
  for (const auto& [operation, function] : simple) {
    std::snprintf(label, sizeof(label), "%s %s reports no device", name, operation);
    check(function(&input, 1, &sum) == PGACCEL_ERROR_NO_DEVICE, label);
  }

  std::snprintf(label, sizeof(label), "%s multi reports no device", name);
  check(fns.multi(&input, 1, &sum, &min, &max, &count) == PGACCEL_ERROR_NO_DEVICE, label);
  std::snprintf(label, sizeof(label), "%s masked reports no device", name);
  check(fns.masked(&input, nullptr, nullptr, 1, &sum, &min, &max, &count) ==
            PGACCEL_ERROR_NO_DEVICE,
        label);
}

void test_no_device_paths() {
  const pgaccel_status init_status = pgaccel_init();
  check(init_status != PGACCEL_OK, "CPU-only visibility has no GPU device");
  if (init_status == PGACCEL_OK) {
    pgaccel_shutdown();
    return;
  }

  test_numeric_no_device<float>("f32", {pgaccel_reduce_sum_f32, pgaccel_reduce_min_f32,
                                        pgaccel_reduce_max_f32, pgaccel_reduce_multi_f32,
                                        pgaccel_reduce_multi_masked_f32});
  test_numeric_no_device<double>("f64", {pgaccel_reduce_sum_f64, pgaccel_reduce_min_f64,
                                         pgaccel_reduce_max_f64, pgaccel_reduce_multi_f64,
                                         pgaccel_reduce_multi_masked_f64});
  test_numeric_no_device<int64_t>("i64", {pgaccel_reduce_sum_i64, pgaccel_reduce_min_i64,
                                          pgaccel_reduce_max_i64, pgaccel_reduce_multi_i64,
                                          pgaccel_reduce_multi_masked_i64});

  const uint8_t mask = 1;
  size_t count = 0;
  check(pgaccel_reduce_count(&mask, 1, &count) == PGACCEL_ERROR_NO_DEVICE,
        "count reports no device");

  const float f32 = 2.0f;
  const double f64 = 2.0;
  double result = 0.0;
  check(pgaccel_reduce_sum_sq_f32(&f32, 1, &result) == PGACCEL_ERROR_NO_DEVICE,
        "f32 sum_sq reports no device");
  check(pgaccel_reduce_sum_sq_f64(&f64, 1, &result) == PGACCEL_ERROR_NO_DEVICE,
        "f64 sum_sq reports no device");
  uint64_t stats_count = 0;
  double sum = 0.0;
  double sum_sq = 0.0;
  check(pgaccel_reduce_stats_f32(&f32, 1, &stats_count, &sum, &sum_sq) == PGACCEL_ERROR_NO_DEVICE,
        "f32 stats reports no device");
  check(pgaccel_reduce_stats_f64(&f64, 1, &stats_count, &sum, &sum_sq) == PGACCEL_ERROR_NO_DEVICE,
        "f64 stats reports no device");

  uint8_t bool_result = 0;
  check(pgaccel_reduce_bool_and(&mask, 1, &bool_result) == PGACCEL_ERROR_NO_DEVICE,
        "bool_and reports no device");
  check(pgaccel_reduce_bool_or(&mask, 1, &bool_result) == PGACCEL_ERROR_NO_DEVICE,
        "bool_or reports no device");

  test_bit_no_device<int16_t>("i16", pgaccel_reduce_bit_and_i16, pgaccel_reduce_bit_or_i16,
                              pgaccel_reduce_bit_xor_i16);
  test_bit_no_device<int32_t>("i32", pgaccel_reduce_bit_and_i32, pgaccel_reduce_bit_or_i32,
                              pgaccel_reduce_bit_xor_i32);
  test_bit_no_device<int64_t>("i64", pgaccel_reduce_bit_and_i64, pgaccel_reduce_bit_or_i64,
                              pgaccel_reduce_bit_xor_i64);

  check(pgaccel_shutdown() == PGACCEL_OK, "failed initialization shuts down cleanly");
}

}  // namespace

int main(int argc, char** argv) {
  if (std::getenv("PGACCEL_TEST_NO_DEVICE") != nullptr) {
    test_no_device_paths();
    std::printf("reduce no-device matrix: %d failure(s)\n", failures);
    return failures == 0 ? 0 : 1;
  }

  check(argc > 0 && argv[0] != nullptr && run_no_device_child(argv[0]), "no-device reduce child");
  if (!ok(pgaccel_init(), "pgaccel_init"))
    return 1;
  pgaccel_reset_gpu_exec_count();

  test_numeric_reductions<float>("f32", {pgaccel_reduce_sum_f32, pgaccel_reduce_min_f32,
                                         pgaccel_reduce_max_f32, pgaccel_reduce_multi_f32,
                                         pgaccel_reduce_multi_masked_f32});
  test_numeric_reductions<double>("f64", {pgaccel_reduce_sum_f64, pgaccel_reduce_min_f64,
                                          pgaccel_reduce_max_f64, pgaccel_reduce_multi_f64,
                                          pgaccel_reduce_multi_masked_f64});
  test_numeric_reductions<int64_t>("i64", {pgaccel_reduce_sum_i64, pgaccel_reduce_min_i64,
                                           pgaccel_reduce_max_i64, pgaccel_reduce_multi_i64,
                                           pgaccel_reduce_multi_masked_i64});
  test_numeric_argument_contracts<float>("f32", {pgaccel_reduce_sum_f32, pgaccel_reduce_min_f32,
                                                 pgaccel_reduce_max_f32, pgaccel_reduce_multi_f32,
                                                 pgaccel_reduce_multi_masked_f32});
  test_numeric_argument_contracts<double>("f64", {pgaccel_reduce_sum_f64, pgaccel_reduce_min_f64,
                                                  pgaccel_reduce_max_f64, pgaccel_reduce_multi_f64,
                                                  pgaccel_reduce_multi_masked_f64});
  test_numeric_argument_contracts<int64_t>("i64", {pgaccel_reduce_sum_i64, pgaccel_reduce_min_i64,
                                                   pgaccel_reduce_max_i64, pgaccel_reduce_multi_i64,
                                                   pgaccel_reduce_multi_masked_i64});
  test_mask_combinations_and_empty_selection();
  test_postgres_float_ordering();
  test_reduce_count_and_stats();
  test_count_and_stats_argument_contracts();
  test_bool_and_bit_reductions();
  test_bool_and_bit_argument_contracts();
  check(pgaccel_gpu_exec_count() > 0, "positive GPU dispatch count");
  ok(pgaccel_shutdown(), "pgaccel_shutdown");
  std::printf("reduce matrix: %d failure(s)\n", failures);
  return failures == 0 ? 0 : 1;
}
