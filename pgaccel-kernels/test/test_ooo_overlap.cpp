#include <algorithm>
#include <cstdio>
#include <cstdlib>

#include "ooo_overlap_support.h"

static size_t env_size(const char* name, size_t fallback) {
  const char* raw = std::getenv(name);
  if (raw == nullptr || raw[0] == '\0')
    return fallback;
  char* end = nullptr;
  unsigned long long value = std::strtoull(raw, &end, 10);
  if (end == raw || value == 0)
    return fallback;
  return static_cast<size_t>(value);
}

static double env_double(const char* name, double fallback) {
  const char* raw = std::getenv(name);
  if (raw == nullptr || raw[0] == '\0')
    return fallback;
  char* end = nullptr;
  double value = std::strtod(raw, &end);
  if (end == raw || value <= 0.0)
    return fallback;
  return value;
}

int main() {
  if (pgaccel_init() != PGACCEL_OK) {
    std::fprintf(stderr, "test_ooo_overlap: pgaccel_init failed\n");
    return 1;
  }

  pgaccel_platform_caps caps = pgaccel_get_caps();
  pgaccel_device_info info = pgaccel_get_device_info();
  std::printf("device=%s backend=%s ooo_queue=%s\n", info.device_name, info.backend_name,
              caps.has_ooo_queue ? "yes" : "no");
  if (!caps.has_ooo_queue) {
    std::printf("test_ooo_overlap: out-of-order queue unavailable - skipping\n");
    pgaccel_shutdown();
    return 0;
  }

  const size_t count = env_size("PGACCEL_OOO_OVERLAP_COUNT", 8192);
  const uint32_t spin =
      static_cast<uint32_t>(std::min<size_t>(env_size("PGACCEL_OOO_OVERLAP_SPIN", 512), 1u << 20));
  const double min_speedup = env_double("PGACCEL_OOO_OVERLAP_MIN_SPEEDUP", 1.01);

  pgaccel_ooo_overlap_report report = {};
  pgaccel_status st = pgaccel_resident_reduce_overlap_probe(count, spin, &report);
  if (st != PGACCEL_OK) {
    std::fprintf(stderr, "test_ooo_overlap: probe failed status=%d\n", static_cast<int>(st));
    pgaccel_shutdown();
    return 1;
  }

  const double serial_ms = static_cast<double>(report.serial_wall_ns) / 1.0e6;
  const double overlap_ms = static_cast<double>(report.overlap_wall_ns) / 1.0e6;
  const double speedup = overlap_ms > 0.0 ? serial_ms / overlap_ms : 0.0;
  const double reduce_ms =
      static_cast<double>(report.reduce_end_ns - report.reduce_start_ns) / 1.0e6;
  const double resident_ms =
      static_cast<double>(report.resident_end_ns - report.resident_start_ns) / 1.0e6;
  const double final_ms = static_cast<double>(report.final_end_ns - report.final_start_ns) / 1.0e6;

  std::printf("count=%zu spin=%u\n", count, spin);
  std::printf("serial_wall_ms=%.3f overlap_wall_ms=%.3f speedup_x=%.3f\n", serial_ms, overlap_ms,
              speedup);
  std::printf("overlap_trace_ns reduce=[%llu,%llu] resident=[%llu,%llu] final=[%llu,%llu]\n",
              static_cast<unsigned long long>(report.reduce_start_ns),
              static_cast<unsigned long long>(report.reduce_end_ns),
              static_cast<unsigned long long>(report.resident_start_ns),
              static_cast<unsigned long long>(report.resident_end_ns),
              static_cast<unsigned long long>(report.final_start_ns),
              static_cast<unsigned long long>(report.final_end_ns));
  std::printf("span_ms reduce=%.3f resident=%.3f final=%.3f spans_overlap=%s improved=%s\n",
              reduce_ms, resident_ms, final_ms, report.spans_overlap ? "yes" : "no",
              report.wall_time_improved ? "yes" : "no");

  pgaccel_shutdown();

  if (!report.spans_overlap) {
    std::fprintf(stderr, "test_ooo_overlap: resident/reduce GPU spans did not overlap\n");
    return 1;
  }
  if (!report.wall_time_improved || speedup < min_speedup) {
    std::fprintf(stderr, "test_ooo_overlap: overlap speedup %.3fx below threshold %.3fx\n", speedup,
                 min_speedup);
    return 1;
  }

  std::printf("test_ooo_overlap: OK\n");
  return 0;
}
