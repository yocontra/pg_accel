// Exact lookup semantics from the production device helper, without a GPU
// library or a PostgreSQL server. This is not a device qualification test.
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <vector>

#include "pgaccel_raster_lookup.hpp"

namespace {
using Rule = pgaccel_resident_raster_reclass_rule;
constexpr double epsilon = 1.1920928955078125e-7;
size_t checked = 0;
size_t failed = 0;
uint64_t random_state = UINT64_C(0x9e3779b97f4a7c15);

uint64_t random_bits() {
  random_state ^= random_state << 13;
  random_state ^= random_state >> 7;
  random_state ^= random_state << 17;
  return random_state;
}

int64_t linear(const Rule* rules, size_t count, double value) {
  for (size_t index = 0; index < count; ++index) {
    const double source = static_cast<double>(rules[index].source);
    if (source == value || std::fabs(source - value) <= epsilon)
      return rules[index].destination;
  }
  return 0;
}

void check(const std::vector<Rule>& rules, double value) {
  const int64_t expected = linear(rules.data(), rules.size(), value);
  const int64_t actual =
      rules.size() <= pgaccel_raster_detail::linear_rule_limit
          ? pgaccel_raster_detail::reclass_lookup_linear(rules.data(), rules.size(), value)
          : pgaccel_raster_detail::reclass_lookup_indexed(rules.data(), rules.size(), value);
  ++checked;
  if (expected != actual) {
    if (failed < 10)
      std::fprintf(stderr, "FAIL n=%zu value=%a expected=%lld actual=%lld\n", rules.size(), value,
                   static_cast<long long>(expected), static_cast<long long>(actual));
    ++failed;
  }
}

void boundaries(const std::vector<Rule>& rules) {
  for (double value :
       {0.0, -0.0, std::numeric_limits<double>::denorm_min(),
        -std::numeric_limits<double>::denorm_min(), std::numeric_limits<double>::infinity(),
        -std::numeric_limits<double>::infinity(), std::numeric_limits<double>::quiet_NaN(),
        -std::numeric_limits<double>::quiet_NaN()})
    check(rules, value);
  for (const Rule& rule : rules) {
    const double source = static_cast<double>(rule.source);
    for (double value : {source, source - epsilon, source + epsilon, source - 0.5, source + 0.5,
                         source - 1, source + 1}) {
      check(rules, value);
      check(rules, std::nextafter(value, -std::numeric_limits<double>::infinity()));
      check(rules, std::nextafter(value, std::numeric_limits<double>::infinity()));
      check(rules, static_cast<double>(static_cast<float>(value)));
    }
  }
}

volatile int64_t benchmark_sink = 0;
using Lookup = int64_t (*)(const Rule*, size_t, double);

// Keep both arms behind the same non-inlined function-pointer driver. Otherwise
// the compiler can inline only one arm and time different dispatch overhead.
__attribute__((noinline)) double elapsed_ns(Lookup lookup, const std::vector<Rule>& rules,
                                            const std::vector<double>& values, int64_t& checksum) {
  const auto start = std::chrono::steady_clock::now();
  int64_t sum = 0;
  for (double value : values)
    sum += lookup(rules.data(), rules.size(), value);
  const auto end = std::chrono::steady_clock::now();
  checksum = sum;
  benchmark_sink = sum;
  return std::chrono::duration<double, std::nano>(end - start).count();
}

void benchmark() {
  for (size_t count : {size_t{1}, size_t{4}, size_t{16}, size_t{17}, size_t{64}}) {
    const Lookup candidate = count <= pgaccel_raster_detail::linear_rule_limit
                                 ? pgaccel_raster_detail::reclass_lookup_linear
                                 : pgaccel_raster_detail::reclass_lookup_indexed;
    std::vector<Rule> rules;
    for (size_t index = 0; index < count; ++index)
      rules.push_back({static_cast<int64_t>(index), static_cast<int64_t>(index + 1)});
    for (unsigned pattern = 0; pattern < 6; ++pattern) {
      const char* names[] = {"first", "last", "mixed", "miss", "boundary", "interior_miss"};
      std::vector<double> values(1000000);
      for (double& value : values) {
        if (pattern == 0)
          value = 0;
        else if (pattern == 1)
          value = static_cast<double>(count - 1);
        else if (pattern == 2)
          value = static_cast<double>(random_bits() % (count * 2)) - static_cast<double>(count / 2);
        else if (pattern == 3)
          value = -1;
        else
          value = static_cast<double>(random_bits() % count) + (pattern == 4 ? epsilon : 0.25);
      }
      std::vector<double> old_times, new_times;
      for (unsigned pair = 0; pair < 24; ++pair) {
        int64_t old_sum = 0, new_sum = 0;
        double old_ns, new_ns;
        if (pair % 2) {
          new_ns = elapsed_ns(candidate, rules, values, new_sum);
          old_ns = elapsed_ns(linear, rules, values, old_sum);
        } else {
          old_ns = elapsed_ns(linear, rules, values, old_sum);
          new_ns = elapsed_ns(candidate, rules, values, new_sum);
        }
        if (old_sum != new_sum) {
          ++failed;
          return;
        }
        if (pair >= 3) {
          old_times.push_back(old_ns);
          new_times.push_back(new_ns);
        }
      }
      auto old_sorted = old_times, new_sorted = new_times;
      std::sort(old_sorted.begin(), old_sorted.end());
      std::sort(new_sorted.begin(), new_sorted.end());
      std::printf("{\"rules\":%zu,\"pattern\":\"%s\",\"rows\":%zu,\"linear_median_ns\":%.0f,"
                  "\"candidate_median_ns\":%.0f,\"linear_ns\":[",
                  count, names[pattern], values.size(), old_sorted[10], new_sorted[10]);
      for (size_t i = 0; i < old_times.size(); ++i)
        std::printf("%s%.0f", i ? "," : "", old_times[i]);
      std::printf("],\"candidate_ns\":[");
      for (size_t i = 0; i < new_times.size(); ++i)
        std::printf("%s%.0f", i ? "," : "", new_times[i]);
      std::printf("]}\n");
    }
  }
}
}  // namespace

int main(int argc, char** argv) {
  if (argc == 2 && std::strcmp(argv[1], "--bench") == 0) {
    benchmark();
    return failed == 0 ? 0 : 1;
  }
  // Zero rules is an internal helper boundary, not a new accepted public ABI
  // request. Native validation still requires a nonempty sorted rule set.
  for (size_t count = 0; count <= 64; ++count) {
    for (unsigned pattern = 0; pattern < 4; ++pattern) {
      std::vector<Rule> rules;
      for (size_t index = 0; index < count; ++index) {
        int64_t source;
        if (pattern == 0)
          source = static_cast<int64_t>(index) - 32;
        else if (pattern == 1)
          source = INT64_C(-2147483648) + static_cast<int64_t>(index);
        else if (pattern == 2)
          source =
              INT64_C(4294967295) - static_cast<int64_t>(count) + static_cast<int64_t>(index) + 1;
        else
          source =
              INT64_C(-2147483648) + static_cast<int64_t>(random_bits() % UINT64_C(6442450944));
        rules.push_back({source, index % 2 ? static_cast<int64_t>(index + 1)
                                           : -static_cast<int64_t>(index + 1)});
      }
      std::sort(rules.begin(), rules.end(),
                [](const Rule& a, const Rule& b) { return a.source < b.source; });
      rules.erase(std::unique(rules.begin(), rules.end(),
                              [](const Rule& a, const Rule& b) { return a.source == b.source; }),
                  rules.end());
      boundaries(rules);
      for (unsigned sample = 0; sample < 2048; ++sample) {
        const uint64_t bits = random_bits();
        double value;
        std::memcpy(&value, &bits, sizeof(value));
        check(rules, value);
        check(rules,
              static_cast<double>(INT64_C(-2147483648) +
                                  static_cast<int64_t>(random_bits() % UINT64_C(6442450944))) +
                  0.25);
      }
    }
  }
  std::printf("raster lookup: %zu checked, %zu failed\n", checked, failed);
  return failed == 0 ? 0 : 1;
}
