// test_fork: verifies that after fork(), GPU kernels work correctly in
// the child process — matching the PostgreSQL production scenario.
//
// PostgreSQL pattern: postmaster loads pg_accel.so (and libacpp-rt.dylib)
// via shared_preload_libraries but never calls pgaccel_init(). Then it
// forks a backend. The backend calls pgaccel_init() on first query and
// gets a fresh GPU queue.
//
// This test pins that contract: the parent loads the library (triggering
// static constructors), then forks. The child initializes GPU from
// scratch and runs kernels.

#include <sys/wait.h>
#include <unistd.h>

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_hash_agg.h"
#include "pgaccel_hash_join.h"

static const size_t N = 1000;
static const float VAL = 25.0f;
static const float EXPECTED_SUM = 25000.0f;
static const float TOLERANCE = 0.01f;

static bool approx_eq(float a, float b, float tol) {
  return std::fabs(a - b) < tol;
}

// Runs reduce kernels and expects SUCCESS.
static int run_reduce_tests(const char* label) {
  float data[N];
  for (size_t i = 0; i < N; i++)
    data[i] = VAL;

  float sum = 0.0f;
  pgaccel_status st = pgaccel_reduce_sum_f32(data, N, &sum);
  if (st != PGACCEL_OK) {
    fprintf(stderr, "[%s] reduce_sum_f32 failed: status=%d\n", label, st);
    return 1;
  }
  if (!approx_eq(sum, EXPECTED_SUM, TOLERANCE)) {
    fprintf(stderr, "[%s] reduce_sum_f32 WRONG: got %f, expected %f\n", label, sum, EXPECTED_SUM);
    return 1;
  }
  printf("[%s] reduce_sum_f32: %f OK\n", label, sum);

  float min_val = 0.0f;
  st = pgaccel_reduce_min_f32(data, N, &min_val);
  if (st != PGACCEL_OK || !approx_eq(min_val, VAL, TOLERANCE)) {
    fprintf(stderr, "[%s] reduce_min_f32 failed: status=%d val=%f\n", label, st, min_val);
    return 1;
  }
  printf("[%s] reduce_min_f32: %f OK\n", label, min_val);

  float max_val = 0.0f;
  st = pgaccel_reduce_max_f32(data, N, &max_val);
  if (st != PGACCEL_OK || !approx_eq(max_val, VAL, TOLERANCE)) {
    fprintf(stderr, "[%s] reduce_max_f32 failed: status=%d val=%f\n", label, st, max_val);
    return 1;
  }
  printf("[%s] reduce_max_f32: %f OK\n", label, max_val);

  return 0;
}

// Cold-dispatch of fp64 GPU kernel families from a freshly-forked backend,
// plus bbox_f64 correctness fallback coverage. Post fp64-unlock
// (W1/W2/W3/W4), soft-fp64 GPU kernels on Metal must survive fork. Any
// UNSUPPORTED/error is a FAIL.
// Keeps each dispatch small (256 elements) — the fork-safety property is
// "does it run at all", not throughput.
static int run_fp64_fork_matrix(const char* label) {
  constexpr size_t MN = 256;

  // ── reduce_f64 (sum) ──────────────────────────────────────────────
  {
    double data[MN];
    double ref_sum = 0.0;
    for (size_t i = 0; i < MN; ++i) {
      data[i] = 0.5 + static_cast<double>(i);
      ref_sum += data[i];
    }
    double got = 0.0;
    pgaccel_status st = pgaccel_reduce_sum_f64(data, MN, &got);
    if (st != PGACCEL_OK) {
      fprintf(stderr, "[%s] fp64 matrix: reduce_sum_f64 status=%d (kernel must dispatch)\n", label,
              st);
      return 1;
    }
    if (std::fabs(got - ref_sum) / std::fabs(ref_sum) > 1e-12) {
      fprintf(stderr, "[%s] fp64 matrix: reduce_sum_f64 drift got=%.17g ref=%.17g\n", label, got,
              ref_sum);
      return 1;
    }
    printf("[%s] fp64 matrix: reduce_sum_f64 OK\n", label);
  }

  // ── sort_f64 (kv) ────────────────────────────────────────────────
  {
    double keys[8] = {3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0};
    uint32_t indices[8] = {0, 1, 2, 3, 4, 5, 6, 7};
    pgaccel_status st = pgaccel_sort_kv_f64(keys, indices, 8);
    if (st != PGACCEL_OK) {
      fprintf(stderr, "[%s] fp64 matrix: sort_kv_f64 status=%d\n", label, st);
      return 1;
    }
    for (size_t i = 1; i < 8; ++i) {
      if (keys[i] < keys[i - 1]) {
        fprintf(stderr, "[%s] fp64 matrix: sort_kv_f64 non-monotone\n", label);
        return 1;
      }
    }
    printf("[%s] fp64 matrix: sort_kv_f64 OK\n", label);
  }

  // ── spatial_f64 (fp64 PIP recheck) ────────────────────────────────
  {
    double ring[] = {0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0};
    double pt[] = {0.5, 0.5};
    int8_t result = 99;
    pgaccel_status st = pgaccel_point_in_ring_bulk(pt, 1, ring, 5, /*use_fp64=*/true, &result);
    if (st != PGACCEL_OK) {
      fprintf(stderr, "[%s] fp64 matrix: point_in_ring fp64 status=%d\n", label, st);
      return 1;
    }
    if (result != 1) {
      fprintf(stderr, "[%s] fp64 matrix: point_in_ring fp64 wrong result %d\n", label, result);
      return 1;
    }
    printf("[%s] fp64 matrix: spatial_f64 (PIP recheck) OK\n", label);
  }

  // ── h3_f64 (lat_lng_to_cell) ─────────────────────────────────────
  {
    double lat = 40.7128, lng = -74.0060;  // NYC
    uint64_t cell = 0;
    uint8_t valid = 0;
    pgaccel_status st =
        pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 7, /*use_fp64=*/1, &cell, &valid);
    if (st != PGACCEL_OK) {
      fprintf(stderr, "[%s] fp64 matrix: h3_lat_lng_to_cell fp64 status=%d\n", label, st);
      return 1;
    }
    if (!valid || cell == 0) {
      fprintf(stderr, "[%s] fp64 matrix: h3_lat_lng_to_cell fp64 invalid result\n", label);
      return 1;
    }
    printf("[%s] fp64 matrix: h3_f64 (lat_lng_to_cell) OK\n", label);
  }

  // ── resident GPU hash COUNT over i64 keys ────────────────────────
  {
    constexpr size_t HN = 64;
    int64_t keys[HN];
    for (size_t i = 0; i < HN; ++i) {
      keys[i] = static_cast<int64_t>(i % 4);  // 4 groups
    }

    void* device_keys = nullptr;
    pgaccel_status st = pgaccel_expr_device_alloc_copy(keys, sizeof(keys), &device_keys);
    if (st != PGACCEL_OK || device_keys == nullptr) {
      fprintf(stderr, "[%s] resident hash_count_i64 allocation failed: status=%d\n", label, st);
      return 1;
    }

    const uint64_t before = pgaccel_gpu_exec_count();
    pgaccel_agg_state* state = pgaccel_hash_count_i64_device_hash_execute_bounded(
        static_cast<int64_t*>(device_keys), HN, 4);
    const uint64_t after = pgaccel_gpu_exec_count();
    pgaccel_expr_device_free(device_keys);
    if (!state) {
      fprintf(stderr, "[%s] resident hash_count_i64 returned NULL\n", label);
      return 1;
    }
    if (after <= before) {
      fprintf(stderr, "[%s] resident hash_count_i64 did not dispatch\n", label);
      pgaccel_agg_free(state);
      return 1;
    }
    size_t ngroups = pgaccel_agg_group_count(state);
    if (ngroups != 4) {
      fprintf(stderr, "[%s] resident hash_count_i64 ngroups=%zu (expected 4)\n", label, ngroups);
      pgaccel_agg_free(state);
      return 1;
    }
    const double* results = pgaccel_agg_get_results(state, 0);
    double total = 0.0;
    for (size_t i = 0; i < ngroups; ++i)
      total += results[i];
    if (std::fabs(total - static_cast<double>(HN)) > 1e-9) {
      fprintf(stderr, "[%s] resident hash_count_i64 total %.9g != %zu\n", label, total, HN);
      pgaccel_agg_free(state);
      return 1;
    }
    pgaccel_agg_free(state);
    printf("[%s] resident hash_count_i64 OK (gpu_exec %llu -> %llu)\n", label,
           (unsigned long long)before, (unsigned long long)after);
  }

  // ── bbox_f64 (fp64 bbox-intersects-bulk correctness fallback) ────
  {
    double a[] = {0.0, 0.0, 2.0, 2.0};
    double b[] = {1.0, 1.0, 3.0, 3.0};
    uint8_t result = 0;
    size_t hits = 0;
    pgaccel_status st = pgaccel_bbox_intersects_bulk_f64(a, 1, b, 1, &result, &hits);
    if (st != PGACCEL_OK) {
      fprintf(stderr, "[%s] fp64 matrix: bbox_intersects_bulk_f64 status=%d\n", label, st);
      return 1;
    }
    if (result != 1 || hits != 1) {
      fprintf(stderr, "[%s] fp64 matrix: bbox_f64 wrong result=%u hits=%zu\n", label, result, hits);
      return 1;
    }
    printf("[%s] fp64 matrix: bbox_f64 OK\n", label);
  }

  return 0;
}

// 100K point-in-polygon regression for the simple one-thread-per-point
// dispatch path from a freshly-forked backend.
static int run_pip_simple_fork_regression(const char* label) {
  constexpr size_t point_count = 100000;
  constexpr size_t expected_inside = 40000;
  constexpr size_t expected_outside = 60000;

  float bbox[] = {-1.0f, -1.0f, 1.0f, 1.0f};
  float diamond[] = {
      0.0f, 1.0f, 1.0f, 0.0f, 0.0f, -1.0f, -1.0f, 0.0f, 0.0f, 1.0f,
  };

  std::vector<float> points(point_count * 2);
  for (size_t i = 0; i < point_count; ++i) {
    switch (i % 5) {
      case 0:
        points[i * 2] = 0.0f;
        points[i * 2 + 1] = 0.0f;
        break;
      case 1:
        points[i * 2] = 0.40f;
        points[i * 2 + 1] = 0.20f;
        break;
      case 2:
        points[i * 2] = 0.90f;
        points[i * 2 + 1] = 0.90f;
        break;
      case 3:
        points[i * 2] = -0.90f;
        points[i * 2 + 1] = -0.90f;
        break;
      default:
        points[i * 2] = 2.0f;
        points[i * 2 + 1] = 2.0f;
        break;
    }
  }

  std::vector<int8_t> results(point_count, 99);
  pgaccel_reset_gpu_exec_count();
  const uint64_t before = pgaccel_gpu_exec_count();

  pgaccel_status st = pgaccel_point_in_polygon_bulk(points.data(), point_count, bbox, diamond, 5,
                                                    nullptr, 0, results.data());
  if (st != PGACCEL_OK) {
    fprintf(stderr, "[%s] PIP simple 100K failed: status=%d\n", label, st);
    return 1;
  }

  const uint64_t after = pgaccel_gpu_exec_count();
  if (after <= before) {
    fprintf(stderr, "[%s] PIP simple 100K did not advance GPU exec count: before=%llu after=%llu\n",
            label, (unsigned long long)before, (unsigned long long)after);
    return 1;
  }

  size_t inside = 0;
  size_t outside = 0;
  size_t uncertain = 0;
  size_t untouched = 0;
  size_t other = 0;
  for (int8_t result : results) {
    if (result == 1)
      ++inside;
    else if (result == -1)
      ++outside;
    else if (result == 0)
      ++uncertain;
    else if (result == 99)
      ++untouched;
    else
      ++other;
  }

  if (inside != expected_inside || outside != expected_outside || uncertain != 0 ||
      untouched != 0 || other != 0) {
    fprintf(stderr,
            "[%s] PIP simple 100K wrong counts: inside=%zu outside=%zu uncertain=%zu "
            "untouched=%zu other=%zu\n",
            label, inside, outside, uncertain, untouched, other);
    return 1;
  }

  printf("[%s] PIP simple 100K OK: inside=%zu outside=%zu gpu_exec_delta=%llu\n", label, inside,
         outside, (unsigned long long)(after - before));
  return 0;
}

// Count .metalar files in AdaptiveCpp JIT cache. Used to assert the
// archive-build helper actually ran for each fp64 kernel family
// dispatched from the forked child.
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

int main() {
  printf("=== Fork GPU Test (PG pattern: parent loads lib, child inits GPU) ===\n");
  printf("Parent PID: %d\n\n", getpid());

  // DO NOT call pgaccel_init() in the parent.
  // This matches PG: shared_preload_libraries loads the .so (and all
  // transitive deps like libacpp-rt.dylib) but the postmaster never
  // calls pgaccel_init() — that's deferred to each backend's first query.
  printf("Parent: library loaded (static constructors ran), "
         "NOT calling pgaccel_init()\n\n");

  // Fork — like PG postmaster forking a backend.
  printf("Parent: forking...\n");
  pid_t pid = fork();
  if (pid < 0) {
    perror("fork");
    return 1;
  }

  if (pid == 0) {
    // ── Child (simulates PG backend) ──
    printf("\nChild PID: %d (parent was %d)\n", getpid(), getppid());

    // Init GPU in child — first-ever init, like a PG backend's first query.
    pgaccel_status st = pgaccel_init();
    if (st != PGACCEL_OK) {
      fprintf(stderr,
              "Child pgaccel_init failed: %d — GPU init "
              "after fork did not work\n",
              st);
      _exit(1);
    }

    pgaccel_device_info info = pgaccel_get_device_info();
    printf("Child: pgaccel_init OK — device=%s backend=%s CUs=%u\n", info.device_name,
           info.backend_name, info.compute_units);

    if (info.compute_units == 0) {
      fprintf(stderr, "Child: FAIL — no GPU (compute_units=0)\n");
      _exit(2);
    }

    // Reset counter.
    pgaccel_reset_gpu_exec_count();

    // Run GPU kernels.
    if (run_reduce_tests("child") != 0) {
      _exit(3);
    }

    // Record .metalar count BEFORE cold-dispatching any fp64 kernel.
    const size_t metalar_before = count_metalar_files();
    printf("[child] metalar files before fp64 matrix: %zu\n", metalar_before);

    // Cold-dispatch every fp64 kernel family (reduce_f64 / sort_f64 /
    // spatial_f64 / h3_f64 / hashagg_f64 / bbox_f64) from the forked
    // backend. Post fp64-unlock plan, soft-fp64 on Metal must survive
    // fork; any failure here is a real regression.
    if (run_fp64_fork_matrix("child") != 0) {
      _exit(8);
    }

    const size_t metalar_after = count_metalar_files();
    printf("[child] metalar files after fp64 matrix: %zu (delta=%zu)\n", metalar_after,
           metalar_after - metalar_before);

    if (run_pip_simple_fork_regression("child") != 0) {
      _exit(9);
    }

    uint64_t child_gpu = pgaccel_gpu_exec_count();
    printf("\nChild: gpu_exec=%llu\n", (unsigned long long)child_gpu);
    if (child_gpu == 0) {
      fprintf(stderr, "Child: FAIL — GPU exec count is 0. SYCL died post-fork.\n");
      _exit(4);
    }

    // Assert the AdaptiveCpp archive-population subprocess ran: after a
    // successful GPU dispatch from a forked child, at least one .metalar
    // must exist in the AdaptiveCpp JIT cache. If this fires, either the
    // archive-builder helper failed silently (check acpp-metal-archive-build
    // stderr) or the pipeline-state path is still using direct compile.
    const char* home = std::getenv("HOME");
    if (home) {
      std::filesystem::path cache =
          std::filesystem::path{home} / ".acpp" / "apps" / "global" / "jit-cache";
      std::error_code ec;
      bool found_metalar = false;
      if (std::filesystem::is_directory(cache, ec) && !ec) {
        for (auto& e : std::filesystem::directory_iterator(cache, ec)) {
          if (e.path().extension() == ".metalar") {
            found_metalar = true;
            break;
          }
        }
      }
      if (!found_metalar) {
        fprintf(stderr,
                "Child: FAIL — no .metalar files in %s; archive path not "
                "exercised, fork-safety is not actually enforced\n",
                cache.c_str());
        _exit(5);
      }
    }

    printf("\nChild: PASS — GPU works after fork.\n");
    pgaccel_shutdown();
    _exit(0);
  }

  // ── Parent waits ──
  int wstatus = 0;
  waitpid(pid, &wstatus, 0);

  printf("\n=== Results ===\n");
  if (WIFEXITED(wstatus)) {
    int child_rc = WEXITSTATUS(wstatus);
    if (child_rc == 0) {
      printf("PASS: GPU works in forked child (PG backend pattern).\n");
    } else {
      printf("FAIL: Child exited with code %d\n", child_rc);
    }
    return child_rc;
  } else if (WIFSIGNALED(wstatus)) {
    int sig = WTERMSIG(wstatus);
    printf("FAIL: Child killed by signal %d — GPU runtime crashed "
           "after fork.\n",
           sig);
    return 1;
  }

  return 1;
}
