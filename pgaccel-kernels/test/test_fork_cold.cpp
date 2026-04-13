// test_fork_cold: Tests whether Metal GPU can initialize in a forked child
// when the PARENT never touched Metal/SYCL.
//
// This simulates the PostgreSQL scenario: postmaster loads the extension
// (_PG_init) but never calls pgaccel_init(). Then it forks a backend.
// Can that backend initialize Metal from scratch?
//
// If YES: we can eliminate the entire BGW IPC layer and run GPU kernels
// directly inside PG backends/parallel workers.
// If NO: fork+exec remains necessary for Metal on macOS.

#include "pgaccel_ffi.h"
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <sys/wait.h>
#include <unistd.h>

static const size_t N = 10000;
static const float VAL = 7.0f;
static const float EXPECTED_SUM = 70000.0f;
static const float TOLERANCE = 1.0f;

static bool approx_eq(float a, float b, float tol) {
    return std::fabs(a - b) < tol;
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

        // Check if we got a real GPU or CPU fallback
        pgaccel_device_info info = pgaccel_get_device_info();
        printf("Child: device=%s backend=%s compute_units=%u unified=%d fp64=%d\n",
               info.device_name, info.backend_name, info.compute_units,
               info.is_unified_memory, info.has_fp64);

        if (info.compute_units == 0) {
            fprintf(stderr, "\nRESULT: FAIL — Got CPU fallback, not real GPU.\n");
            fprintf(stderr, "Cold Metal init after fork does NOT work.\n");
            _exit(2);
        }

        // Reset counters
        pgaccel_reset_gpu_exec_count();
        pgaccel_reset_cpu_fallback_count();

        // Run actual GPU kernels
        float* data = (float*)malloc(N * sizeof(float));
        for (size_t i = 0; i < N; i++) data[i] = VAL;

        float sum = 0.0f;
        st = pgaccel_reduce_sum_f32(data, N, &sum);
        if (st != PGACCEL_OK) {
            fprintf(stderr, "Child: reduce_sum_f32 failed: status=%d\n", st);
            _exit(3);
        }
        if (!approx_eq(sum, EXPECTED_SUM, TOLERANCE)) {
            fprintf(stderr, "Child: reduce_sum_f32 WRONG: got %f, expected %f\n",
                    sum, EXPECTED_SUM);
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
        printf("Child: sort_kv_f32 OK (first 3 keys: %f %f %f)\n",
               keys[0], keys[1], keys[2]);

        uint64_t gpu_count = pgaccel_gpu_exec_count();
        uint64_t fb_count = pgaccel_cpu_fallback_count();
        printf("\nChild: gpu_exec=%llu cpu_fallback=%llu\n",
               (unsigned long long)gpu_count, (unsigned long long)fb_count);

        if (gpu_count == 0) {
            fprintf(stderr, "\nRESULT: FAIL — GPU exec count is 0. "
                    "Kernels ran on CPU, not GPU.\n");
            _exit(7);
        }
        if (fb_count > 0) {
            fprintf(stderr, "\nRESULT: FAIL — %llu CPU fallback(s). "
                    "Some kernels fell back to CPU.\n",
                    (unsigned long long)fb_count);
            _exit(8);
        }

        free(data);
        printf("\n=== RESULT: SUCCESS ===\n");
        printf("Cold Metal init in forked child WORKS!\n");
        printf("GPU kernels execute on real GPU hardware after fork.\n");
        printf("The entire BGW IPC layer can be eliminated.\n");
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
                printf("→ IPC layer can be eliminated.\n");
                break;
            case 1:
                printf("FAIL: pgaccel_init() failed in forked child.\n");
                break;
            case 2:
                printf("FAIL: Got CPU fallback — Metal did not initialize.\n");
                break;
            case 3: case 4: case 5: case 6:
                printf("FAIL: GPU kernel failed or produced wrong results.\n");
                break;
            case 7:
                printf("FAIL: GPU exec count was 0 — kernels ran on CPU.\n");
                break;
            case 8:
                printf("FAIL: CPU fallbacks occurred.\n");
                break;
            default:
                printf("FAIL: Child exited with code %d\n", rc);
        }
        return rc;
    } else if (WIFSIGNALED(wstatus)) {
        int sig = WTERMSIG(wstatus);
        printf("CRASH: Child killed by signal %d — Metal init or kernel "
               "crashed after fork.\n", sig);
        return 1;
    }

    return 1;
}
