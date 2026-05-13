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

// Cold-dispatch of every fp64 kernel family from a freshly-forked
// backend. Post fp64-unlock (W1/W2/W3/W4), soft-fp64 on Metal must
// survive fork. Any UNSUPPORTED/error is a FAIL — kernel must execute.
// Keeps each dispatch small (256 elements) — the fork-safety property
// is "does it run at all", not throughput.
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

  // ── hashagg_f64 (fp64 sum over fp64 values, i64 keys) ────────────
  {
    constexpr size_t HN = 64;
    int64_t keys[HN];
    uint8_t key_nulls[HN] = {};
    double vals[HN];
    uint8_t val_nulls[HN] = {};
    for (size_t i = 0; i < HN; ++i) {
      keys[i] = static_cast<int64_t>(i % 4);  // 4 groups
      vals[i] = static_cast<double>(i) + 0.5;
    }
    const void* val_arrays[1] = {vals};
    const uint8_t* val_null_arrays[1] = {val_nulls};
    int val_types[1] = {PGACCEL_VAL_FLOAT64};
    pgaccel_agg_col agg_cols[1] = {{PGACCEL_AGG_SUM, 0}};
    pgaccel_agg_state* state =
        pgaccel_hash_agg_execute(keys, key_nulls, HN, PGACCEL_KEY_INT64, val_arrays,
                                 val_null_arrays, val_types, agg_cols, 1);
    if (!state) {
      fprintf(stderr, "[%s] fp64 matrix: hashagg_f64 returned NULL\n", label);
      return 1;
    }
    size_t ngroups = pgaccel_agg_group_count(state);
    if (ngroups != 4) {
      fprintf(stderr, "[%s] fp64 matrix: hashagg_f64 ngroups=%zu (expected 4)\n", label, ngroups);
      pgaccel_agg_free(state);
      return 1;
    }
    const double* results = pgaccel_agg_get_results(state, 0);
    // Sum check: total should equal sum of vals[0..63]
    double total = 0.0;
    for (size_t i = 0; i < ngroups; ++i)
      total += results[i];
    double ref_total = 0.0;
    for (size_t i = 0; i < HN; ++i)
      ref_total += vals[i];
    if (std::fabs(total - ref_total) > 1e-9) {
      fprintf(stderr, "[%s] fp64 matrix: hashagg_f64 total %.9g != %.9g\n", label, total,
              ref_total);
      pgaccel_agg_free(state);
      return 1;
    }
    pgaccel_agg_free(state);
    printf("[%s] fp64 matrix: hashagg_f64 OK\n", label);
  }

  // ── bbox_f64 (fp64 bbox-intersects-bulk) ─────────────────────────
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
