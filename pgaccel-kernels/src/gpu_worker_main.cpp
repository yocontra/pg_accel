/// Standalone GPU worker process for pg_accel.
///
/// This binary is fork+exec()'d by the PG Background Worker. The exec() call
/// resets all inherited Mach ports, establishing fresh XPC connections to
/// MTLCompilerService — which is required for Metal shader JIT compilation.
///
/// Protocol (binary, little-endian, v2 — POSIX shm for bulk data):
///   Request  (stdin):  [op:u32][n_rows:u64][shm_name_len:u32][shm_name:u8*len][data_len:u64]
///   Response (stdout): [status:i32][scalar_f64:f64][scalar_i64:i64]
///
///   Bulk data is exchanged via a POSIX shared memory segment (shm_open).
///   The BGW creates the segment, the worker mmaps it, operates in-place,
///   then the BGW reads results back. Only tiny control messages go through pipes.
///
/// The worker stays alive and processes requests in a loop until stdin is closed
/// (BGW exit) or an unrecoverable error occurs.

#include "pgaccel_ffi.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <unistd.h>

#include <fcntl.h>
#include <sys/mman.h>

// ---------------------------------------------------------------------------
// I/O helpers — read/write exact byte counts from/to fd
// ---------------------------------------------------------------------------

static bool read_exact(int fd, void* buf, size_t len) {
    auto* p = static_cast<uint8_t*>(buf);
    while (len > 0) {
        ssize_t n = read(fd, p, len);
        if (n <= 0) return false; // EOF or error
        p += n;
        len -= static_cast<size_t>(n);
    }
    return true;
}

static bool write_exact(int fd, const void* buf, size_t len) {
    auto* p = static_cast<const uint8_t*>(buf);
    while (len > 0) {
        ssize_t n = write(fd, p, len);
        if (n <= 0) return false;
        p += n;
        len -= static_cast<size_t>(n);
    }
    return true;
}

// ---------------------------------------------------------------------------
// Op codes — must match GpuOp in gpu_bgw.rs
// ---------------------------------------------------------------------------

enum GpuWorkerOp : uint32_t {
    ReduceSumF32 = 1,
    ReduceMinF32 = 2,
    ReduceMaxF32 = 3,
    ReduceSumF64 = 4,
    ReduceMinF64 = 5,
    ReduceMaxF64 = 6,
    ReduceSumI64 = 7,
    SortKvF32 = 14,
    SortKvF64 = 15,
};

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

int main() {
    const int in_fd = STDIN_FILENO;
    const int out_fd = STDOUT_FILENO;

    fprintf(stderr, "pgaccel-gpu-worker: starting (pid=%d)\n", getpid());

    // Initialize GPU.
    pgaccel_status init_st = pgaccel_init();
    if (init_st != PGACCEL_OK) {
        fprintf(stderr, "pgaccel-gpu-worker: pgaccel_init failed: %d\n", init_st);
        return 1;
    }

    pgaccel_device_info info = pgaccel_get_device_info();
    fprintf(stderr, "pgaccel-gpu-worker: GPU ready — %s (%u CUs)\n",
            info.device_name, info.compute_units);

    // Signal readiness: write a single byte, then device info.
    uint8_t ready = 1;
    if (!write_exact(out_fd, &ready, 1)) {
        fprintf(stderr, "pgaccel-gpu-worker: failed to signal readiness\n");
        return 1;
    }

    // Send device info: [compute_units:u32][has_fp64:u8][is_unified:u8][max_alloc:u64][name:128]
    if (!write_exact(out_fd, &info.compute_units, sizeof(uint32_t))) return 1;
    uint8_t fp64 = info.has_fp64 ? 1 : 0;
    uint8_t unified = info.is_unified_memory ? 1 : 0;
    if (!write_exact(out_fd, &fp64, 1)) return 1;
    if (!write_exact(out_fd, &unified, 1)) return 1;
    uint64_t max_alloc = static_cast<uint64_t>(info.max_alloc_bytes);
    if (!write_exact(out_fd, &max_alloc, sizeof(uint64_t))) return 1;
    if (!write_exact(out_fd, info.device_name, 128)) return 1;

    // Request loop.
    while (true) {
        // Read request header: [op:u32][n_rows:u64][shm_name_len:u32]
        uint32_t op;
        uint64_t n_rows;
        uint32_t shm_name_len;

        if (!read_exact(in_fd, &op, sizeof(op))) break;
        if (!read_exact(in_fd, &n_rows, sizeof(n_rows))) break;
        if (!read_exact(in_fd, &shm_name_len, sizeof(shm_name_len))) break;

        // Read shm name.
        std::string shm_name(shm_name_len, '\0');
        if (shm_name_len > 0) {
            if (!read_exact(in_fd, &shm_name[0], shm_name_len)) break;
        }

        // Read data length (total bytes in the shm segment).
        uint64_t data_len;
        if (!read_exact(in_fd, &data_len, sizeof(data_len))) break;

        // Open and mmap the POSIX shared memory segment.
        uint8_t* shm_ptr = nullptr;
        int shm_fd = -1;
        if (data_len > 0 && shm_name_len > 0) {
            shm_fd = shm_open(shm_name.c_str(), O_RDWR, 0);
            if (shm_fd < 0) {
                fprintf(stderr, "pgaccel-gpu-worker: shm_open(%s) failed: %s\n",
                        shm_name.c_str(), strerror(errno));
                // Send error response and continue.
                int32_t err_status = -1;
                double zero_f64 = 0.0;
                int64_t zero_i64 = 0;
                write_exact(out_fd, &err_status, sizeof(err_status));
                write_exact(out_fd, &zero_f64, sizeof(zero_f64));
                write_exact(out_fd, &zero_i64, sizeof(zero_i64));
                continue;
            }
            shm_ptr = static_cast<uint8_t*>(
                mmap(nullptr, data_len, PROT_READ | PROT_WRITE, MAP_SHARED, shm_fd, 0));
            if (shm_ptr == MAP_FAILED) {
                fprintf(stderr, "pgaccel-gpu-worker: mmap failed: %s\n", strerror(errno));
                close(shm_fd);
                int32_t err_status = -1;
                double zero_f64 = 0.0;
                int64_t zero_i64 = 0;
                write_exact(out_fd, &err_status, sizeof(err_status));
                write_exact(out_fd, &zero_f64, sizeof(zero_f64));
                write_exact(out_fd, &zero_i64, sizeof(zero_i64));
                continue;
            }
        }

        // Execute the kernel. Data is operated on in-place in shm.
        int32_t status = 0;
        double scalar_f64 = 0.0;
        int64_t scalar_i64 = 0;

        switch (op) {
        case ReduceSumF32: {
            float result = 0.0f;
            status = pgaccel_reduce_sum_f32(
                reinterpret_cast<const float*>(shm_ptr),
                static_cast<size_t>(n_rows), &result);
            scalar_f64 = static_cast<double>(result);
            break;
        }
        case ReduceMinF32: {
            float result = 0.0f;
            status = pgaccel_reduce_min_f32(
                reinterpret_cast<const float*>(shm_ptr),
                static_cast<size_t>(n_rows), &result);
            scalar_f64 = static_cast<double>(result);
            break;
        }
        case ReduceMaxF32: {
            float result = 0.0f;
            status = pgaccel_reduce_max_f32(
                reinterpret_cast<const float*>(shm_ptr),
                static_cast<size_t>(n_rows), &result);
            scalar_f64 = static_cast<double>(result);
            break;
        }
        case ReduceSumF64: {
            double result = 0.0;
            status = pgaccel_reduce_sum_f64(
                reinterpret_cast<const double*>(shm_ptr),
                static_cast<size_t>(n_rows), &result);
            scalar_f64 = result;
            break;
        }
        case ReduceMinF64: {
            double result = 0.0;
            status = pgaccel_reduce_min_f64(
                reinterpret_cast<const double*>(shm_ptr),
                static_cast<size_t>(n_rows), &result);
            scalar_f64 = result;
            break;
        }
        case ReduceMaxF64: {
            double result = 0.0;
            status = pgaccel_reduce_max_f64(
                reinterpret_cast<const double*>(shm_ptr),
                static_cast<size_t>(n_rows), &result);
            scalar_f64 = result;
            break;
        }
        case ReduceSumI64: {
            int64_t result = 0;
            status = pgaccel_reduce_sum_i64(
                reinterpret_cast<const int64_t*>(shm_ptr),
                static_cast<size_t>(n_rows), &result);
            scalar_i64 = result;
            break;
        }
        case SortKvF32: {
            size_t n = static_cast<size_t>(n_rows);
            auto* keys = reinterpret_cast<float*>(shm_ptr);
            auto* indices = reinterpret_cast<uint32_t*>(shm_ptr + n * sizeof(float));
            status = pgaccel_sort_kv_f32(keys, indices, n);
            // Sorted in-place in shm — BGW reads back directly.
            break;
        }
        case SortKvF64: {
            size_t n = static_cast<size_t>(n_rows);
            auto* keys = reinterpret_cast<double*>(shm_ptr);
            auto* indices = reinterpret_cast<uint32_t*>(shm_ptr + n * sizeof(double));
            status = pgaccel_sort_kv_f64(keys, indices, n);
            break;
        }
        default:
            fprintf(stderr, "pgaccel-gpu-worker: unknown op %u\n", op);
            status = PGACCEL_UNSUPPORTED;
            break;
        }

        // Unmap shm (BGW owns cleanup of the segment).
        if (shm_ptr && shm_ptr != MAP_FAILED) {
            munmap(shm_ptr, data_len);
        }
        if (shm_fd >= 0) {
            close(shm_fd);
        }

        // Send response: only scalars, no bulk data (it's in shm).
        if (!write_exact(out_fd, &status, sizeof(status))) break;
        if (!write_exact(out_fd, &scalar_f64, sizeof(scalar_f64))) break;
        if (!write_exact(out_fd, &scalar_i64, sizeof(scalar_i64))) break;
    }

    fprintf(stderr, "pgaccel-gpu-worker: shutting down\n");
    pgaccel_shutdown();
    return 0;
}
