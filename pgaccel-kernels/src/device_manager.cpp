#include <sycl/sycl.hpp>

#include <signal.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

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

// Accessed by other translation units (sort.cpp, window.cpp, mem_pool.cpp) via extern.
sycl::queue* g_queue = nullptr;
sycl::queue* g_ooo_queue = nullptr;

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

  if (lower.find("cuda") != std::string::npos)
    return "cuda";
  if (lower.find("hip") != std::string::npos)
    return "hip";
  if (lower.find("level-zero") != std::string::npos ||
      lower.find("level zero") != std::string::npos || lower.find("oneapi") != std::string::npos)
    return "level_zero";
  if (lower.find("metal") != std::string::npos)
    return "metal";

  return "unknown";
}

// ---------------------------------------------------------------------------
// Device scoring — higher is better
// ---------------------------------------------------------------------------

static int score_device(const sycl::device& dev) {
  if (!dev.is_gpu())
    return -1;

  std::string backend = detect_backend_name(dev);

  // Discrete GPU backends, ranked by maturity.
  if (backend == "cuda")
    return 100;
  if (backend == "hip")
    return 90;
  if (backend == "level_zero")
    return 80;

  // Integrated GPU backends.
  if (backend == "metal")
    return 50;

  // Generic GPU we don't recognize.
  if (dev.is_gpu())
    return 40;

  return -1;
}

// ---------------------------------------------------------------------------
// Populate caps from SYCL device
// ---------------------------------------------------------------------------

static void populate_caps(const sycl::device& dev, const std::string& backend) {
  g_caps.has_native_fp64 = dev.has(sycl::aspect::fp64);
  g_caps.has_atomic64 = dev.has(sycl::aspect::atomic64);
  g_caps.has_ooo_queue = false;
  g_caps.max_alloc_bytes = dev.get_info<sycl::info::device::max_mem_alloc_size>();
  g_caps.compute_units = dev.get_info<sycl::info::device::max_compute_units>();
  std::strncpy(g_caps.backend_name, backend.c_str(), sizeof(g_caps.backend_name) - 1);
  g_caps.backend_name[sizeof(g_caps.backend_name) - 1] = '\0';
}

// ---------------------------------------------------------------------------
// Populate device info from SYCL device
// ---------------------------------------------------------------------------

static void populate_device_info(const sycl::device& dev, const std::string& backend) {
  // SAFETY: device name is always available on a valid SYCL device.
  std::string name = dev.get_info<sycl::info::device::name>();
  std::strncpy(g_device_info.device_name, name.c_str(), sizeof(g_device_info.device_name) - 1);
  g_device_info.device_name[sizeof(g_device_info.device_name) - 1] = '\0';

  std::strncpy(g_device_info.backend_name, backend.c_str(), sizeof(g_device_info.backend_name) - 1);
  g_device_info.backend_name[sizeof(g_device_info.backend_name) - 1] = '\0';

  g_device_info.compute_units = dev.get_info<sycl::info::device::max_compute_units>();
  g_device_info.max_alloc_bytes = dev.get_info<sycl::info::device::max_mem_alloc_size>();
  g_device_info.has_native_fp64 = dev.has(sycl::aspect::fp64);
  g_device_info.has_atomic64 = dev.has(sycl::aspect::atomic64);
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
    // Forked child — drop our cached SYCL queue and fall through to a
    // fresh device pick + queue construction. AdaptiveCpp detects the
    // fork internally at its dispatch chokepoints and recovers (Metal /
    // ROCm) or raises a clean error (CUDA / Level Zero) on the next use.
    g_queue = nullptr;
    g_ooo_queue = nullptr;
    fprintf(stderr,
            "pgaccel: fork detected (parent=%d, child=%d)"
            " — attempting fresh GPU init\n",
            g_init_pid, current_pid);
    g_initialized.store(false, std::memory_order_release);
    // Fall through to normal init path below.
  }

  // Narrow the signal surface around SYCL init instead of resetting all 31
  // handlers to SIG_DFL. PG installs custom SIGSEGV/SIGBUS handlers that
  // interfere with Metal/IOKit driver initialization (the driver relies on
  // default fault-signal dispositions during device enumeration), so only
  // the synchronous fault signals are reset to SIG_DFL for the window.
  // Every asynchronous signal (SIGTERM, SIGQUIT, SIGINT, SIGUSR1, ...) is
  // instead BLOCKED via sigprocmask: PG's die/quickdie handlers stay
  // installed and a signal arriving mid-enumeration is deferred until the
  // mask is restored, then delivered to PG's own handler. The previous
  // blanket SIG_DFL reset meant a SIGTERM in this window killed the backend
  // outright, bypassing PostgreSQL shared-memory cleanup.
  //
  // shared_preload_libraries ensures libacpp-rt.dylib was loaded in the
  // postmaster before fork, so the child inherits the loaded library and
  // can create a fresh MTLDevice without triggering the compiler service.
  static const int k_fault_signals[] = {SIGSEGV, SIGBUS, SIGILL, SIGFPE, SIGTRAP};
  constexpr size_t k_fault_count = sizeof(k_fault_signals) / sizeof(k_fault_signals[0]);

  // Block all async signals for the window. Fault signals must never be
  // blocked (delivery of a blocked synchronous fault is undefined), and
  // SIGKILL/SIGSTOP are unblockable anyway (sigprocmask ignores them).
  sigset_t block_mask;
  sigset_t old_mask;
  sigfillset(&block_mask);
  for (size_t i = 0; i < k_fault_count; i++) {
    sigdelset(&block_mask, k_fault_signals[i]);
  }
  sigprocmask(SIG_BLOCK, &block_mask, &old_mask);

  // Reset only the fault-signal handlers PG hooks to SIG_DFL.
  struct sigaction old_handlers[k_fault_count];
  for (size_t i = 0; i < k_fault_count; i++) {
    struct sigaction sa = {};
    sa.sa_handler = SIG_DFL;
    sigemptyset(&sa.sa_mask);
    sigaction(k_fault_signals[i], &sa, &old_handlers[i]);
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

      if (best_score < 0 || !best.is_gpu()) {
        fprintf(stderr, "pgaccel: FATAL: no SYCL GPU device found\n");
      } else {
        std::string backend = detect_backend_name(best);
        populate_caps(best, backend);
        populate_device_info(best, backend);

        // Async exception handler: AdaptiveCpp's Metal backend (and
        // other backends) report kernel-launch failures asynchronously
        // through the queue's exception list instead of throwing
        // synchronously from submit().  Without this handler, a
        // failed dispatch (e.g. MTLCompilerService unreachable) logs
        // "[AdaptiveCpp Error] ... metal: Failed to create compute
        // pipeline state" to stderr but q.submit(...).wait() returns
        // silently and the caller reads a zero-initialized output
        // buffer as if the kernel had succeeded.
        //
        // By installing a handler that rethrows every sycl::exception
        // it sees, callers that use wait_and_throw() (on either the
        // queue or the returned event) will surface these async
        // errors as synchronous exceptions at the wait site.
        auto async_handler = [](sycl::exception_list exceptions) {
          for (std::exception_ptr const& e : exceptions) {
            try {
              std::rethrow_exception(e);
            } catch (const sycl::exception& ex) {
              fprintf(stderr, "pgaccel: async SYCL error: %s\n", ex.what());
              throw;
            } catch (const std::exception& ex) {
              fprintf(stderr, "pgaccel: async error: %s\n", ex.what());
              throw;
            }
          }
        };

        g_queue = new sycl::queue(best, async_handler,
                                  sycl::property_list{sycl::property::queue::in_order{}});
        g_ooo_queue = new sycl::queue(best, async_handler,
                                      sycl::property_list{
                                          sycl::property::queue::enable_profiling{}});
        g_caps.has_ooo_queue = !g_ooo_queue->is_in_order();

        // Silent success: backend init fires per-forked-backend, so
        // logging here produces O(queries) log lines. See Justfile
        // `log-rails` recipe for how PG's own log is rotated.
        init_ok = true;
      }
    }
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: FATAL: SYCL init failed: %s\n", e.what());
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: FATAL: init failed: %s\n", e.what());
  } catch (...) {
    // Without this arm a non-std exception would escape the extern "C"
    // boundary (std::terminate) AND skip the handler/mask restore below.
    fprintf(stderr, "pgaccel: FATAL: init failed: unknown C++ exception\n");
  }

  // Restore PG's fault-signal handlers, then the signal mask. Any async
  // signal that arrived during the window is delivered now, to PG's own
  // handlers.
  for (size_t i = 0; i < k_fault_count; i++) {
    sigaction(k_fault_signals[i], &old_handlers[i], nullptr);
  }
  sigprocmask(SIG_SETMASK, &old_mask, nullptr);

  if (!init_ok) {
    return PGACCEL_ERROR;
  }

  g_init_pid = current_pid;
  g_initialized.store(true, std::memory_order_release);
  return PGACCEL_OK;
}

// Fork-safe queue accessors (declared in pgaccel_queue.h). pgaccel_init()
// is idempotent and re-checks getpid() on every call, so routing every
// queue access through these guarantees a forked child never dispatches on
// the parent's stale Metal/CUDA context.
sycl::queue* pgaccel_get_queue() {
  if (pgaccel_init() != PGACCEL_OK)
    return nullptr;
  return g_queue;
}

sycl::queue* pgaccel_get_ooo_queue() {
  if (pgaccel_init() != PGACCEL_OK)
    return nullptr;
  return g_ooo_queue;
}

extern "C" pgaccel_status pgaccel_shutdown(void) {
  if (!g_initialized.load(std::memory_order_acquire)) {
    return PGACCEL_OK;
  }

  // SAFETY: queues are only modified during init/shutdown which are
  // guaranteed to be called from the PG backend main thread (single writer).
  if (g_queue != nullptr || g_ooo_queue != nullptr) {
    try {
      if (g_ooo_queue != nullptr)
        g_ooo_queue->wait_and_throw();
      if (g_queue != nullptr)
        g_queue->wait_and_throw();
      pgaccel_pool_reset();
      if (g_ooo_queue != nullptr)
        g_ooo_queue->wait_and_throw();
      if (g_queue != nullptr)
        g_queue->wait_and_throw();
    } catch (const std::exception& e) {
      // Best-effort flush during teardown; log so a failing in-flight
      // kernel is visible, but continue releasing the queues.
      fprintf(stderr, "pgaccel: shutdown queue flush failed: %s\n", e.what());
    } catch (...) {
      fprintf(stderr, "pgaccel: shutdown queue flush failed: unknown C++ exception\n");
    }
    if (g_ooo_queue != nullptr) {
      delete g_ooo_queue;
      g_ooo_queue = nullptr;
    }
    if (g_queue != nullptr) {
      delete g_queue;
      g_queue = nullptr;
    }
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
#if defined(__APPLE__)
  // macOS Sequoia+ aborts forked processes that initialize Objective-C
  // frameworks (like Metal/SkyLight) after fork. This is the documented
  // opt-out. We deliberately do NOT initialize SYCL or touch Metal in the
  // parent: the AGX device stack caches process-local state that
  // cannot be reset on the child side, so any parent-side Metal activity
  // propagates stale IOKit handles into every forked child. Kernel
  // compilation is deferred to the first child: AdaptiveCpp's Metal
  // backend now routes MSL through `xcrun metal` in a subprocess and
  // caches the resulting .metallib on disk, so post-fork cold-starts
  // avoid MTLCompilerService entirely.
  setenv("OBJC_DISABLE_INITIALIZE_FORK_SAFETY", "YES", 0);
#endif
}
