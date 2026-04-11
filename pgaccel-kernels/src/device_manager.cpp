#include "pgaccel_ffi.h"
#include <cstring>
#include <cstdio>
#include <atomic>
#include <unistd.h>

#if PGACCEL_HAS_SYCL
#include <sycl/sycl.hpp>
#include <string>
#include <algorithm>
#include <cctype>
#endif

// ---------------------------------------------------------------------------
// GPU execution observability — thread-local counters
// ---------------------------------------------------------------------------

static thread_local uint64_t tl_gpu_exec_count = 0;
static thread_local uint64_t tl_cpu_fallback_count = 0;

extern "C" uint64_t pgaccel_gpu_exec_count(void) {
    return tl_gpu_exec_count;
}

extern "C" void pgaccel_reset_gpu_exec_count(void) {
    tl_gpu_exec_count = 0;
}

extern "C" uint64_t pgaccel_cpu_fallback_count(void) {
    return tl_cpu_fallback_count;
}

extern "C" void pgaccel_reset_cpu_fallback_count(void) {
    tl_cpu_fallback_count = 0;
}

void pgaccel_record_gpu_exec() {
    tl_gpu_exec_count++;
}

void pgaccel_warn_cpu_fallback(const char* kernel_name) {
    tl_cpu_fallback_count++;
    fprintf(stderr,
        "pgaccel WARNING: %s fell back to CPU! GPU kernel did not execute. "
        "Fallback count: %lu. This means the GPU is NOT being used.\n",
        kernel_name, (unsigned long)tl_cpu_fallback_count);
}

// ---------------------------------------------------------------------------
// Module-level state (initialized once, read-only after init)
// ---------------------------------------------------------------------------

static std::atomic<bool> g_initialized{false};
/// PID that called pgaccel_init(). After fork(), getpid() differs, so
/// we know to reinitialize the SYCL runtime in the child process.
static pid_t g_init_pid = 0;
static pgaccel_device_info g_device_info = {};
static pgaccel_platform_caps g_caps = {};

#if PGACCEL_HAS_SYCL
// Accessed by other translation units (sort.cpp, mem_pool.cpp) via extern.
sycl::queue* g_queue = nullptr;

// Accessed by alloc_helper.h via extern. Set during pgaccel_init().
bool g_unified_memory = false;

// ---------------------------------------------------------------------------
// Backend name detection
// ---------------------------------------------------------------------------

static std::string detect_backend_name(const sycl::device& dev) {
    // SAFETY: platform name is always available on a valid SYCL device.
    std::string platform_name = dev.get_platform().get_info<sycl::info::platform::name>();

    // Lowercase for matching.
    std::string lower = platform_name;
    std::transform(lower.begin(), lower.end(), lower.begin(),
                   [](unsigned char c) { return std::tolower(c); });

    if (lower.find("cuda") != std::string::npos) return "cuda";
    if (lower.find("hip") != std::string::npos) return "hip";
    if (lower.find("level-zero") != std::string::npos
        || lower.find("level zero") != std::string::npos
        || lower.find("oneapi") != std::string::npos) return "level_zero";
    if (lower.find("metal") != std::string::npos
        || lower.find("apple") != std::string::npos) return "metal";

    if (dev.is_cpu()) return "cpu";
    return "unknown";
}

// ---------------------------------------------------------------------------
// Device scoring — higher is better
// ---------------------------------------------------------------------------

static int score_device(const sycl::device& dev) {
    std::string backend = detect_backend_name(dev);

    if (dev.is_cpu()) return 0;

    // Discrete GPU backends, ranked by maturity.
    if (backend == "cuda")       return 100;
    if (backend == "hip")        return 90;
    if (backend == "level_zero") return 80;

    // Integrated GPU backends.
    if (backend == "metal")      return 50;

    // Generic GPU we don't recognize.
    if (dev.is_gpu()) return 40;

    return 10;
}

// ---------------------------------------------------------------------------
// Populate caps from SYCL device
// ---------------------------------------------------------------------------

static void populate_caps(const sycl::device& dev, const std::string& backend) {
    g_caps.has_fp64 = dev.has(sycl::aspect::fp64);
    g_caps.has_atomic64 = dev.has(sycl::aspect::atomic64);
    g_caps.has_ooo_queue = (backend != "metal" && backend != "cpu");
    g_caps.is_unified_memory =
        dev.has(sycl::aspect::usm_shared_allocations) && !dev.is_gpu();
    // Apple Silicon has unified physical memory, but AdaptiveCpp's Metal
    // backend requires sycl::malloc_device/shared — raw host pointers
    // silently read as zero from GPU kernels.  Do NOT enable the unified
    // memory fast-path for Metal.
    g_caps.max_alloc_bytes =
        dev.get_info<sycl::info::device::max_mem_alloc_size>();
    g_caps.compute_units =
        dev.get_info<sycl::info::device::max_compute_units>();
    std::strncpy(g_caps.backend_name, backend.c_str(),
                 sizeof(g_caps.backend_name) - 1);
    g_caps.backend_name[sizeof(g_caps.backend_name) - 1] = '\0';
}

// ---------------------------------------------------------------------------
// Populate device info from SYCL device
// ---------------------------------------------------------------------------

static void populate_device_info(const sycl::device& dev,
                                 const std::string& backend) {
    // SAFETY: device name is always available on a valid SYCL device.
    std::string name = dev.get_info<sycl::info::device::name>();
    std::strncpy(g_device_info.device_name, name.c_str(),
                 sizeof(g_device_info.device_name) - 1);
    g_device_info.device_name[sizeof(g_device_info.device_name) - 1] = '\0';

    std::strncpy(g_device_info.backend_name, backend.c_str(),
                 sizeof(g_device_info.backend_name) - 1);
    g_device_info.backend_name[sizeof(g_device_info.backend_name) - 1] = '\0';

    g_device_info.compute_units =
        dev.get_info<sycl::info::device::max_compute_units>();
    g_device_info.max_alloc_bytes =
        dev.get_info<sycl::info::device::max_mem_alloc_size>();
    g_device_info.has_fp64 = dev.has(sycl::aspect::fp64);
    g_device_info.has_atomic64 = dev.has(sycl::aspect::atomic64);
    g_device_info.is_unified_memory = g_caps.is_unified_memory;
}

#endif // PGACCEL_HAS_SYCL

// ---------------------------------------------------------------------------
// CPU fallback population (no SYCL, or all device init failed)
// ---------------------------------------------------------------------------

static void populate_cpu_fallback() {
    std::strncpy(g_device_info.device_name, "CPU Fallback",
                 sizeof(g_device_info.device_name) - 1);
    std::strncpy(g_device_info.backend_name, "cpu",
                 sizeof(g_device_info.backend_name) - 1);
    g_device_info.has_fp64 = true;
    g_device_info.has_atomic64 = true;
    g_device_info.is_unified_memory = true;
    g_device_info.compute_units = 0;
    g_device_info.max_alloc_bytes = 0;

    g_caps.has_fp64 = true;
    g_caps.has_atomic64 = true;
    g_caps.has_ooo_queue = false;
    g_caps.is_unified_memory = true;
    g_caps.max_alloc_bytes = 0;
    g_caps.compute_units = 0;
    std::strncpy(g_caps.backend_name, "cpu",
                 sizeof(g_caps.backend_name) - 1);
}

// ===========================================================================
// Public API
// ===========================================================================

extern "C" pgaccel_status pgaccel_init(void) {
    // Detect fork: if PID changed, the SYCL runtime context from the
    // parent process is stale. Reset state so we reinitialize.
    pid_t current_pid = getpid();
    if (g_initialized.load(std::memory_order_acquire)) {
        if (g_init_pid == current_pid) {
            return PGACCEL_OK;
        }
        // Forked child — stale SYCL/Metal context. The IOKit GPU
        // driver connection (Mach ports) is inherited but broken after
        // fork(). Even creating a fresh sycl::queue doesn't help because
        // AdaptiveCpp reuses the parent's MTLDevice, whose IOKit
        // allocator crashes on sycl::malloc_device (SIGSEGV in
        // IOGPUMetalDevice allocBuffer). Fall back to CPU in forked
        // backends; GPU work is routed through the BGW coordinator.
#if PGACCEL_HAS_SYCL
        g_queue = nullptr;
        g_unified_memory = false;
#endif
        fprintf(stderr, "pgaccel: fork detected (parent=%d, child=%d)"
                        " — GPU unavailable in forked backend\n",
                g_init_pid, current_pid);
        populate_cpu_fallback();
        g_init_pid = current_pid;
        g_initialized.store(true, std::memory_order_release);
        return PGACCEL_OK;
    }

#if PGACCEL_HAS_SYCL
    try {
        // Enumerate all devices and pick the best one.
        auto devices = sycl::device::get_devices();
        if (devices.empty()) {
            fprintf(stderr, "pgaccel: no SYCL devices found, using CPU fallback\n");
            populate_cpu_fallback();
            g_initialized.store(true, std::memory_order_release);
            return PGACCEL_OK;
        }

        sycl::device best = devices[0];
        int best_score = score_device(best);
        for (size_t i = 1; i < devices.size(); ++i) {
            int s = score_device(devices[i]);
            if (s > best_score) {
                best = devices[i];
                best_score = s;
            }
        }

        std::string backend = detect_backend_name(best);
        populate_caps(best, backend);
        g_unified_memory = g_caps.is_unified_memory;
        populate_device_info(best, backend);

        // Create queue: in-order for Metal, out-of-order otherwise.
        if (g_caps.has_ooo_queue) {
            // SAFETY: g_queue is only written here, under the g_initialized
            // guard, so no concurrent writes are possible.
            g_queue = new sycl::queue(
                best, sycl::property_list{sycl::property::queue::in_order{}});
            // Note: AdaptiveCpp may not support out-of-order on all backends.
            // We request in-order universally for now and set has_ooo_queue
            // based on backend capability for future use.
        } else {
            // SAFETY: same as above.
            g_queue = new sycl::queue(
                best, sycl::property_list{sycl::property::queue::in_order{}});
        }

        fprintf(stderr, "pgaccel: initialized [%s] on %s (%u CUs, %s)\n",
                g_device_info.backend_name, g_device_info.device_name,
                g_device_info.compute_units,
                g_caps.has_fp64 ? "fp64" : "fp32-only");

    } catch (const sycl::exception& e) {
        fprintf(stderr, "pgaccel: SYCL init failed: %s — using CPU fallback\n",
                e.what());
        populate_cpu_fallback();
    } catch (const std::exception& e) {
        fprintf(stderr, "pgaccel: init failed: %s — using CPU fallback\n",
                e.what());
        populate_cpu_fallback();
    }
#else
    populate_cpu_fallback();
    fprintf(stderr, "pgaccel: built without SYCL, using CPU fallback\n");
#endif

    g_init_pid = current_pid;
    g_initialized.store(true, std::memory_order_release);
    return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_shutdown(void) {
    if (!g_initialized.load(std::memory_order_acquire)) {
        return PGACCEL_OK;
    }

#if PGACCEL_HAS_SYCL
    // SAFETY: g_queue is only modified during init/shutdown which are
    // guaranteed to be called from the PG backend main thread (single writer).
    if (g_queue != nullptr) {
        try {
            g_queue->wait();
        } catch (...) {
            // Best-effort flush; nothing useful to do on failure.
        }
        delete g_queue;
        g_queue = nullptr;
    }
#endif

    std::memset(&g_device_info, 0, sizeof(g_device_info));
    std::memset(&g_caps, 0, sizeof(g_caps));
    g_initialized.store(false, std::memory_order_release);
    return PGACCEL_OK;
}

extern "C" pgaccel_device_info pgaccel_get_device_info(void) {
    return g_device_info;
}

extern "C" pgaccel_platform_caps pgaccel_get_caps(void) {
    return g_caps;
}
