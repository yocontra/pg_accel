#include "pgaccel_ffi.h"
#include <cstring>
#include <cstdio>
#include <atomic>
#include <unistd.h>
#include <signal.h>
#include <cstdlib>

#include <sycl/sycl.hpp>
#include <hipSYCL/runtime/fork_safety.h>
#include <string>
#include <algorithm>
#include <cctype>

// ---------------------------------------------------------------------------
// GPU execution observability — thread-local counter
// ---------------------------------------------------------------------------

static thread_local uint64_t tl_gpu_exec_count = 0;

extern "C" uint64_t pgaccel_gpu_exec_count(void) {
    return tl_gpu_exec_count;
}

extern "C" void pgaccel_reset_gpu_exec_count(void) {
    tl_gpu_exec_count = 0;
}

void pgaccel_record_gpu_exec() {
    tl_gpu_exec_count++;
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
        // Forked child — release inherited backend state through
        // AdaptiveCpp's fork-safety entry point, then fall through to a
        // fresh device pick + queue construction. The reset drops stale
        // MTL::Device/CommandQueue pointers and the in-memory kernel
        // cache; compiled .metallib files on disk survive and will be
        // reloaded without re-entering MTLCompilerService.
        g_queue = nullptr;
        g_unified_memory = false;
        hipsycl_rt_reset_after_fork();
        fprintf(stderr, "pgaccel: fork detected (parent=%d, child=%d)"
                        " — attempting fresh GPU init\n",
                g_init_pid, current_pid);
        g_initialized.store(false, std::memory_order_release);
        // Fall through to normal init path below.
    }

    // Temporarily reset signal handlers around SYCL init. PG installs
    // custom SIGSEGV/SIGBUS handlers that interfere with Metal/IOKit
    // driver initialization (the driver uses signals internally during
    // device enumeration). shared_preload_libraries ensures libacpp-rt.dylib
    // was loaded in the postmaster before fork, so the child inherits the
    // loaded library and can create a fresh MTLDevice without triggering
    // the compiler service.
    struct sigaction old_handlers[32];
    for (int sig = 1; sig < 32; sig++) {
        if (sig == SIGKILL || sig == SIGSTOP) continue;
        struct sigaction sa = {};
        sa.sa_handler = SIG_DFL;
        sigemptyset(&sa.sa_mask);
        sigaction(sig, &sa, &old_handlers[sig]);
    }

    bool init_ok = false;
    try {
        auto devices = sycl::device::get_devices();

        if (devices.empty()) {
            fprintf(stderr, "pgaccel: FATAL: no SYCL devices found\n");
        } else {
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

            g_queue = new sycl::queue(
                best, sycl::property_list{sycl::property::queue::in_order{}});

            // Silent success: backend init fires per-forked-backend, so
            // logging here produces O(queries) log lines. See Justfile
            // `log-rails` recipe for how PG's own log is rotated.
            init_ok = true;
        }
    } catch (const sycl::exception& e) {
        fprintf(stderr, "pgaccel: FATAL: SYCL init failed: %s\n", e.what());
    } catch (const std::exception& e) {
        fprintf(stderr, "pgaccel: FATAL: init failed: %s\n", e.what());
    }

    // Restore PG signal handlers.
    for (int sig = 1; sig < 32; sig++) {
        if (sig == SIGKILL || sig == SIGSTOP) continue;
        sigaction(sig, &old_handlers[sig], nullptr);
    }

    if (!init_ok) {
        return PGACCEL_ERROR;
    }

    g_init_pid = current_pid;
    g_initialized.store(true, std::memory_order_release);
    return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_shutdown(void) {
    if (!g_initialized.load(std::memory_order_acquire)) {
        return PGACCEL_OK;
    }

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

extern "C" void pgaccel_prefork_warmup(void) {
#ifdef __APPLE__
    // macOS Sequoia+ aborts forked processes that initialize Objective-C
    // frameworks (like Metal/SkyLight) after fork. This is the documented
    // opt-out. We deliberately do NOT initialize SYCL or touch Metal in the
    // parent — Apple's IOGPUMetalDevice caches process-local state that
    // cannot be reset on the child side, so any parent-side Metal activity
    // propagates stale IOKit handles into every forked child. Kernel
    // compilation is deferred to the first child: AdaptiveCpp's Metal
    // backend now routes MSL through `xcrun metal` in a subprocess and
    // caches the resulting .metallib on disk, so post-fork cold-starts
    // avoid MTLCompilerService entirely.
    setenv("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES", 0);
#endif
}
