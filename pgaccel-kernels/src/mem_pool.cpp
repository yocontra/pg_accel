#include <sycl/sycl.hpp>

#include <cstdlib>
#include <cstring>
#include <vector>

#include "pgaccel_ffi.h"

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

static constexpr size_t DEFAULT_BLOCK_SIZE = 256 * 1024;  // 256 KB
static constexpr size_t ALIGNMENT = 16;

// ---------------------------------------------------------------------------
// Pool data structures
// ---------------------------------------------------------------------------

struct Block {
  void* data;       // USM-allocated (or malloc'd) memory
  size_t capacity;  // total bytes in block
  size_t used;      // bump pointer offset
};

struct OversizedAlloc {
  void* data;
  size_t size;
};

enum class AllocMode {
  SharedUSM,  // sycl::malloc_shared (unified memory, e.g. Apple Silicon)
  DeviceUSM,  // sycl::malloc_device (discrete GPU) + explicit prefetch
};

struct Pool {
  std::vector<Block> blocks;
  std::vector<OversizedAlloc> oversized;  // direct allocations > block_size
  size_t block_size = DEFAULT_BLOCK_SIZE;
  size_t total_allocated = 0;
  AllocMode mode = AllocMode::SharedUSM;
  bool initialized = false;
  sycl::queue* queue = nullptr;  // owned by pool, created lazily
};

static Pool g_pool;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

static size_t align_up(size_t n, size_t align) {
  return (n + align - 1) & ~(align - 1);
}

static sycl::queue& get_queue() {
  // SAFETY: only called after ensure_pool_initialized() which sets up the
  // queue. Pool is per-backend (single PG process), no thread safety needed.
  return *g_pool.queue;
}

static void* raw_alloc(size_t bytes) {
  if (!g_pool.queue) {
    // Unreachable by construction — ensure_pool_initialized() aborts if
    // the SYCL queue cannot be created. If the queue is still null here,
    // something removed the queue out from under the pool. No host-memory
    // backing: SYCL is the only supported allocator.
    return nullptr;
  }
  switch (g_pool.mode) {
    case AllocMode::SharedUSM:
      return sycl::malloc_shared(bytes, get_queue());
    case AllocMode::DeviceUSM:
      return sycl::malloc_device(bytes, get_queue());
  }
  return nullptr;
}

static void raw_free(void* ptr) {
  if (!ptr)
    return;
  if (!g_pool.queue)
    return;
  switch (g_pool.mode) {
    case AllocMode::SharedUSM:
    case AllocMode::DeviceUSM:
      sycl::free(ptr, get_queue());
      return;
  }
}

static void ensure_pool_initialized() {
  if (g_pool.initialized)
    return;
  g_pool.initialized = true;

  // Query the device manager's public API to determine platform capabilities.
  // The device manager owns the primary queue (static linkage), so we create
  // our own queue targeting the same default device for USM allocations.
  pgaccel_platform_caps caps = pgaccel_get_caps();

  try {
    // SAFETY: pgaccel_init() must have been called before any alloc.
    // We create a queue on the default device, which should match the
    // device manager's selection (highest-scored device).
    g_pool.queue = new sycl::queue{sycl::default_selector_v, sycl::property::queue::in_order{}};

    if (caps.is_unified_memory) {
      g_pool.mode = AllocMode::SharedUSM;
    } else {
      g_pool.mode = AllocMode::DeviceUSM;
    }
  } catch (...) {
    // SYCL queue creation failed. No CPU fallback: subsequent raw_alloc
    // calls will return nullptr, propagating allocation failure to callers.
    g_pool.queue = nullptr;
  }
}

static Block allocate_block(size_t capacity) {
  Block b;
  b.data = raw_alloc(capacity);
  b.capacity = (b.data != nullptr) ? capacity : 0;
  b.used = 0;
  if (b.data) {
    g_pool.total_allocated += capacity;
  }
  return b;
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

extern "C" void* pgaccel_alloc(size_t bytes) {
  if (bytes == 0)
    return nullptr;

  ensure_pool_initialized();

  size_t aligned = align_up(bytes, ALIGNMENT);

  // Over-size allocation: bypass arena, allocate directly.
  if (aligned > g_pool.block_size) {
    void* ptr = raw_alloc(aligned);
    if (ptr) {
      g_pool.oversized.push_back({ptr, aligned});
      g_pool.total_allocated += aligned;
    }
    return ptr;
  }

  // Try to bump-allocate from the current (last) block.
  if (!g_pool.blocks.empty()) {
    Block& cur = g_pool.blocks.back();
    if (cur.used + aligned <= cur.capacity) {
      // SAFETY: cur.data is non-null (checked at allocation time) and
      // cur.used + aligned <= cur.capacity guarantees we stay in bounds.
      void* ptr = static_cast<char*>(cur.data) + cur.used;
      cur.used += aligned;
      return ptr;
    }
  }

  // Need a new block.
  Block blk = allocate_block(g_pool.block_size);
  if (!blk.data)
    return nullptr;

  // SAFETY: freshly allocated block, used==0, aligned <= block_size.
  void* ptr = blk.data;
  blk.used = aligned;
  g_pool.blocks.push_back(blk);
  return ptr;
}

extern "C" void pgaccel_free(void* ptr) {
  // Arena allocator: individual frees are no-ops.
  // Memory is reclaimed in bulk via pgaccel_pool_reset().
  (void)ptr;
}

extern "C" void pgaccel_pool_reset() {
  for (auto& blk : g_pool.blocks) {
    raw_free(blk.data);
  }
  g_pool.blocks.clear();

  for (auto& alloc : g_pool.oversized) {
    raw_free(alloc.data);
  }
  g_pool.oversized.clear();

  g_pool.total_allocated = 0;
}

extern "C" size_t pgaccel_pool_bytes_used() {
  size_t used = 0;
  for (const auto& blk : g_pool.blocks) {
    used += blk.used;
  }
  for (const auto& alloc : g_pool.oversized) {
    used += alloc.size;
  }
  return used;
}

extern "C" void pgaccel_prefetch(void* ptr, size_t bytes) {
  if (!ptr || bytes == 0)
    return;

  ensure_pool_initialized();
  // Prefetch is only meaningful on discrete GPUs with device memory.
  if (g_pool.mode == AllocMode::DeviceUSM && g_pool.queue) {
    get_queue().prefetch(ptr, bytes);
  }
}
