// USM bump-arena allocator — demolished.
//
// The arena (pgaccel_alloc / pgaccel_free / pgaccel_pool_bytes_used /
// pgaccel_prefetch) had zero kernel callers: kernels allocate device memory
// through the templated pgaccel_alloc<T>() / pgaccel_alloc_input<T>() helpers
// in include/alloc_helper.h, not through this extern "C" arena. The arena's
// per-process bookkeeping (blocks/oversized vectors) and its PID fork guard in
// ensure_pool_initialized() were therefore never exercised — the real
// fork-safe queue re-check lives in device_manager.cpp (pgaccel_init/getpid).
//
// Only pgaccel_pool_reset() is retained: pgaccel_shutdown() in
// device_manager.cpp still invokes it during teardown. With the arena gone it
// has nothing to free, so it is a genuine no-op.

extern "C" void pgaccel_pool_reset(void) {}
