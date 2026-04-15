// test_fork: verifies that after fork(), GPU kernels do NOT crash and
// instead return PGACCEL_ERROR_NO_DEVICE cleanly.
//
// Background: Metal/SYCL state is unusable in a forked child process
// (MTLCreateSystemDefaultDevice reuses stale IOKit Mach ports whose GPU
// memory allocator is broken after fork). pg_accel's production path
// dispatches GPU work directly via Metal API from the query backend.
// This test pins that contract: forked-backend code paths must detect
// the fork and return a clean error, never crash.

#include "pgaccel_ffi.h"
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <sys/wait.h>
#include <unistd.h>

static const size_t N = 1000;
static const float VAL = 25.0f;
static const float EXPECTED_SUM = 25000.0f;
static const float TOLERANCE = 0.01f;

static bool approx_eq(float a, float b, float tol) {
    return std::fabs(a - b) < tol;
}

// Runs reduce kernels and expects SUCCESS (parent, pre-fork).
static int run_reduce_tests_parent() {
    float data[N];
    for (size_t i = 0; i < N; i++) data[i] = VAL;

    float sum = 0.0f;
    pgaccel_status st = pgaccel_reduce_sum_f32(data, N, &sum);
    if (st != PGACCEL_OK) {
        fprintf(stderr, "[parent] reduce_sum_f32 failed: status=%d\n", st);
        return 1;
    }
    if (!approx_eq(sum, EXPECTED_SUM, TOLERANCE)) {
        fprintf(stderr, "[parent] reduce_sum_f32 WRONG: got %f, expected %f\n",
                sum, EXPECTED_SUM);
        return 1;
    }
    printf("[parent] reduce_sum_f32: %f OK\n", sum);

    float min_val = 0.0f;
    st = pgaccel_reduce_min_f32(data, N, &min_val);
    if (st != PGACCEL_OK || !approx_eq(min_val, VAL, TOLERANCE)) {
        fprintf(stderr, "[parent] reduce_min_f32 failed: status=%d val=%f\n",
                st, min_val);
        return 1;
    }
    printf("[parent] reduce_min_f32: %f OK\n", min_val);

    float max_val = 0.0f;
    st = pgaccel_reduce_max_f32(data, N, &max_val);
    if (st != PGACCEL_OK || !approx_eq(max_val, VAL, TOLERANCE)) {
        fprintf(stderr, "[parent] reduce_max_f32 failed: status=%d val=%f\n",
                st, max_val);
        return 1;
    }
    printf("[parent] reduce_max_f32: %f OK\n", max_val);

    return 0;
}

// Runs reduce kernels in the child post-fork. Expects clean NO_DEVICE
// returns — NOT crashes, NOT silent CPU fallback success.
static int run_reduce_tests_child() {
    float data[N];
    for (size_t i = 0; i < N; i++) data[i] = VAL;

    float sum = 0.0f;
    pgaccel_status st = pgaccel_reduce_sum_f32(data, N, &sum);
    if (st != PGACCEL_ERROR_NO_DEVICE) {
        fprintf(stderr,
            "[child] reduce_sum_f32 expected PGACCEL_ERROR_NO_DEVICE (%d), "
            "got %d — fork detection is broken\n",
            PGACCEL_ERROR_NO_DEVICE, st);
        return 1;
    }
    printf("[child] reduce_sum_f32: PGACCEL_ERROR_NO_DEVICE (expected) OK\n");

    st = pgaccel_reduce_min_f32(data, N, &sum);
    if (st != PGACCEL_ERROR_NO_DEVICE) {
        fprintf(stderr, "[child] reduce_min_f32 expected NO_DEVICE, got %d\n",
                st);
        return 1;
    }
    printf("[child] reduce_min_f32: PGACCEL_ERROR_NO_DEVICE (expected) OK\n");

    st = pgaccel_reduce_max_f32(data, N, &sum);
    if (st != PGACCEL_ERROR_NO_DEVICE) {
        fprintf(stderr, "[child] reduce_max_f32 expected NO_DEVICE, got %d\n",
                st);
        return 1;
    }
    printf("[child] reduce_max_f32: PGACCEL_ERROR_NO_DEVICE (expected) OK\n");

    return 0;
}

int main() {
    printf("=== Fork GPU Test (contract: fork → NO_DEVICE, no crash) ===\n");
    printf("Parent PID: %d\n\n", getpid());

    // Step 1: Init in parent (real GPU)
    pgaccel_status st = pgaccel_init();
    if (st != PGACCEL_OK) {
        fprintf(stderr, "Parent pgaccel_init failed: %d\n", st);
        return 1;
    }
    printf("Parent: pgaccel_init OK\n");

    // Step 2: Verify kernels work pre-fork
    if (run_reduce_tests_parent() != 0) return 1;

    uint64_t parent_gpu = pgaccel_gpu_exec_count();
    uint64_t parent_fb = pgaccel_cpu_fallback_count();
    printf("[parent] gpu_exec=%llu cpu_fallback=%llu\n",
           (unsigned long long)parent_gpu, (unsigned long long)parent_fb);
    if (parent_gpu == 0) {
        fprintf(stderr, "Parent: FAIL — GPU exec count is 0; GPU not running "
                        "in parent\n");
        return 1;
    }
    if (parent_fb > 0) {
        fprintf(stderr, "Parent: FAIL — %llu CPU fallback(s) in parent\n",
                (unsigned long long)parent_fb);
        return 1;
    }
    printf("\n");

    // Step 3: Fork
    printf("Parent: forking...\n");
    pid_t pid = fork();
    if (pid < 0) {
        perror("fork");
        return 1;
    }

    if (pid == 0) {
        // ── Child ──
        printf("\nChild PID: %d (parent was %d)\n", getpid(), getppid());

        // Re-init in child: expected to detect fork and mark GPU unavailable.
        // init itself should succeed (returns OK with g_queue=nullptr).
        st = pgaccel_init();
        if (st != PGACCEL_OK) {
            fprintf(stderr, "Child pgaccel_init failed: %d\n", st);
            _exit(1);
        }
        printf("Child: pgaccel_init OK (fork detected, GPU disabled)\n");

        // Reset counters — we'll assert nothing runs on GPU post-fork.
        pgaccel_reset_gpu_exec_count();
        pgaccel_reset_cpu_fallback_count();

        // Kernels must return NO_DEVICE, not crash, not silently succeed.
        if (run_reduce_tests_child() != 0) {
            _exit(1);
        }

        uint64_t child_gpu = pgaccel_gpu_exec_count();
        printf("\nChild: gpu_exec_count=%llu (expect 0)\n",
               (unsigned long long)child_gpu);
        if (child_gpu != 0) {
            fprintf(stderr,
                "Child: FAIL — GPU ran in forked process (count=%llu). "
                "This means fork detection broke and kernels executed on "
                "a stale Metal context.\n",
                (unsigned long long)child_gpu);
            _exit(1);
        }

        printf("\nChild: PASS — fork detected, kernels returned NO_DEVICE "
               "cleanly (no crash).\n");
        _exit(0);
    }

    // ── Parent waits ──
    int wstatus = 0;
    waitpid(pid, &wstatus, 0);

    printf("\n=== Results ===\n");
    if (WIFEXITED(wstatus)) {
        int child_rc = WEXITSTATUS(wstatus);
        if (child_rc == 0) {
            printf("PASS: Child exited cleanly. Fork contract upheld: "
                   "GPU kernels return NO_DEVICE in forked backend.\n");
        } else {
            printf("FAIL: Child (PID %d) exited with code %d\n", pid, child_rc);
        }
        pgaccel_shutdown();
        return child_rc;
    } else if (WIFSIGNALED(wstatus)) {
        int sig = WTERMSIG(wstatus);
        printf("FAIL: Child (PID %d) killed by signal %d — GPU runtime "
               "crashed after fork (NO_DEVICE contract broken).\n", pid, sig);
        pgaccel_shutdown();
        return 1;
    }

    pgaccel_shutdown();
    return 1;
}
