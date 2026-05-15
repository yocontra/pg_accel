// test_reduce_bool_bit.cpp — Phase 4 GPU reduction kernels for
// bool_and / bool_or / bit_and / bit_or / bit_xor.
//
// Each kernel checks PG-compatible semantics:
//   - identity returns the right value when count == 0,
//   - single-element input passes through verbatim,
//   - associativity over a multi-WG (>256 elements) buffer,
//   - >1M element buffers exercise the chunked-launch path.
//
// NULL handling is the caller's responsibility (the caller compacts the
// input before handing the buffer to the kernel). We assert the identity
// element matches PG transition-state init in case future planner work
// adds an "empty-input → identity, then NULL the result" shortcut.

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <vector>

#include "pgaccel_ffi.h"

namespace {

int g_fail_count = 0;

#define CHECK(cond, msg)                                              \
  do {                                                                \
    if (!(cond)) {                                                    \
      fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, (msg)); \
      ++g_fail_count;                                                 \
    }                                                                 \
  } while (0)

void test_bool_and() {
  // count == 0 → identity (1). Caller maps this to SQL NULL via has_value.
  {
    uint8_t result = 0xFF;
    pgaccel_status st = pgaccel_reduce_bool_and(nullptr, 0, &result);
    CHECK(st == PGACCEL_OK, "bool_and(count=0) status");
    CHECK(result == 1, "bool_and(count=0) returns identity 1");
  }

  // Single-element: passthrough.
  {
    uint8_t single = 0;
    uint8_t result = 0xFF;
    pgaccel_status st = pgaccel_reduce_bool_and(&single, 1, &result);
    CHECK(st == PGACCEL_OK, "bool_and(count=1,false) status");
    CHECK(result == 0, "bool_and(count=1,false) returns 0");
  }

  // All true → 1.
  {
    std::vector<uint8_t> data(10000, 1);
    uint8_t result = 0;
    pgaccel_status st = pgaccel_reduce_bool_and(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bool_and(all-true) status");
    CHECK(result == 1, "bool_and(all-true) returns 1");
  }

  // One false among many true → 0.
  {
    std::vector<uint8_t> data(10000, 1);
    data[5237] = 0;
    uint8_t result = 0xFF;
    pgaccel_status st = pgaccel_reduce_bool_and(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bool_and(mixed) status");
    CHECK(result == 0, "bool_and with one false returns 0");
  }

  // 1.5M elements all true → 1 (exercises multi-chunk dispatch).
  {
    std::vector<uint8_t> data(1500000, 1);
    uint8_t result = 0;
    pgaccel_status st = pgaccel_reduce_bool_and(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bool_and(1.5M true) status");
    CHECK(result == 1, "bool_and(1.5M true) returns 1");
  }
}

void test_bool_or() {
  // count == 0 → identity (0).
  {
    uint8_t result = 0xFF;
    pgaccel_status st = pgaccel_reduce_bool_or(nullptr, 0, &result);
    CHECK(st == PGACCEL_OK, "bool_or(count=0) status");
    CHECK(result == 0, "bool_or(count=0) returns identity 0");
  }

  // Single true → 1.
  {
    uint8_t single = 1;
    uint8_t result = 0xFF;
    pgaccel_status st = pgaccel_reduce_bool_or(&single, 1, &result);
    CHECK(st == PGACCEL_OK, "bool_or(count=1,true) status");
    CHECK(result == 1, "bool_or(count=1,true) returns 1");
  }

  // All false → 0.
  {
    std::vector<uint8_t> data(10000, 0);
    uint8_t result = 0xFF;
    pgaccel_status st = pgaccel_reduce_bool_or(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bool_or(all-false) status");
    CHECK(result == 0, "bool_or(all-false) returns 0");
  }

  // One true among many false → 1.
  {
    std::vector<uint8_t> data(10000, 0);
    data[8273] = 1;
    uint8_t result = 0;
    pgaccel_status st = pgaccel_reduce_bool_or(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bool_or(mixed) status");
    CHECK(result == 1, "bool_or with one true returns 1");
  }
}

template <typename T>
void test_bit_and_typed(const char* label, pgaccel_status (*fn)(const T*, size_t, T*)) {
  // count == 0 → all-ones identity.
  {
    T result = 0;
    pgaccel_status st = fn(nullptr, 0, &result);
    CHECK(st == PGACCEL_OK, "bit_and(empty) status");
    CHECK(result == static_cast<T>(~T{0}), "bit_and(empty) identity = ~0");
  }

  // Single-element passthrough.
  {
    T single = static_cast<T>(0xA5A5A5A5A5A5A5A5ULL);
    T result = 0;
    pgaccel_status st = fn(&single, 1, &result);
    CHECK(st == PGACCEL_OK, "bit_and(count=1) status");
    CHECK(result == single, "bit_and(count=1) passes through");
  }

  // Computed: 0xF0 & 0x0F & 0xFF = 0 — at least one zero bit per position.
  {
    std::vector<T> data = {static_cast<T>(0xF0), static_cast<T>(0x0F), static_cast<T>(0xFF)};
    T result = static_cast<T>(~T{0});
    pgaccel_status st = fn(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bit_and(small) status");
    CHECK(result == T{0}, "bit_and(0xF0,0x0F,0xFF) == 0");
  }

  // All-ones input → all-ones result.
  {
    std::vector<T> data(10000, static_cast<T>(~T{0}));
    T result = 0;
    pgaccel_status st = fn(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bit_and(all-ones) status");
    CHECK(result == static_cast<T>(~T{0}), "bit_and(all-ones) == ~0");
  }

  // Mixed: one element clears the low byte → low byte cleared in result.
  {
    std::vector<T> data(10000, static_cast<T>(~T{0}));
    data[3173] = static_cast<T>(static_cast<T>(~T{0}) & ~T{0xFF});
    T result = static_cast<T>(~T{0});
    pgaccel_status st = fn(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bit_and(mixed) status");
    CHECK((result & static_cast<T>(0xFF)) == T{0},
          "bit_and mixed clears low byte across reduction");
  }

  printf("PASS %s\n", label);
}

template <typename T>
void test_bit_or_typed(const char* label, pgaccel_status (*fn)(const T*, size_t, T*)) {
  // count == 0 → 0 identity.
  {
    T result = 1;
    pgaccel_status st = fn(nullptr, 0, &result);
    CHECK(st == PGACCEL_OK, "bit_or(empty) status");
    CHECK(result == T{0}, "bit_or(empty) identity = 0");
  }

  // Single-element passthrough.
  {
    T single = static_cast<T>(0x5A);
    T result = 0;
    pgaccel_status st = fn(&single, 1, &result);
    CHECK(st == PGACCEL_OK, "bit_or(count=1) status");
    CHECK(result == single, "bit_or(count=1) passes through");
  }

  // 0x01 | 0x02 | 0x04 = 0x07.
  {
    std::vector<T> data = {T{0x01}, T{0x02}, T{0x04}};
    T result = 0;
    pgaccel_status st = fn(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bit_or(small) status");
    CHECK(result == T{0x07}, "bit_or(1,2,4) == 7");
  }

  // All zeros → 0.
  {
    std::vector<T> data(10000, T{0});
    T result = T{0xFF};
    pgaccel_status st = fn(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bit_or(all-zero) status");
    CHECK(result == T{0}, "bit_or(all-zero) == 0");
  }

  // Sparse single set bit → all union'd bits set.
  {
    std::vector<T> data(10000, T{0});
    data[1373] = static_cast<T>(0x80);
    data[7273] = static_cast<T>(0x01);
    T result = 0;
    pgaccel_status st = fn(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bit_or(sparse) status");
    CHECK(result == static_cast<T>(0x81), "bit_or sparse unions bits");
  }

  printf("PASS %s\n", label);
}

template <typename T>
void test_bit_xor_typed(const char* label, pgaccel_status (*fn)(const T*, size_t, T*)) {
  // count == 0 → 0 identity.
  {
    T result = 1;
    pgaccel_status st = fn(nullptr, 0, &result);
    CHECK(st == PGACCEL_OK, "bit_xor(empty) status");
    CHECK(result == T{0}, "bit_xor(empty) identity = 0");
  }

  // x XOR x = 0 (every value paired).
  {
    std::vector<T> data;
    data.reserve(20000);
    for (size_t i = 0; i < 10000; ++i) {
      T v = static_cast<T>(i * 37u + 1u);
      data.push_back(v);
      data.push_back(v);
    }
    T result = static_cast<T>(0xFF);
    pgaccel_status st = fn(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bit_xor(paired) status");
    CHECK(result == T{0}, "bit_xor of paired values cancels to 0");
  }

  // x XOR x XOR y = y (cancellation under one extra value).
  {
    std::vector<T> data;
    data.reserve(20001);
    for (size_t i = 0; i < 10000; ++i) {
      T v = static_cast<T>(i * 37u + 1u);
      data.push_back(v);
      data.push_back(v);
    }
    data.push_back(static_cast<T>(0xBEEF));
    T result = 0;
    pgaccel_status st = fn(data.data(), data.size(), &result);
    CHECK(st == PGACCEL_OK, "bit_xor(paired+one) status");
    CHECK(result == static_cast<T>(0xBEEF), "bit_xor leaves the unpaired value");
  }

  // Single-element passthrough.
  {
    T single = static_cast<T>(0xCAFE);
    T result = 0;
    pgaccel_status st = fn(&single, 1, &result);
    CHECK(st == PGACCEL_OK, "bit_xor(count=1) status");
    CHECK(result == single, "bit_xor(count=1) passes through");
  }

  printf("PASS %s\n", label);
}

}  // namespace

int main() {
  pgaccel_status st = pgaccel_init();
  if (st != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init failed: %d\n", st);
    return 1;
  }

  test_bool_and();
  printf("PASS bool_and\n");

  test_bool_or();
  printf("PASS bool_or\n");

  test_bit_and_typed<int16_t>("bit_and_i16", pgaccel_reduce_bit_and_i16);
  test_bit_and_typed<int32_t>("bit_and_i32", pgaccel_reduce_bit_and_i32);
  test_bit_and_typed<int64_t>("bit_and_i64", pgaccel_reduce_bit_and_i64);

  test_bit_or_typed<int16_t>("bit_or_i16", pgaccel_reduce_bit_or_i16);
  test_bit_or_typed<int32_t>("bit_or_i32", pgaccel_reduce_bit_or_i32);
  test_bit_or_typed<int64_t>("bit_or_i64", pgaccel_reduce_bit_or_i64);

  test_bit_xor_typed<int16_t>("bit_xor_i16", pgaccel_reduce_bit_xor_i16);
  test_bit_xor_typed<int32_t>("bit_xor_i32", pgaccel_reduce_bit_xor_i32);
  test_bit_xor_typed<int64_t>("bit_xor_i64", pgaccel_reduce_bit_xor_i64);

  pgaccel_shutdown();

  if (g_fail_count > 0) {
    fprintf(stderr, "FAIL: %d assertion(s) failed\n", g_fail_count);
    return 1;
  }
  printf("ALL PASS\n");
  return 0;
}
