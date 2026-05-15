/*
 * test_nested_loop_ineq.cpp — Standalone tests for the GPU NLJ scalar
 * inequality kernel. Mirrors the test patterns in `test_bbox.cpp`.
 *
 * Coverage:
 *   - Empty inputs return OK with zero matches.
 *   - Each of the four ineq opcodes against a known oracle.
 *   - BETWEEN-shape against a known oracle, including a 1000×100 event-
 *     vs-window dataset that mirrors the planned bench cell.
 *   - Overflow detection: `max_pairs` smaller than the match count must
 *     report the true count in `*pair_count_out`.
 *   - Null-pointer rejection.
 */

#include <cassert>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_nested_loop_ineq.h"

static int tests_passed = 0;
static int tests_failed = 0;

#define CHECK(cond, msg)                                      \
  do {                                                        \
    if (!(cond)) {                                            \
      fprintf(stderr, "FAIL: %s (line %d)\n", msg, __LINE__); \
      ++tests_failed;                                         \
    } else {                                                  \
      ++tests_passed;                                         \
    }                                                         \
  } while (0)

namespace {

template <typename T>
bool ref_eval(T a, T b, pgaccel_nlj_ineq_op op) {
  switch (op) {
    case PGACCEL_NLJ_LT:
      return a < b;
    case PGACCEL_NLJ_LE:
      return a <= b;
    case PGACCEL_NLJ_GE:
      return a >= b;
    case PGACCEL_NLJ_GT:
      return a > b;
  }
  return false;
}

template <typename T>
size_t oracle_count(const std::vector<T>& outer, const std::vector<T>& inner,
                    pgaccel_nlj_ineq_op op) {
  size_t c = 0;
  for (size_t i = 0; i < outer.size(); ++i) {
    for (size_t j = 0; j < inner.size(); ++j) {
      if (ref_eval(outer[i], inner[j], op))
        ++c;
    }
  }
  return c;
}

}  // namespace

static void test_empty_inputs_i64() {
  size_t pair_count = 999;
  uint32_t pairs[2] = {99, 99};
  int64_t dummy = 0;

  pgaccel_status s =
      pgaccel_nlj_ineq_i64(nullptr, 0, nullptr, 0, PGACCEL_NLJ_LT, pairs, 1, &pair_count);
  CHECK(s == PGACCEL_OK, "empty i64 ineq returns OK");
  CHECK(pair_count == 0, "empty i64 ineq pair_count == 0");

  // Even if buffers are non-null, n==0 short-circuits with OK.
  pair_count = 999;
  s = pgaccel_nlj_ineq_i64(&dummy, 0, &dummy, 0, PGACCEL_NLJ_LT, pairs, 1, &pair_count);
  CHECK(s == PGACCEL_OK, "n=0 i64 ineq returns OK");
  CHECK(pair_count == 0, "n=0 i64 ineq pair_count == 0");
}

static void test_null_pointers_i64() {
  int64_t outer = 1;
  int64_t inner = 2;
  uint32_t pairs[2] = {0, 0};
  size_t pair_count = 0;

  pgaccel_status s =
      pgaccel_nlj_ineq_i64(nullptr, 1, &inner, 1, PGACCEL_NLJ_LT, pairs, 1, &pair_count);
  CHECK(s == PGACCEL_ERROR, "null outer rejected");

  s = pgaccel_nlj_ineq_i64(&outer, 1, nullptr, 1, PGACCEL_NLJ_LT, pairs, 1, &pair_count);
  CHECK(s == PGACCEL_ERROR, "null inner rejected");

  s = pgaccel_nlj_ineq_i64(&outer, 1, &inner, 1, PGACCEL_NLJ_LT, nullptr, 1, &pair_count);
  CHECK(s == PGACCEL_ERROR, "null pairs_out rejected");

  s = pgaccel_nlj_ineq_i64(&outer, 1, &inner, 1, PGACCEL_NLJ_LT, pairs, 1, nullptr);
  CHECK(s == PGACCEL_ERROR, "null pair_count_out rejected");
}

static void test_ineq_oracle_small_i64() {
  // 5 outer x 5 inner = 25 pairs. For each op, count matches and verify
  // every emitted index pair satisfies the predicate. Indexes 0..N-1 so
  // we can re-derive values from the index.
  std::vector<int64_t> outer = {1, 2, 3, 4, 5};
  std::vector<int64_t> inner = {1, 2, 3, 4, 5};
  const size_t cap = 64;
  std::vector<uint32_t> pairs(cap * 2, 0);

  for (pgaccel_nlj_ineq_op op : {PGACCEL_NLJ_LT, PGACCEL_NLJ_LE, PGACCEL_NLJ_GE, PGACCEL_NLJ_GT}) {
    size_t pair_count = 0;
    pgaccel_status s = pgaccel_nlj_ineq_i64(outer.data(), outer.size(), inner.data(), inner.size(),
                                            op, pairs.data(), cap, &pair_count);
    char msg[64];
    snprintf(msg, sizeof(msg), "small ineq op=%d returns OK", static_cast<int>(op));
    CHECK(s == PGACCEL_OK, msg);

    const size_t expected = oracle_count(outer, inner, op);
    snprintf(msg, sizeof(msg), "small ineq op=%d count matches oracle (%zu)", static_cast<int>(op),
             expected);
    CHECK(pair_count == expected, msg);

    // Verify every emitted pair satisfies the predicate.
    for (size_t k = 0; k < pair_count; ++k) {
      uint32_t i = pairs[k * 2 + 0];
      uint32_t j = pairs[k * 2 + 1];
      CHECK(i < outer.size(), "emitted i in range");
      CHECK(j < inner.size(), "emitted j in range");
      CHECK(ref_eval(outer[i], inner[j], op), "emitted pair satisfies predicate");
    }
  }
}

static void test_overflow_detection() {
  // 4 outer x 4 inner = 16 pairs. With LE and identical [1..4] arrays the
  // oracle count is 1+2+3+4 = 10. Set max_pairs = 4 — kernel should still
  // report pair_count == 10 so the caller can detect overflow.
  std::vector<int64_t> outer = {1, 2, 3, 4};
  std::vector<int64_t> inner = {1, 2, 3, 4};
  const size_t cap = 4;
  std::vector<uint32_t> pairs(cap * 2, 0);
  size_t pair_count = 0;

  pgaccel_status s = pgaccel_nlj_ineq_i64(outer.data(), outer.size(), inner.data(), inner.size(),
                                          PGACCEL_NLJ_LE, pairs.data(), cap, &pair_count);
  CHECK(s == PGACCEL_OK, "overflow scenario returns OK");
  const size_t expected = oracle_count(outer, inner, PGACCEL_NLJ_LE);
  CHECK(pair_count == expected, "overflow pair_count reports true total");
  CHECK(pair_count > cap, "overflow detected (pair_count > max_pairs)");
}

static void test_between_oracle_small() {
  // 5 outer events, 3 inner windows. Each window is (lo[j], hi[j]).
  std::vector<int64_t> outer = {1, 5, 10, 15, 25};
  std::vector<int64_t> lo = {0, 9, 20};
  std::vector<int64_t> hi = {5, 16, 30};
  // Expected matches: (outer >= lo) && (outer <= hi)
  // outer=1: in [0,5]? yes. in [9,16]? no. in [20,30]? no. → 1 match
  // outer=5: in [0,5]? yes (edge). [9,16]? no. [20,30]? no. → 1 match
  // outer=10: [0,5]? no. [9,16]? yes. [20,30]? no. → 1 match
  // outer=15: [0,5]? no. [9,16]? yes. [20,30]? no. → 1 match
  // outer=25: [0,5]? no. [9,16]? no. [20,30]? yes. → 1 match
  // Total: 5
  const size_t cap = 64;
  std::vector<uint32_t> pairs(cap * 2, 0);
  size_t pair_count = 0;

  pgaccel_status s = pgaccel_nlj_between_i64(outer.data(), outer.size(), lo.data(), hi.data(),
                                             lo.size(), pairs.data(), cap, &pair_count);
  CHECK(s == PGACCEL_OK, "between small returns OK");
  CHECK(pair_count == 5, "between small count matches oracle (5)");

  // Validate each emitted pair.
  for (size_t k = 0; k < pair_count; ++k) {
    uint32_t i = pairs[k * 2 + 0];
    uint32_t j = pairs[k * 2 + 1];
    CHECK(i < outer.size(), "emitted i in range");
    CHECK(j < lo.size(), "emitted j in range");
    int64_t x = outer[i];
    CHECK(x >= lo[j] && x <= hi[j], "emitted pair satisfies BETWEEN predicate");
  }
}

static void test_between_bench_shape() {
  // 1000 events × 100 non-overlapping windows = exactly 1000 matches
  // (the bench-cell scenario from the launchpad: each event lies in
  // exactly one window). Windows: [k*10, k*10+9] for k in 0..100.
  // Events: 10 events per window at positions [k*10+1..k*10+9]
  // (skip k*10 to avoid edge-coincidence ambiguity).
  const size_t n_windows = 100;
  const size_t events_per = 10;
  const size_t n_events = n_windows * events_per;

  std::vector<int64_t> outer;
  outer.reserve(n_events);
  for (size_t k = 0; k < n_windows; ++k) {
    for (size_t e = 0; e < events_per; ++e) {
      outer.push_back(static_cast<int64_t>(k * 10 + e));
    }
  }

  std::vector<int64_t> lo, hi;
  lo.reserve(n_windows);
  hi.reserve(n_windows);
  for (size_t k = 0; k < n_windows; ++k) {
    lo.push_back(static_cast<int64_t>(k * 10));
    hi.push_back(static_cast<int64_t>(k * 10 + 9));
  }

  const size_t cap = n_events + 100;  // headroom
  std::vector<uint32_t> pairs(cap * 2, 0);
  size_t pair_count = 0;

  pgaccel_status s = pgaccel_nlj_between_i64(outer.data(), outer.size(), lo.data(), hi.data(),
                                             lo.size(), pairs.data(), cap, &pair_count);
  CHECK(s == PGACCEL_OK, "between bench-shape returns OK");
  CHECK(pair_count == n_events, "between bench-shape count = n_events");
}

static void test_between_f64_small() {
  std::vector<double> outer = {0.5, 1.5, 2.5};
  std::vector<double> lo = {0.0, 2.0};
  std::vector<double> hi = {1.0, 3.0};
  // outer=0.5: [0,1]? yes. [2,3]? no. → 1
  // outer=1.5: [0,1]? no. [2,3]? no. → 0
  // outer=2.5: [0,1]? no. [2,3]? yes. → 1
  const size_t cap = 16;
  std::vector<uint32_t> pairs(cap * 2, 0);
  size_t pair_count = 0;

  pgaccel_status s = pgaccel_nlj_between_f64(outer.data(), outer.size(), lo.data(), hi.data(),
                                             lo.size(), pairs.data(), cap, &pair_count);
  CHECK(s == PGACCEL_OK, "between f64 small returns OK");
  CHECK(pair_count == 2, "between f64 small count == 2");
}

int main() {
  pgaccel_init();

  printf("Running NLJ inequality kernel tests...\n");
  test_empty_inputs_i64();
  test_null_pointers_i64();
  test_ineq_oracle_small_i64();
  test_overflow_detection();
  test_between_oracle_small();
  test_between_bench_shape();
  test_between_f64_small();

  printf("\nResults: %d passed, %d failed\n", tests_passed, tests_failed);

  pgaccel_shutdown();
  return tests_failed > 0 ? 1 : 0;
}
