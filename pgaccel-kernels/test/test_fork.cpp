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

#include "pgaccel_ffi.h"
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <sys/wait.h>
#include <unistd.h>

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
    for (size_t i = 0; i < N; i++) data[i] = VAL;

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
    printf("[%s] reduce_sum_f32: %f OK\n", label, sum);

    float min_val = 0.0f;
    st = pgaccel_reduce_min_f32(data, N, &min_val);
    if (st != PGACCEL_OK || !approx_eq(min_val, VAL, TOLERANCE)) {
        fprintf(stderr, "[%s] reduce_min_f32 failed: status=%d val=%f\n",
                label, st, min_val);
        return 1;
    }
    printf("[%s] reduce_min_f32: %f OK\n", label, min_val);

    float max_val = 0.0f;
    st = pgaccel_reduce_max_f32(data, N, &max_val);
    if (st != PGACCEL_OK || !approx_eq(max_val, VAL, TOLERANCE)) {
        fprintf(stderr, "[%s] reduce_max_f32 failed: status=%d val=%f\n",
                label, st, max_val);
        return 1;
    }
    printf("[%s] reduce_max_f32: %f OK\n", label, max_val);

    return 0;
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
            fprintf(stderr, "Child pgaccel_init failed: %d — GPU init "
                            "after fork did not work\n", st);
            _exit(1);
        }

        pgaccel_device_info info = pgaccel_get_device_info();
        printf("Child: pgaccel_init OK — device=%s backend=%s CUs=%u\n",
               info.device_name, info.backend_name, info.compute_units);

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

        uint64_t child_gpu = pgaccel_gpu_exec_count();
        printf("\nChild: gpu_exec=%llu\n", (unsigned long long)child_gpu);
        if (child_gpu == 0) {
            fprintf(stderr,
                "Child: FAIL — GPU exec count is 0. SYCL died post-fork.\n");
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
               "after fork.\n", sig);
        return 1;
    }

    return 1;
}
