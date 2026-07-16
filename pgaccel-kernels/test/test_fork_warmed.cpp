// test_fork_warmed: verifies the PostgreSQL postmaster-to-backend flow. The
// parent establishes the pre-fork process environment, then the child creates
// the Metal device and runs representative dispatches.

#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>

#include "pgaccel_ffi.h"

static const size_t N = 100000;
#if defined(__APPLE__)
static const int FORK_CYCLES = 8;
#else
static const int FORK_CYCLES = 1;
#endif

int main() {
  printf("=== Fork Policy Test (PG postmaster→backend flow) ===\n");
  printf("Parent PID: %d\n\n", getpid());

  // Like _PG_init() in the postmaster.
  printf("Parent: calling pgaccel_prefork_warmup()...\n");
#if defined(__APPLE__)
  unsetenv("OS_ACTIVITY_MODE");
#endif
  pgaccel_prefork_warmup();
#if defined(__APPLE__)
  const char* activity_mode = getenv("OS_ACTIVITY_MODE");
  if (activity_mode == nullptr || std::strcmp(activity_mode, "disable") != 0) {
    fprintf(stderr, "Parent: OS_ACTIVITY_MODE default was not installed\n");
    return 1;
  }

  setenv("OS_ACTIVITY_MODE", "owner-override", 1);
  pgaccel_prefork_warmup();
  activity_mode = getenv("OS_ACTIVITY_MODE");
  if (activity_mode == nullptr || std::strcmp(activity_mode, "owner-override") != 0) {
    fprintf(stderr, "Parent: OS_ACTIVITY_MODE owner override was replaced\n");
    return 1;
  }

  // The synthetic override only tests ownership semantics. Restore the safe
  // default before creating the child that initializes Metal.
  setenv("OS_ACTIVITY_MODE", "disable", 1);
#endif
  printf("Parent: pre-fork policy ready, forking...\n\n");

  auto run_fork_cycle = []() -> int {
    pid_t pid = fork();
    if (pid < 0) {
      perror("fork");
      return 1;
    }

    if (pid == 0) {
      printf("Child PID: %d (parent was %d)\n", getpid(), getppid());

      // Like a PG backend's first query.
      pgaccel_status st = pgaccel_init();
      if (st != PGACCEL_OK) {
        fprintf(stderr, "Child pgaccel_init failed: %d\n", st);
        _exit(1);
      }

      pgaccel_device_info info = pgaccel_get_device_info();
      printf("Child: pgaccel_init OK — device=%s backend=%s\n", info.device_name,
             info.backend_name);

      // Exercise queue::memcpy and a representative Metal command-buffer
      // completion path via a reduce kernel on 100K floats.
      float* data = new float[N];
      for (size_t i = 0; i < N; i++)
        data[i] = 1.0f;

      float sum = 0.0f;
      st = pgaccel_reduce_sum_f32(data, N, &sum);
      if (st != PGACCEL_OK) {
        fprintf(stderr, "Child reduce_sum_f32 failed: %d\n", st);
        _exit(2);
      }

      if (std::fabs(sum - (float)N) > 0.5f) {
        fprintf(stderr, "Child reduce_sum_f32 WRONG: got %f, expected %zu\n", sum, N);
        _exit(3);
      }
      printf("Child: reduce_sum_f32(%zu floats) = %f OK\n", N, sum);

      // Soft-fp64 on Metal must also run after the pre-fork environment policy.
      // Any UNSUPPORTED/error is a regression.
      double* ddata = new double[N];
      for (size_t i = 0; i < N; i++)
        ddata[i] = 1.0;
      double dsum = 0.0;
      st = pgaccel_reduce_sum_f64(ddata, N, &dsum);
      if (st != PGACCEL_OK) {
        fprintf(stderr, "Child reduce_sum_f64 failed: %d (soft-fp64 post-fork broken?)\n", st);
        _exit(5);
      }
      if (std::fabs(dsum - (double)N) > 1e-6) {
        fprintf(stderr, "Child reduce_sum_f64 WRONG: got %f, expected %zu\n", dsum, N);
        _exit(6);
      }
      printf("Child: reduce_sum_f64(%zu doubles) = %f OK (soft-fp64 post-fork)\n", N, dsum);
      delete[] ddata;

      uint64_t gpu = pgaccel_gpu_exec_count();
      printf("Child: gpu_exec=%llu\n", (unsigned long long)gpu);
      if (gpu == 0) {
        fprintf(stderr, "Child: FAIL — gpu_exec=0 (no post-fork GPU execution)\n");
        _exit(4);
      }

      delete[] data;
      pgaccel_shutdown();
      printf("\nChild: PASS — GPU works after pre-fork policy + fork.\n");
      _exit(0);
    }

    int wstatus = 0;
    pid_t waited;
    do {
      waited = waitpid(pid, &wstatus, 0);
    } while (waited < 0 && errno == EINTR);
    if (waited < 0) {
      perror("waitpid");
      return 1;
    }

    printf("\n=== Results ===\n");
    if (WIFEXITED(wstatus)) {
      int rc = WEXITSTATUS(wstatus);
      if (rc == 0) {
        printf("PASS: pre-fork policy + child GPU dispatch OK.\n");
        return 0;
      }
      printf("FAIL: child exit code %d\n", rc);
      return rc;
    }
    if (WIFSIGNALED(wstatus)) {
      int sig = WTERMSIG(wstatus);
      printf("FAIL: child killed by signal %d (pre-fork policy did not prevent crash)\n", sig);
      return 1;
    }
    return 1;
  };

  for (int cycle = 1; cycle <= FORK_CYCLES; ++cycle) {
    printf("\n=== Fork cycle %d/%d ===\n", cycle, FORK_CYCLES);
    int rc = run_fork_cycle();
    if (rc != 0)
      return rc;
  }

  printf("\nPASS: all %d pre-fork policy GPU cycles completed.\n", FORK_CYCLES);
  return 0;
}
