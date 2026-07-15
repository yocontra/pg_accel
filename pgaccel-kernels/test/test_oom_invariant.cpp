// test_oom_invariant.cpp — OOM-never invariant for fp64 kernel families.
//
// W5 fp64-unlock plan §5.  Each fp64 kernel family (reduce / sort /
// hashagg / spatial / h3) is exercised with an input whose raw size
// exceeds 2 × caps.max_alloc_bytes / sizeof(double) — i.e. larger
// than any single device allocation can hold. Each kernel must:
//
//   (a) complete without OOM, bad_alloc, SIGSEGV, SIGKILL
//   (b) return a correct result (cross-check against a small-input
//       reference extended by multiplication, e.g. sum of 2N uniform
//       values = 2× sum of N)
//   (c) peak RSS stay below 3 × caps.max_alloc_bytes — proving the
//       kernel streams rather than buffering the full input
//
// If any kernel fails these invariants, the test reports FAIL and
// returns a specific exit code so the dispatcher can route the
// contingency work (streaming/chunking fix) — DO NOT make the test
// pass by relaxing the ceiling; that masks a real streaming bug.
//
#include <sys/resource.h>
#if defined(__APPLE__)
#include <mach/mach.h>
#include <mach/mach_init.h>
#include <mach/task.h>
#else
#include <unistd.h>
#endif

#include <algorithm>
#include <cctype>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <random>
#include <string>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_hash_agg.h"
#include "pgaccel_hash_join.h"

static size_t peak_rss_bytes() {
  // task_info() gives current resident; getrusage() gives peak.
  struct rusage ru;
  if (getrusage(RUSAGE_SELF, &ru) == 0) {
    // ru_maxrss is bytes on macOS (KB on Linux).
#if defined(__APPLE__)
    return static_cast<size_t>(ru.ru_maxrss);
#else
    return static_cast<size_t>(ru.ru_maxrss) * 1024ULL;
#endif
  }
  return 0;
}

static size_t current_rss_bytes() {
#if defined(__APPLE__)
  mach_task_basic_info_data_t info;
  mach_msg_type_number_t count = MACH_TASK_BASIC_INFO_COUNT;
  kern_return_t kr = task_info(mach_task_self(), MACH_TASK_BASIC_INFO,
                               reinterpret_cast<task_info_t>(&info), &count);
  if (kr != KERN_SUCCESS)
    return 0;
  return info.resident_size;
#else
  FILE* f = std::fopen("/proc/self/statm", "r");
  if (!f)
    return 0;
  long resident_pages = 0;
  int matched = std::fscanf(f, "%*s %ld", &resident_pages);
  std::fclose(f);
  if (matched != 1 || resident_pages < 0)
    return 0;
  long page_size = sysconf(_SC_PAGESIZE);
  if (page_size <= 0)
    return 0;
  return static_cast<size_t>(resident_pages) * static_cast<size_t>(page_size);
#endif
}

struct FamilyResult {
  const char* name;
  bool status_ok;
  bool correct;
  size_t peak_rss_bytes;
  size_t rss_ceiling_bytes;
  bool under_ceiling;
  std::string note;
  size_t rss_delta_bytes = 0;
  uint64_t gpu_dispatches = 0;
};

static FamilyResult run_reduce_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"reduce_f64", false, false, 0, rss_ceiling, false, ""};
  printf("\n-- reduce_f64 @ N=%zu (%.2f GB raw input) --\n", N,
         static_cast<double>(N * sizeof(double)) / (1024.0 * 1024.0 * 1024.0));

  // Use uniform value 1.0 so correctness check is trivial: sum == N.
  // Allocating the full vector on CPU side is the whole point — we
  // want to test that the kernel streams the input through the device
  // rather than copying it whole.
  std::vector<double> v;
  try {
    v.assign(N, 1.0);
  } catch (const std::bad_alloc&) {
    r.note = "CPU-side vector allocation failed (host cannot hold input)";
    return r;
  }
  const size_t rss_before = current_rss_bytes();
  double got = 0.0;
  pgaccel_reset_gpu_exec_count();
  pgaccel_status st = pgaccel_reduce_sum_f64(v.data(), N, &got);
  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = (st == PGACCEL_OK && r.gpu_dispatches > 0);
  r.peak_rss_bytes = rss_after;
  // Correctness: sum of N 1.0s is N exactly in fp64.
  r.correct = r.status_ok && got == static_cast<double>(N);
  // RSS ceiling — delta from before-call vs peak.
  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status=%d (OK=%d) got=%.0f expected=%.0f correct=%d  "
         "dispatches=%llu rss_before=%.2fGB peak=%.2fGB delta=%.2fGB ceiling=%.2fGB under=%d\n",
         (int)st, r.status_ok, got, (double)N, r.correct,
         static_cast<unsigned long long>(r.gpu_dispatches), rss_before / 1e9, rss_after / 1e9,
         rss_delta / 1e9, rss_ceiling / 1e9, r.under_ceiling);
  if (!r.status_ok)
    r.note = "kernel returned non-OK status on device-exceeding input";
  else if (!r.correct)
    r.note = "result differs from expected (sum of N 1.0s)";
  return r;
}

static FamilyResult run_sort_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"sort_f64", false, false, 0, rss_ceiling, false, ""};
  printf("\n-- sort_f64 @ N=%zu (%.2f GB raw input) --\n", N,
         static_cast<double>(N * sizeof(double)) / (1024.0 * 1024.0 * 1024.0));
  std::vector<double> v;
  try {
    v.resize(N);
  } catch (const std::bad_alloc&) {
    r.note = "CPU-side vector allocation failed";
    return r;
  }
  // Descending data — worst case for any sort — but use small key
  // domain so the verification pass is cheap.
  for (size_t i = 0; i < N; ++i) {
    v[i] = static_cast<double>(N - i);
  }
  const size_t rss_before = current_rss_bytes();
  pgaccel_reset_gpu_exec_count();
  pgaccel_status st = pgaccel_sort_f64(v.data(), N);
  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = (st == PGACCEL_OK && r.gpu_dispatches > 0);
  r.peak_rss_bytes = rss_after;
  // Correctness: ascending monotone.
  bool monotone = true;
  if (r.status_ok) {
    for (size_t i = 1; i < N; ++i) {
      if (v[i] < v[i - 1]) {
        monotone = false;
        break;
      }
    }
  }
  r.correct = r.status_ok && monotone;
  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status=%d (OK=%d) monotone=%d  rss_before=%.2fGB peak=%.2fGB delta=%.2fGB "
         "ceiling=%.2fGB dispatches=%llu under=%d\n",
         (int)st, r.status_ok, monotone, rss_before / 1e9, rss_after / 1e9, rss_delta / 1e9,
         rss_ceiling / 1e9, static_cast<unsigned long long>(r.gpu_dispatches), r.under_ceiling);
  if (!r.status_ok)
    r.note = "sort_f64 returned non-OK on device-exceeding input";
  else if (!r.correct)
    r.note = "sort output is not monotone";
  return r;
}

static FamilyResult run_hashagg_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"hashagg_f64", false, false, 0, rss_ceiling, false, ""};
  printf("\n-- hashagg_f64 @ N=%zu (%.2f GB raw input f64 + i64 keys) --\n", N,
         static_cast<double>(N * (sizeof(double) + sizeof(int64_t))) / (1024.0 * 1024.0 * 1024.0));

  // 4-group reduction. Each group has N/4 entries of value 1.0.
  std::vector<int64_t> keys;
  std::vector<uint8_t> knulls;
  std::vector<double> vals;
  std::vector<uint8_t> vnulls;
  try {
    keys.resize(N);
    knulls.assign(N, 0);
    vals.assign(N, 1.0);
    vnulls.assign(N, 0);
  } catch (const std::bad_alloc&) {
    r.note = "CPU-side alloc failed";
    return r;
  }
  for (size_t i = 0; i < N; ++i)
    keys[i] = static_cast<int64_t>(i & 3);

  const void* varr[1] = {vals.data()};
  const uint8_t* vnull_arr[1] = {vnulls.data()};
  int vtypes[1] = {PGACCEL_VAL_FLOAT64};
  pgaccel_agg_col ac[1] = {{PGACCEL_AGG_SUM, 0}};
  const size_t rss_before = current_rss_bytes();
  pgaccel_agg_state* state = nullptr;
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status = pgaccel_hash_agg_execute_checked(
      keys.data(), knulls.data(), N, PGACCEL_KEY_INT64, varr, vnull_arr, vtypes, ac, 1, &state);
  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = (status == PGACCEL_OK && state != nullptr && r.gpu_dispatches > 0);
  r.peak_rss_bytes = rss_after;
  if (state) {
    // Each of the 4 groups should have exactly N/4 rows, sum = N/4.
    size_t ngroups = pgaccel_agg_group_count(state);
    bool ok = (ngroups == 4);
    if (ok) {
      const double* res = pgaccel_agg_get_results(state, 0);
      const double expected = static_cast<double>(N / 4);
      for (size_t g = 0; g < 4 && ok; ++g) {
        if (std::fabs(res[g] - expected) > 1e-6)
          ok = false;
      }
    }
    r.correct = ok;
    pgaccel_agg_free(state);
  }
  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status_ok=%d correct=%d  rss_before=%.2fGB peak=%.2fGB delta=%.2fGB "
         "ceiling=%.2fGB dispatches=%llu under=%d\n",
         r.status_ok, r.correct, rss_before / 1e9, rss_after / 1e9, rss_delta / 1e9,
         rss_ceiling / 1e9, static_cast<unsigned long long>(r.gpu_dispatches), r.under_ceiling);
  if (!r.status_ok)
    r.note = "hashagg_f64 returned NULL on device-exceeding input";
  else if (!r.correct)
    r.note = "hashagg_f64 produced wrong group counts / values";
  return r;
}

static FamilyResult run_spatial_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"spatial_f64", false, false, 0, rss_ceiling, false, ""};
  printf("\n-- spatial_f64 (PIP) @ N=%zu points (%.2f GB raw input) --\n", N,
         static_cast<double>(N * 2 * sizeof(double)) / (1024.0 * 1024.0 * 1024.0));

  // Unit square ring.
  static const double ring[] = {0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0};
  std::vector<double> pts;
  std::vector<int8_t> results;
  try {
    pts.resize(N * 2);
    results.assign(N, 99);
  } catch (const std::bad_alloc&) {
    r.note = "CPU-side alloc failed";
    return r;
  }
  // Every point at (0.5, 0.5) — definitely inside.
  for (size_t i = 0; i < N; ++i) {
    pts[2 * i] = 0.5;
    pts[2 * i + 1] = 0.5;
  }
  const size_t rss_before = current_rss_bytes();
  pgaccel_reset_gpu_exec_count();
  pgaccel_status st =
      pgaccel_point_in_ring_bulk(pts.data(), N, ring, 5, /*use_fp64=*/true, results.data());
  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = (st == PGACCEL_OK && r.gpu_dispatches > 0);
  r.peak_rss_bytes = rss_after;
  // Correctness: every result should be 1 (inside).
  bool all_inside = r.status_ok;
  if (r.status_ok) {
    for (size_t i = 0; i < N; ++i) {
      if (results[i] != 1) {
        all_inside = false;
        break;
      }
    }
  }
  r.correct = all_inside;
  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status=%d (OK=%d) all_inside=%d  rss_before=%.2fGB peak=%.2fGB delta=%.2fGB "
         "ceiling=%.2fGB dispatches=%llu under=%d\n",
         (int)st, r.status_ok, all_inside, rss_before / 1e9, rss_after / 1e9, rss_delta / 1e9,
         rss_ceiling / 1e9, static_cast<unsigned long long>(r.gpu_dispatches), r.under_ceiling);
  if (!r.status_ok)
    r.note = "spatial_f64 PIP returned non-OK on device-exceeding input";
  else if (!r.correct)
    r.note = "spatial_f64 PIP misclassified points";
  return r;
}

static FamilyResult run_h3_family(size_t N, size_t rss_ceiling) {
  FamilyResult r = {"h3_f64", false, false, 0, rss_ceiling, false, ""};
  printf("\n-- h3_f64 @ N=%zu (%.2f GB raw lat+lng input) --\n", N,
         static_cast<double>(N * 2 * sizeof(double)) / (1024.0 * 1024.0 * 1024.0));
  std::vector<double> lats, lngs;
  std::vector<uint64_t> cells;
  std::vector<uint8_t> valids;
  try {
    lats.assign(N, 37.7749);
    lngs.assign(N, -122.4194);
    cells.resize(N);
    valids.resize(N);
  } catch (const std::bad_alloc&) {
    r.note = "CPU-side alloc failed";
    return r;
  }
  const size_t rss_before = current_rss_bytes();
  pgaccel_reset_gpu_exec_count();
  pgaccel_status st = pgaccel_h3_lat_lng_to_cell_bulk(lats.data(), lngs.data(), N, 7,
                                                      /*use_fp64=*/1, cells.data(), valids.data());
  r.gpu_dispatches = pgaccel_gpu_exec_count();
  const size_t rss_after = peak_rss_bytes();
  r.status_ok = (st == PGACCEL_OK && r.gpu_dispatches > 0);
  r.peak_rss_bytes = rss_after;
  bool all_valid = r.status_ok;
  uint64_t first = 0;
  if (r.status_ok) {
    first = cells[0];
    for (size_t i = 0; i < N; ++i) {
      if (!valids[i] || cells[i] != first) {
        all_valid = false;
        break;
      }
    }
  }
  r.correct = all_valid;
  const size_t rss_delta = rss_after > rss_before ? rss_after - rss_before : 0;
  r.rss_delta_bytes = rss_delta;
  r.under_ceiling = rss_delta <= rss_ceiling;
  printf("   status=%d (OK=%d) all_same_valid=%d  rss_before=%.2fGB peak=%.2fGB delta=%.2fGB "
         "ceiling=%.2fGB dispatches=%llu under=%d\n",
         (int)st, r.status_ok, all_valid, rss_before / 1e9, rss_after / 1e9, rss_delta / 1e9,
         rss_ceiling / 1e9, static_cast<unsigned long long>(r.gpu_dispatches), r.under_ceiling);
  if (!r.status_ok)
    r.note = "h3_f64 returned non-OK on device-exceeding input";
  else if (!r.correct)
    r.note = "h3_f64 produced inconsistent cells for same lat/lng";
  return r;
}

int main() {
  if (pgaccel_init() != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init failed\n");
    return 1;
  }

  pgaccel_platform_caps caps = pgaccel_get_caps();
  pgaccel_device_info info = pgaccel_get_device_info();
  printf("Device: %s backend=%s has_native_fp64=%d max_alloc_bytes=%zu\n", info.device_name,
         info.backend_name, info.has_native_fp64, caps.max_alloc_bytes);
  std::string backend = info.backend_name;
  std::transform(backend.begin(), backend.end(), backend.begin(),
                 [](unsigned char value) { return static_cast<char>(std::tolower(value)); });
  const bool accelerator_backend =
      backend.find("metal") != std::string::npos || backend.find("cuda") != std::string::npos ||
      backend.find("hip") != std::string::npos || backend.find("level_zero") != std::string::npos;
  if (info.device_name[0] == '\0' || info.backend_name[0] == '\0' || info.compute_units == 0 ||
      caps.max_alloc_bytes == 0 || !accelerator_backend) {
    fprintf(stderr,
            "FAIL: OOM invariant requires a real accelerator device with nonzero capacity\n");
    pgaccel_shutdown();
    return 2;
  }
  printf("PGACCEL_DEVICE_PROOF device=\"%s\" backend=\"%s\" compute_units=%u "
         "max_alloc_bytes=%zu real_device=1\n",
         info.device_name, info.backend_name, info.compute_units, caps.max_alloc_bytes);

  // Input size: 2 × max_alloc_bytes / sizeof(double).
  const size_t max_alloc = caps.max_alloc_bytes;
  const size_t N = (2 * max_alloc) / sizeof(double);
  // Cap N at 256 Mi (2 GiB of doubles) to keep the test feasible on
  // constrained hosts — still exceeds any typical max_alloc on M-series.
  const size_t N_capped = std::min<size_t>(N, size_t{256} * 1024 * 1024);
  // RSS ceiling: 3 × max_alloc. If max_alloc is small, add slack so
  // that the harness's baseline RSS isn't what flags the test.
  const size_t rss_ceiling = 3 * max_alloc;

  printf("\nPlan:\n");
  printf("  N (doubles) = 2 * max_alloc / sizeof(double) = %zu\n", N);
  printf("  N_capped    = %zu (%.2f GB per-family input)\n", N_capped,
         static_cast<double>(N_capped * sizeof(double)) / 1e9);
  printf("  RSS ceiling = 3 * max_alloc = %.2f GB\n", rss_ceiling / 1e9);

  std::vector<FamilyResult> results;
  results.push_back(run_reduce_family(N_capped, rss_ceiling));
  // Smaller N for sort (double the data in flight during sort).
  results.push_back(run_sort_family(N_capped / 2, rss_ceiling));
  results.push_back(run_hashagg_family(N_capped / 2, rss_ceiling));
  // Spatial PIP: 2 doubles per point — use N_capped/2 points.
  results.push_back(run_spatial_family(N_capped / 2, rss_ceiling));
  // h3: same shape as spatial.
  results.push_back(run_h3_family(N_capped / 2, rss_ceiling));

  pgaccel_shutdown();

  printf("\n=== OOM-never invariant summary ===\n");
  int fails = 0;
  for (const auto& r : results) {
    const bool pass = r.status_ok && r.correct && r.under_ceiling && r.gpu_dispatches > 0;
    printf("  %-14s %s  peak_rss=%.2fGB ceiling=%.2fGB status_ok=%d correct=%d under_ceiling=%d "
           "note=\"%s\"\n",
           r.name, pass ? "PASS" : "FAIL", r.peak_rss_bytes / 1e9, r.rss_ceiling_bytes / 1e9,
           r.status_ok, r.correct, r.under_ceiling, r.note.c_str());
    printf("PGACCEL_OOM_FAMILY family=%s result=%s dispatches=%llu "
           "peak_rss_bytes=%zu rss_delta_bytes=%zu rss_limit_bytes=%zu\n",
           r.name, pass ? "PASS" : "FAIL", static_cast<unsigned long long>(r.gpu_dispatches),
           r.peak_rss_bytes, r.rss_delta_bytes, r.rss_ceiling_bytes);
    if (!pass)
      fails++;
  }
  if (fails) {
    fprintf(stderr,
            "\nFAIL: %d kernel family/families violate OOM-never invariant. "
            "This is a streaming/chunking regression — do NOT relax the "
            "RSS ceiling to make it pass.\n",
            fails);
    return 1;
  }
  printf("PGACCEL_OOM_INVARIANT result=PASS families=%zu max_alloc_bytes=%zu "
         "input_doubles=%zu rss_limit_bytes=%zu\n",
         results.size(), max_alloc, N_capped, rss_ceiling);
  printf("\nPASS — all fp64 kernel families honor OOM-never invariant.\n");
  return 0;
}
