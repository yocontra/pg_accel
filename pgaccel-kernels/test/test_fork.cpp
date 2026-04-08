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

static int run_reduce_tests(const char* label) {
    float data[N];
    for (size_t i = 0; i < N; i++) {
        data[i] = VAL;
    }

    // reduce_sum_f32
    float sum = 0.0f;
    pgaccel_status st = pgaccel_reduce_sum_f32(data, N, &sum);
    if (st != PGACCEL_OK) {
        fprintf(stderr, "[%s] reduce_sum_f32 failed: status=%d\n", label, st);
        return 1;
    }
    if (!approx_eq(sum, EXPECTED_SUM, TOLERANCE)) {
        fprintf(stderr, "[%s] reduce_sum_f32 WRONG: got %f, expected %f\n",
                label, sum, EXPECTED_SUM);
        return 1;
    }
    printf("[%s] reduce_sum_f32: %f (expected %f) OK\n", label, sum, EXPECTED_SUM);

    // reduce_min_f32
    float min_val = 0.0f;
    st = pgaccel_reduce_min_f32(data, N, &min_val);
    if (st != PGACCEL_OK) {
        fprintf(stderr, "[%s] reduce_min_f32 failed: status=%d\n", label, st);
        return 1;
    }
    if (!approx_eq(min_val, VAL, TOLERANCE)) {
        fprintf(stderr, "[%s] reduce_min_f32 WRONG: got %f, expected %f\n",
                label, min_val, VAL);
        return 1;
    }
    printf("[%s] reduce_min_f32: %f (expected %f) OK\n", label, min_val, VAL);

    // reduce_max_f32
    float max_val = 0.0f;
    st = pgaccel_reduce_max_f32(data, N, &max_val);
    if (st != PGACCEL_OK) {
        fprintf(stderr, "[%s] reduce_max_f32 failed: status=%d\n", label, st);
        return 1;
    }
    if (!approx_eq(max_val, VAL, TOLERANCE)) {
        fprintf(stderr, "[%s] reduce_max_f32 WRONG: got %f, expected %f\n",
                label, max_val, VAL);
        return 1;
    }
    printf("[%s] reduce_max_f32: %f (expected %f) OK\n", label, max_val, VAL);

    return 0;
}

int main() {
    printf("=== Fork GPU Test ===\n");
    printf("Parent PID: %d\n\n", getpid());

    // Step 1: Init in parent
    pgaccel_status st = pgaccel_init();
    if (st != PGACCEL_OK) {
        fprintf(stderr, "Parent pgaccel_init failed: %d\n", st);
        return 1;
    }
    printf("Parent: pgaccel_init OK\n");

    // Step 2: Run reduce tests in parent
    if (run_reduce_tests("parent") != 0) {
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
        // ── Child process ──
        printf("\nChild PID: %d (parent was %d)\n", getpid(), getppid());

        // Step 4a: Re-init in child (should detect PID change)
        st = pgaccel_init();
        if (st != PGACCEL_OK) {
            fprintf(stderr, "Child pgaccel_init failed: %d\n", st);
            _exit(1);
        }
        printf("Child: pgaccel_init OK (re-initialized after fork)\n");

        // Step 4b-d: Run reduce tests in child
        int rc = run_reduce_tests("child");
        if (rc != 0) {
            fprintf(stderr, "\nChild: FAIL — GPU kernels broken after fork\n");
            _exit(1);
        }

        printf("\nChild: PASS — all GPU kernels work after fork\n");
        _exit(0);
    }

    // ── Parent waits ──
    int wstatus = 0;
    waitpid(pid, &wstatus, 0);

    printf("\n=== Results ===\n");
    if (WIFEXITED(wstatus)) {
        int child_rc = WEXITSTATUS(wstatus);
        if (child_rc == 0) {
            printf("PASS: Child (PID %d) exited with code 0\n", pid);
            printf("GPU kernels work correctly after fork().\n");
        } else {
            printf("FAIL: Child (PID %d) exited with code %d\n", pid, child_rc);
            printf("GPU kernels BROKEN after fork().\n");
        }
        pgaccel_shutdown();
        return child_rc;
    } else if (WIFSIGNALED(wstatus)) {
        int sig = WTERMSIG(wstatus);
        printf("FAIL: Child (PID %d) killed by signal %d\n", pid, sig);
        printf("GPU runtime crashed after fork().\n");
        pgaccel_shutdown();
        return 1;
    }

    pgaccel_shutdown();
    return 1;
}
