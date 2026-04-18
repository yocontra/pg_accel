// test_fork_warmed: verifies the PG postmaster→backend flow with pre-fork
// SYCL warmup. Parent calls pgaccel_prefork_warmup() (like _PG_init in the
// postmaster does), then forks. Child calls pgaccel_init() (like a PG
// backend on first query) and runs reduce + a memcpy-heavy spatial kernel.
//
// Goal: prove that initializing SYCL + triggering blit-program JIT in the
// parent lets Apple's AGX driver cache blit variants such that the child's
// fresh device can hydrate them without re-entering MTLCompilerService.

#include "pgaccel_ffi.h"
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <sys/wait.h>
#include <unistd.h>

static const size_t N = 100000;

int main() {
    printf("=== Fork+Warmup Test (PG postmaster→backend flow) ===\n");
    printf("Parent PID: %d\n\n", getpid());

    // Like _PG_init() in the postmaster.
    printf("Parent: calling pgaccel_prefork_warmup()...\n");
    pgaccel_prefork_warmup();
    printf("Parent: warmup done, forking...\n\n");

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
        printf("Child: pgaccel_init OK — device=%s backend=%s\n",
               info.device_name, info.backend_name);

        // Exercise queue::memcpy via a reduce kernel on 100K floats. This
        // goes through pgaccel_alloc_input → queue::memcpy → the exact
        // blit-program JIT path that was crashing.
        float* data = new float[N];
        for (size_t i = 0; i < N; i++) data[i] = 1.0f;

        float sum = 0.0f;
        st = pgaccel_reduce_sum_f32(data, N, &sum);
        if (st != PGACCEL_OK) {
            fprintf(stderr, "Child reduce_sum_f32 failed: %d\n", st);
            _exit(2);
        }

        if (std::fabs(sum - (float)N) > 0.5f) {
            fprintf(stderr, "Child reduce_sum_f32 WRONG: got %f, expected %zu\n",
                    sum, N);
            _exit(3);
        }
        printf("Child: reduce_sum_f32(%zu floats) = %f OK\n", N, sum);

        uint64_t gpu = pgaccel_gpu_exec_count();
        printf("Child: gpu_exec=%llu\n", (unsigned long long)gpu);
        if (gpu == 0) {
            fprintf(stderr, "Child: FAIL — gpu_exec=0 (SYCL died post-fork)\n");
            _exit(4);
        }

        delete[] data;
        pgaccel_shutdown();
        printf("\nChild: PASS — GPU works after postmaster warmup + fork.\n");
        _exit(0);
    }

    int wstatus = 0;
    waitpid(pid, &wstatus, 0);

    printf("\n=== Results ===\n");
    if (WIFEXITED(wstatus)) {
        int rc = WEXITSTATUS(wstatus);
        if (rc == 0) {
            printf("PASS: postmaster warmup + fork + child GPU dispatch OK.\n");
            return 0;
        }
        printf("FAIL: child exit code %d\n", rc);
        return rc;
    }
    if (WIFSIGNALED(wstatus)) {
        int sig = WTERMSIG(wstatus);
        printf("FAIL: child killed by signal %d (warmup did NOT prevent crash)\n",
               sig);
        return 1;
    }
    return 1;
}
