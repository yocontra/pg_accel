// test_metal_fork — Prove zero-IPC: metal_backend reduce works after fork.
#include "metal_backend.h"
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <sys/wait.h>
#include <unistd.h>

int main() {
    printf("=== Metal Backend Fork Test ===\n");
    printf("Parent PID %d does NOT touch Metal.\n\n", getpid());

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 1; }

    if (pid == 0) {
        // Child: init Metal from scratch, run reduce kernels
        metal_status st = metal_init();
        if (st != METAL_OK) {
            fprintf(stderr, "Child: metal_init failed: %d\n", st);
            _exit(1);
        }
        printf("Child: metal_init OK\n");

        // reduce_sum_f32
        const size_t N = 100000;
        float* data = (float*)malloc(N * sizeof(float));
        for (size_t i = 0; i < N; i++) data[i] = 7.0f;
        float sum = 0;
        st = metal_reduce_sum_f32(data, N, &sum);
        if (st != METAL_OK || fabsf(sum - 700000.0f) > 100.0f) {
            fprintf(stderr, "Child: reduce_sum_f32 FAIL st=%d sum=%f\n", st, sum);
            _exit(2);
        }
        printf("Child: reduce_sum_f32 = %f OK\n", sum);

        // reduce_sum_i64 (was crashing with SYCL!)
        int64_t* idata = (int64_t*)malloc(N * sizeof(int64_t));
        for (size_t i = 0; i < N; i++) idata[i] = 3;
        int64_t isum = 0;
        st = metal_reduce_sum_i64(idata, N, &isum);
        if (st != METAL_OK || isum != 300000) {
            fprintf(stderr, "Child: reduce_sum_i64 FAIL st=%d sum=%lld\n",
                    st, (long long)isum);
            _exit(3);
        }
        printf("Child: reduce_sum_i64 = %lld OK\n", (long long)isum);

        // reduce_multi_f32
        float ms, mn, mx; int64_t mc;
        st = metal_reduce_multi_f32(data, N, &ms, &mn, &mx, &mc);
        if (st != METAL_OK) {
            fprintf(stderr, "Child: reduce_multi_f32 FAIL st=%d\n", st);
            _exit(4);
        }
        printf("Child: reduce_multi_f32 sum=%f min=%f max=%f count=%lld OK\n",
               ms, mn, mx, (long long)mc);

        free(data); free(idata);
        printf("\n=== PASS: All reduce kernels work after fork. ZERO IPC. ===\n");
        _exit(0);
    }

    int wstatus;
    waitpid(pid, &wstatus, 0);
    if (WIFEXITED(wstatus) && WEXITSTATUS(wstatus) == 0) {
        printf("\nPASS\n");
        return 0;
    }
    if (WIFSIGNALED(wstatus))
        printf("CRASH: signal %d\n", WTERMSIG(wstatus));
    else
        printf("FAIL: exit %d\n", WEXITSTATUS(wstatus));
    return 1;
}
