// test_fork_cold: Tests whether Metal GPU can initialize in a forked child
// when the PARENT never touched Metal/SYCL.
//
// This simulates the PostgreSQL scenario: postmaster loads the extension
// (_PG_init) but never calls pgaccel_init(). Then it forks a backend.
// Can that backend initialize Metal from scratch?
//
// If YES: Metal binary archives work directly in forked PG backends,
// enabling direct GPU dispatch without fork+exec.
// If NO: fork+exec remains necessary for Metal on macOS.

#include <sys/wait.h>
#include <unistd.h>

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <system_error>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_hash_agg.h"
#include "pgaccel_hash_join.h"

static size_t count_metalar_files() {
  const char* home = std::getenv("HOME");
  if (!home)
    return 0;
  std::filesystem::path cache =
      std::filesystem::path{home} / ".acpp" / "apps" / "global" / "jit-cache";
  std::error_code ec;
  if (!std::filesystem::is_directory(cache, ec) || ec)
    return 0;
  size_t count = 0;
  for (auto& e : std::filesystem::directory_iterator(cache, ec)) {
    if (e.path().extension() == ".metalar")
      ++count;
  }
  return count;
}

static const size_t N = 10000;
static const float VAL = 7.0f;
static const float EXPECTED_SUM = 70000.0f;
static const float TOLERANCE = 1.0f;

static bool approx_eq(float a, float b, float tol) {
  return std::fabs(a - b) < tol;
}

struct PipCounts {
  size_t inside = 0;
  size_t outside = 0;
  size_t uncertain = 0;
  size_t untouched = 0;
  size_t other = 0;
};

static PipCounts count_pip_results(const std::vector<int8_t>& results) {
  PipCounts counts;
  for (int8_t result : results) {
    if (result == 1) {
      counts.inside++;
    } else if (result == -1) {
      counts.outside++;
    } else if (result == 0) {
      counts.uncertain++;
    } else if (result == 99) {
      counts.untouched++;
    } else {
      counts.other++;
    }
  }
  return counts;
}

static void fill_pip_selectivity_points(std::vector<float>& points, size_t inside_count) {
  const size_t point_count = points.size() / 2;
  const size_t outside_count = point_count - inside_count;
  const size_t in_bbox_outside_count = outside_count / 2;

  for (size_t i = 0; i < point_count; ++i) {
    if (i < inside_count) {
      points[i * 2] = (i & 1) ? 0.25f : -0.20f;
      points[i * 2 + 1] = (i & 1) ? 0.10f : -0.15f;
    } else if (i < inside_count + in_bbox_outside_count) {
      points[i * 2] = 0.95f;
      points[i * 2 + 1] = 0.95f;
    } else {
      points[i * 2] = 1.50f;
      points[i * 2 + 1] = 1.50f;
    }
  }
}

static std::vector<float> make_regular_ring(size_t unique_vertices, float radius) {
  constexpr double kPi = 3.14159265358979323846264338327950288;
  std::vector<float> ring((unique_vertices + 1) * 2);
  for (size_t i = 0; i < unique_vertices; ++i) {
    const double angle = 2.0 * kPi * static_cast<double>(i) / static_cast<double>(unique_vertices);
    ring[i * 2] = static_cast<float>(radius * std::cos(angle));
    ring[i * 2 + 1] = static_cast<float>(radius * std::sin(angle));
  }
  ring[unique_vertices * 2] = ring[0];
  ring[unique_vertices * 2 + 1] = ring[1];
  return ring;
}

int main() {
  printf("=== Cold Fork GPU Test ===\n");
  printf("Parent PID: %d\n", getpid());
  printf("Parent does NOT call pgaccel_init() — no Metal state to inherit.\n\n");

  // DO NOT call pgaccel_init() here. This is the whole point of the test.
  // The parent has zero Metal/SYCL state.

  printf("Parent: forking WITHOUT any GPU initialization...\n");
  pid_t pid = fork();
  if (pid < 0) {
    perror("fork");
    return 1;
  }

  if (pid == 0) {
    // ── Child: cold Metal init ──
    printf("\nChild PID: %d (parent was %d)\n", getpid(), getppid());
    printf("Child: calling pgaccel_init() — first-ever GPU init in this "
           "process tree branch...\n");

    pgaccel_status st = pgaccel_init();
    if (st != PGACCEL_OK) {
      fprintf(stderr, "Child: pgaccel_init FAILED: %d\n", st);
      fprintf(stderr, "RESULT: Cold Metal init after fork DOES NOT WORK.\n");
      _exit(1);
    }
    printf("Child: pgaccel_init OK!\n");

    // Check that initialization found real GPU compute units.
    pgaccel_device_info info = pgaccel_get_device_info();
    printf("Child: device=%s backend=%s compute_units=%u fp64=%d\n", info.device_name,
           info.backend_name, info.compute_units, info.has_native_fp64);

    if (info.compute_units == 0) {
      fprintf(stderr, "\nRESULT: FAIL — No GPU compute units detected.\n");
      fprintf(stderr, "Cold Metal init after fork does NOT work.\n");
      _exit(2);
    }

    // Reset counter
    pgaccel_reset_gpu_exec_count();

    // Run actual GPU kernels
    float* data = (float*)malloc(N * sizeof(float));
    for (size_t i = 0; i < N; i++)
      data[i] = VAL;

    float sum = 0.0f;
    st = pgaccel_reduce_sum_f32(data, N, &sum);
    if (st != PGACCEL_OK) {
      fprintf(stderr, "Child: reduce_sum_f32 failed: status=%d\n", st);
      _exit(3);
    }
    if (!approx_eq(sum, EXPECTED_SUM, TOLERANCE)) {
      fprintf(stderr, "Child: reduce_sum_f32 WRONG: got %f, expected %f\n", sum, EXPECTED_SUM);
      _exit(4);
    }
    printf("Child: reduce_sum_f32 = %f (expected %f) OK\n", sum, EXPECTED_SUM);

    float min_val = 0.0f;
    st = pgaccel_reduce_min_f32(data, N, &min_val);
    if (st != PGACCEL_OK) {
      fprintf(stderr, "Child: reduce_min_f32 failed: status=%d\n", st);
      _exit(5);
    }
    printf("Child: reduce_min_f32 = %f OK\n", min_val);

    // Sort test
    float keys[8] = {3.0f, 1.0f, 4.0f, 1.0f, 5.0f, 9.0f, 2.0f, 6.0f};
    uint32_t vals[8] = {0, 1, 2, 3, 4, 5, 6, 7};
    st = pgaccel_sort_kv_f32(keys, vals, 8);
    if (st != PGACCEL_OK) {
      fprintf(stderr, "Child: sort_kv_f32 failed: status=%d\n", st);
      _exit(6);
    }
    printf("Child: sort_kv_f32 OK (first 3 keys: %f %f %f)\n", keys[0], keys[1], keys[2]);

    // ── fp64 cold-dispatch matrix ──────────────────────────────────
    // Post fp64-unlock plan, every fp64 kernel family must cold-dispatch
    // from a fresh forked backend via soft-fp64 on Metal. Any UNSUPPORTED
    // or error status is a real regression in the kernel or its bridge.
    const size_t metalar_before = count_metalar_files();
    printf("Child: metalar count before fp64 matrix: %zu\n", metalar_before);

    // reduce_f64
    {
      double d[128];
      for (size_t i = 0; i < 128; ++i)
        d[i] = 0.5 + static_cast<double>(i);
      double s64 = 0.0;
      st = pgaccel_reduce_sum_f64(d, 128, &s64);
      if (st != PGACCEL_OK) {
        fprintf(stderr, "Child: fp64 reduce_sum_f64 status=%d\n", st);
        _exit(10);
      }
      printf("Child: cold fp64 reduce_sum_f64 = %f OK\n", s64);
    }
    // sort_f64
    {
      double k[8] = {7.5, -1.0, 3.25, 2.0, 9.0, 0.0, -3.5, 4.0};
      uint32_t idx[8] = {0, 1, 2, 3, 4, 5, 6, 7};
      st = pgaccel_sort_kv_f64(k, idx, 8);
      if (st != PGACCEL_OK) {
        fprintf(stderr, "Child: fp64 sort_kv_f64 status=%d\n", st);
        _exit(11);
      }
      printf("Child: cold fp64 sort_kv_f64 OK (first=%f last=%f)\n", k[0], k[7]);
    }
    // spatial_f64 (PIP fp64 recheck)
    {
      double ring[] = {0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0};
      double pt[] = {0.25, 0.75};
      int8_t result = 99;
      st = pgaccel_point_in_ring_bulk(pt, 1, ring, 5, /*use_fp64=*/true, &result);
      if (st != PGACCEL_OK || result != 1) {
        fprintf(stderr, "Child: fp64 spatial status=%d result=%d\n", st, (int)result);
        _exit(12);
      }
      printf("Child: cold fp64 spatial (PIP) OK\n");
    }
    // spatial cooperative PIP cold-fork regression
    {
      constexpr size_t point_count = 100000;
      constexpr size_t unique_vertices = 1024;
      constexpr size_t expected_inside = point_count * 90 / 100;

      std::vector<float> ring = make_regular_ring(unique_vertices, 1.0f);
      float bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
      uint32_t rings[] = {0};

      std::vector<float> points(point_count * 2);
      fill_pip_selectivity_points(points, expected_inside);
      std::vector<int8_t> results(point_count, 99);

      const uint64_t pip_gpu_before = pgaccel_gpu_exec_count();
      st = pgaccel_point_in_polygon_bulk(points.data(), point_count, bbox, ring.data(),
                                         unique_vertices + 1, rings, 1, results.data());
      const uint64_t pip_gpu_after = pgaccel_gpu_exec_count();
      if (st != PGACCEL_OK) {
        fprintf(stderr, "Child: coop PIP 1024v/100K status=%d\n", st);
        _exit(17);
      }
      if (pip_gpu_after <= pip_gpu_before) {
        fprintf(stderr,
                "Child: coop PIP 1024v/100K did not advance gpu_exec count "
                "(before=%llu after=%llu)\n",
                (unsigned long long)pip_gpu_before, (unsigned long long)pip_gpu_after);
        _exit(18);
      }

      const PipCounts counts = count_pip_results(results);
      if (counts.inside + counts.outside + counts.uncertain != point_count ||
          counts.inside != expected_inside || counts.outside != point_count - expected_inside ||
          counts.uncertain != 0 || counts.untouched != 0 || counts.other != 0) {
        fprintf(stderr,
                "Child: coop PIP 1024v/100K wrong counts: inside=%zu outside=%zu "
                "uncertain=%zu untouched=%zu other=%zu\n",
                counts.inside, counts.outside, counts.uncertain, counts.untouched, counts.other);
        _exit(19);
      }

      printf("Child: cold coop PIP 1024v/100K OK (gpu_exec %llu -> %llu)\n",
             (unsigned long long)pip_gpu_before, (unsigned long long)pip_gpu_after);
    }
    // h3_f64
    {
      double lat = 37.7749, lng = -122.4194;
      uint64_t cell = 0;
      uint8_t valid = 0;
      st = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 8, /*use_fp64=*/1, &cell, &valid);
      if (st != PGACCEL_OK || !valid) {
        fprintf(stderr, "Child: fp64 h3 status=%d valid=%d\n", st, (int)valid);
        _exit(13);
      }
      printf("Child: cold fp64 h3_lat_lng_to_cell OK (cell=0x%llx)\n", (unsigned long long)cell);
    }
    // hashagg_f64
    {
      constexpr size_t HN = 32;
      int64_t keys[HN];
      uint8_t knulls[HN] = {};
      double vals[HN];
      uint8_t vnulls[HN] = {};
      for (size_t i = 0; i < HN; ++i) {
        keys[i] = static_cast<int64_t>(i % 4);
        vals[i] = static_cast<double>(i) + 0.25;
      }
      const void* varr[1] = {vals};
      const uint8_t* vnull_arr[1] = {vnulls};
      int vtypes[1] = {PGACCEL_VAL_FLOAT64};
      pgaccel_agg_col ac[1] = {{PGACCEL_AGG_SUM, 0}};
      pgaccel_agg_state* state = pgaccel_hash_agg_execute(keys, knulls, HN, PGACCEL_KEY_INT64, varr,
                                                          vnull_arr, vtypes, ac, 1);
      if (!state) {
        fprintf(stderr, "Child: fp64 hashagg returned NULL\n");
        _exit(14);
      }
      if (pgaccel_agg_group_count(state) != 4) {
        fprintf(stderr, "Child: fp64 hashagg wrong groups=%zu\n", pgaccel_agg_group_count(state));
        pgaccel_agg_free(state);
        _exit(14);
      }
      pgaccel_agg_free(state);
      printf("Child: cold fp64 hashagg_f64 OK\n");
    }
    // bbox_f64
    {
      double a[] = {0.0, 0.0, 2.0, 2.0};
      double b[] = {1.0, 1.0, 3.0, 3.0};
      uint8_t r = 0;
      size_t h = 0;
      st = pgaccel_bbox_intersects_bulk_f64(a, 1, b, 1, &r, &h);
      if (st != PGACCEL_OK || r != 1) {
        fprintf(stderr, "Child: fp64 bbox status=%d result=%u\n", st, r);
        _exit(15);
      }
      printf("Child: cold fp64 bbox OK\n");
    }

    const size_t metalar_after = count_metalar_files();
    printf("Child: metalar count after fp64 matrix: %zu (delta=%zu)\n", metalar_after,
           metalar_after - metalar_before);
    if (metalar_after == 0) {
      fprintf(stderr, "Child: FAIL — no .metalar files found; archive-build path is not "
                      "available for fork-safe Metal dispatch\n");
      _exit(16);
    }

    uint64_t gpu_count = pgaccel_gpu_exec_count();
    printf("\nChild: gpu_exec=%llu\n", (unsigned long long)gpu_count);

    if (gpu_count == 0) {
      fprintf(stderr, "\nRESULT: FAIL — GPU exec count is 0. "
                      "SYCL did not execute kernels post-fork.\n");
      _exit(7);
    }

    free(data);
    printf("\n=== RESULT: SUCCESS ===\n");
    printf("Cold Metal init in forked child WORKS!\n");
    printf("GPU kernels execute on real GPU hardware after fork.\n");
    printf("Metal binary archives work directly in forked PG backends.\n");
    _exit(0);
  }

  // ── Parent waits ──
  int wstatus = 0;
  waitpid(pid, &wstatus, 0);

  printf("\n=== Final Result ===\n");
  if (WIFEXITED(wstatus)) {
    int rc = WEXITSTATUS(wstatus);
    switch (rc) {
      case 0:
        printf("PASS: Cold Metal init after fork WORKS.\n");
        printf("→ Direct Metal dispatch in forked backends confirmed.\n");
        break;
      case 1:
        printf("FAIL: pgaccel_init() failed in forked child.\n");
        break;
      case 2:
        printf("FAIL: No GPU compute units — Metal did not initialize.\n");
        break;
      case 3:
      case 4:
      case 5:
      case 6:
        printf("FAIL: GPU kernel failed or produced wrong results.\n");
        break;
      case 7:
        printf("FAIL: GPU exec count was 0 — SYCL did not dispatch.\n");
        break;
      case 17:
        printf("FAIL: Cold cooperative point-in-polygon dispatch failed.\n");
        break;
      case 18:
        printf("FAIL: Cold cooperative point-in-polygon did not advance GPU exec count.\n");
        break;
      case 19:
        printf("FAIL: Cold cooperative point-in-polygon produced wrong results.\n");
        break;
      default:
        printf("FAIL: Child exited with code %d\n", rc);
    }
    return rc;
  } else if (WIFSIGNALED(wstatus)) {
    int sig = WTERMSIG(wstatus);
    printf("CRASH: Child killed by signal %d — Metal init or kernel "
           "crashed after fork.\n",
           sig);
    return 1;
  }

  return 1;
}
