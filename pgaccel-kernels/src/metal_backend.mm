// metal_backend.mm — Direct Metal API backend for pg_accel.
//
// Replaces AdaptiveCpp/SYCL with native Metal-cpp calls + pre-compiled
// binary archives. Works in forked PG backends because binary archives
// bypass MTLCompilerService (proven by test_fork_zero_ipc.mm).
//
// Design:
// - metal_init(): lazy per-process init (safe after fork if parent never
//   touched Metal, which is guaranteed by _PG_init not calling pgaccel_init)
// - Pipeline cache: loaded from binary archive at init, keyed by kernel name
// - Dispatch: encode → submit → waitUntilCompleted (synchronous)
// - Buffers: MTLResourceStorageModeShared (unified memory on Apple Silicon)

#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#include "metal_backend.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <unistd.h>
#include <dlfcn.h>
#include <string>
#include <unordered_map>
#include <mutex>

// ---------------------------------------------------------------------------
// Module state
// ---------------------------------------------------------------------------

static id<MTLDevice> g_device = nil;
static id<MTLCommandQueue> g_queue = nil;
static id<MTLLibrary> g_library = nil;
static std::unordered_map<std::string, id<MTLComputePipelineState>> g_pipelines;
static bool g_metal_initialized = false;
static std::once_flag g_init_flag;

// Device info cache
static metal_device_info g_device_info = {};

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Find the metallib file. Search order:
/// 1. PGACCEL_METALLIB_PATH env var
/// 2. Next to the shared library (.dylib/.so)
/// 3. Next to the pgaccel_gpu_worker binary (legacy)
static NSString* find_metallib_path() {
    // 1. Environment variable
    const char* env_path = getenv("PGACCEL_METALLIB_PATH");
    if (env_path) {
        NSString* p = [NSString stringWithUTF8String:env_path];
        if ([[NSFileManager defaultManager] fileExistsAtPath:p]) return p;
    }

    // 2. Next to this shared library
    Dl_info dl_info;
    if (dladdr((void*)find_metallib_path, &dl_info) && dl_info.dli_fname) {
        NSString* libDir = [[NSString stringWithUTF8String:dl_info.dli_fname]
                            stringByDeletingLastPathComponent];
        NSString* p = [libDir stringByAppendingPathComponent:@"pgaccel_kernels.metallib"];
        if ([[NSFileManager defaultManager] fileExistsAtPath:p]) return p;
    }

    // 3. Common install locations
    NSArray* searchPaths = @[
        @"/usr/local/lib/pgaccel_kernels.metallib",
        @"/usr/lib/pgaccel_kernels.metallib",
    ];
    for (NSString* p in searchPaths) {
        if ([[NSFileManager defaultManager] fileExistsAtPath:p]) return p;
    }

    return nil;
}

static NSString* find_archive_path() {
    const char* env_path = getenv("PGACCEL_ARCHIVE_PATH");
    if (env_path) {
        NSString* p = [NSString stringWithUTF8String:env_path];
        if ([[NSFileManager defaultManager] fileExistsAtPath:p]) return p;
    }

    Dl_info dl_info;
    if (dladdr((void*)find_archive_path, &dl_info) && dl_info.dli_fname) {
        NSString* libDir = [[NSString stringWithUTF8String:dl_info.dli_fname]
                            stringByDeletingLastPathComponent];
        NSString* p = [libDir stringByAppendingPathComponent:@"pgaccel_kernels.metallib-archive"];
        if ([[NSFileManager defaultManager] fileExistsAtPath:p]) return p;
    }

    return nil;
}

// ---------------------------------------------------------------------------
// Pipeline creation
// ---------------------------------------------------------------------------

static id<MTLComputePipelineState> create_pipeline(
    const char* name, id<MTLBinaryArchive> archive)
{
    id<MTLFunction> func = [g_library newFunctionWithName:
        [NSString stringWithUTF8String:name]];
    if (!func) {
        fprintf(stderr, "pgaccel metal: function '%s' not found in metallib\n", name);
        return nil;
    }

    NSError* error = nil;
    id<MTLComputePipelineState> pipeline = nil;

    if (archive) {
        // Use binary archive to skip compilation (works after fork)
        MTLComputePipelineDescriptor* desc =
            [[MTLComputePipelineDescriptor alloc] init];
        desc.computeFunction = func;
        desc.label = [NSString stringWithUTF8String:name];
        desc.binaryArchives = @[archive];

        pipeline = [g_device newComputePipelineStateWithDescriptor:desc
                     options:0 reflection:nil error:&error];
    }

    if (!pipeline) {
        // Fallback: JIT compile (works in non-forked contexts like tests)
        pipeline = [g_device newComputePipelineStateWithFunction:func
                     error:&error];
    }

    if (!pipeline) {
        fprintf(stderr, "pgaccel metal: pipeline creation failed for '%s': %s\n",
                name, [[error localizedDescription] UTF8String]);
    }
    return pipeline;
}

// ---------------------------------------------------------------------------
// All kernel names we need pipelines for
// ---------------------------------------------------------------------------

static const char* KERNEL_NAMES[] = {
    // reduce
    "reduce_sum_f32",
    "reduce_min_f32",
    "reduce_max_f32",
    "reduce_sum_i64",
    "reduce_count",
    "reduce_multi_f32",
    "reduce_multi_i64",
    // sort (Phase 1b)
    // "bitonic_sort_step_f32",
    // "bitonic_sort_step_i64",
    // spatial (Phase 1c)
    // "point_in_ring_f32",
    // h3 (Phase 1d)
    // "h3_lat_lng_to_cell",
    nullptr
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

static metal_status metal_init_inner() {
    @autoreleasepool {
        // 1. Create device
        g_device = MTLCreateSystemDefaultDevice();
        if (!g_device) {
            fprintf(stderr, "pgaccel metal: no Metal device found\n");
            return METAL_ERROR_NO_DEVICE;
        }

        // 2. Create command queue
        g_queue = [g_device newCommandQueue];
        if (!g_queue) {
            fprintf(stderr, "pgaccel metal: command queue creation failed\n");
            return METAL_ERROR_INIT;
        }

        // 3. Load metallib
        NSString* metallibPath = find_metallib_path();
        if (!metallibPath) {
            fprintf(stderr, "pgaccel metal: metallib not found. "
                    "Set PGACCEL_METALLIB_PATH or run 'just gpu-build'\n");
            return METAL_ERROR_INIT;
        }

        NSError* error = nil;
        NSURL* libUrl = [NSURL fileURLWithPath:metallibPath];
        g_library = [g_device newLibraryWithURL:libUrl error:&error];
        if (!g_library) {
            fprintf(stderr, "pgaccel metal: metallib load failed: %s\n",
                    [[error localizedDescription] UTF8String]);
        }

        // 4. Load binary archive (optional, needed after fork)
        id<MTLBinaryArchive> archive = nil;
        NSString* archivePath = find_archive_path();
        if (archivePath) {
            MTLBinaryArchiveDescriptor* archiveDesc =
                [[MTLBinaryArchiveDescriptor alloc] init];
            archiveDesc.url = [NSURL fileURLWithPath:archivePath];
            archive = [g_device newBinaryArchiveWithDescriptor:archiveDesc
                        error:&error];
            if (!archive) {
                fprintf(stderr, "pgaccel metal: binary archive load failed: %s "
                        "(will try JIT)\n",
                        [[error localizedDescription] UTF8String]);
            }
        }

        if (!g_library && !archive) {
            fprintf(stderr, "pgaccel metal: no metallib or archive available\n");
            return METAL_ERROR_INIT;
        }

        // 5. Create pipelines for all kernels
        for (int i = 0; KERNEL_NAMES[i] != nullptr; ++i) {
            id<MTLComputePipelineState> pipeline =
                create_pipeline(KERNEL_NAMES[i], archive);
            if (pipeline) {
                g_pipelines[KERNEL_NAMES[i]] = pipeline;
            } else {
                fprintf(stderr, "pgaccel metal: WARNING: pipeline '%s' "
                        "not available\n", KERNEL_NAMES[i]);
            }
        }

        // 6. Populate device info
        const char* name = [[g_device name] UTF8String];
        strncpy(g_device_info.device_name, name,
                sizeof(g_device_info.device_name) - 1);
        strncpy(g_device_info.backend_name, "metal",
                sizeof(g_device_info.backend_name) - 1);
        g_device_info.compute_units = 0; // Metal doesn't expose CU count
        g_device_info.max_alloc_bytes = [g_device maxBufferLength];
        g_device_info.has_fp64 = false;  // Metal has no fp64
        g_device_info.is_unified_memory =
            [g_device hasUnifiedMemory] ? true : false;

        g_metal_initialized = true;

        fprintf(stderr, "pgaccel metal: initialized [%s] unified=%d "
                "pipelines=%zu\n",
                g_device_info.device_name,
                g_device_info.is_unified_memory,
                g_pipelines.size());
        return METAL_OK;
    }
}

static metal_status g_init_result = METAL_ERROR;

extern "C" metal_status metal_init(void) {
    std::call_once(g_init_flag, []() {
        g_init_result = metal_init_inner();
    });
    return g_init_result;
}

extern "C" bool metal_is_initialized(void) {
    return g_metal_initialized;
}

extern "C" metal_device_info metal_get_device_info(void) {
    return g_device_info;
}

extern "C" void metal_shutdown(void) {
    g_pipelines.clear();
    g_library = nil;
    g_queue = nil;
    g_device = nil;
    g_metal_initialized = false;
}

// ---------------------------------------------------------------------------
// Dispatch helpers
// ---------------------------------------------------------------------------

id<MTLComputePipelineState> metal_get_pipeline(const char* name) {
    auto it = g_pipelines.find(name);
    if (it == g_pipelines.end()) return nil;
    return it->second;
}

/// Synchronous compute dispatch. Returns status.
static metal_status dispatch_sync(
    id<MTLComputePipelineState> pipeline,
    void (^encode)(id<MTLComputeCommandEncoder>),
    uint32_t total_threads)
{
    if (!pipeline || !g_queue) return METAL_ERROR_NO_DEVICE;

    @autoreleasepool {
        id<MTLCommandBuffer> cmdBuf = [g_queue commandBuffer];
        if (!cmdBuf) return METAL_ERROR_INIT;

        id<MTLComputeCommandEncoder> enc = [cmdBuf computeCommandEncoder];
        if (!enc) return METAL_ERROR_INIT;

        [enc setComputePipelineState:pipeline];
        encode(enc);

        uint32_t tg_size = MIN((uint32_t)[pipeline maxTotalThreadsPerThreadgroup],
                               (uint32_t)256);
        uint32_t num_tg = (total_threads + tg_size - 1) / tg_size;

        [enc dispatchThreadgroups:MTLSizeMake(num_tg, 1, 1)
            threadsPerThreadgroup:MTLSizeMake(tg_size, 1, 1)];
        [enc endEncoding];

        [cmdBuf commit];
        [cmdBuf waitUntilCompleted];

        if ([cmdBuf error]) {
            fprintf(stderr, "pgaccel metal: dispatch error: %s\n",
                    [[[cmdBuf error] localizedDescription] UTF8String]);
            return METAL_ERROR;
        }
    }
    return METAL_OK;
}

// ---------------------------------------------------------------------------
// Reduce dispatch functions
// ---------------------------------------------------------------------------

extern "C" metal_status metal_reduce_sum_f32(
    const float* data, size_t count, float* result)
{
    if (!result) return METAL_ERROR;
    if (count == 0) { *result = 0.0f; return METAL_OK; }
    if (!data) return METAL_ERROR;
    if (count == 1) { *result = data[0]; return METAL_OK; }

    id<MTLComputePipelineState> pipeline = metal_get_pipeline("reduce_sum_f32");
    if (!pipeline) return METAL_ERROR_NO_DEVICE;

    uint32_t n = (uint32_t)count;
    uint32_t num_groups = (n + 255) / 256;

    // Unified memory: shared buffers accessible by both CPU and GPU
    id<MTLBuffer> input_buf = [g_device newBufferWithBytes:data
                                length:count * sizeof(float)
                                options:MTLResourceStorageModeShared];
    id<MTLBuffer> partial_buf = [g_device newBufferWithLength:num_groups * sizeof(float)
                                  options:MTLResourceStorageModeShared];
    id<MTLBuffer> count_buf = [g_device newBufferWithBytes:&n
                                length:sizeof(uint32_t)
                                options:MTLResourceStorageModeShared];

    if (!input_buf || !partial_buf || !count_buf) return METAL_ERROR_OOM;

    metal_status st = dispatch_sync(pipeline,
        ^(id<MTLComputeCommandEncoder> enc) {
            [enc setBuffer:input_buf offset:0 atIndex:0];
            [enc setBuffer:partial_buf offset:0 atIndex:1];
            [enc setBuffer:count_buf offset:0 atIndex:2];
        }, n);

    if (st != METAL_OK) return st;

    // Sum partials on CPU
    float* partials = (float*)[partial_buf contents];
    float sum = 0.0f;
    for (uint32_t i = 0; i < num_groups; ++i) sum += partials[i];
    *result = sum;
    return METAL_OK;
}

extern "C" metal_status metal_reduce_min_f32(
    const float* data, size_t count, float* result)
{
    if (!result) return METAL_ERROR;
    if (count == 0) return METAL_ERROR;
    if (!data) return METAL_ERROR;
    if (count == 1) { *result = data[0]; return METAL_OK; }

    id<MTLComputePipelineState> pipeline = metal_get_pipeline("reduce_min_f32");
    if (!pipeline) return METAL_ERROR_NO_DEVICE;

    uint32_t n = (uint32_t)count;
    uint32_t num_groups = (n + 255) / 256;

    id<MTLBuffer> input_buf = [g_device newBufferWithBytes:data
                                length:count * sizeof(float)
                                options:MTLResourceStorageModeShared];
    id<MTLBuffer> partial_buf = [g_device newBufferWithLength:num_groups * sizeof(float)
                                  options:MTLResourceStorageModeShared];
    id<MTLBuffer> count_buf = [g_device newBufferWithBytes:&n
                                length:sizeof(uint32_t)
                                options:MTLResourceStorageModeShared];

    if (!input_buf || !partial_buf || !count_buf) return METAL_ERROR_OOM;

    metal_status st = dispatch_sync(pipeline,
        ^(id<MTLComputeCommandEncoder> enc) {
            [enc setBuffer:input_buf offset:0 atIndex:0];
            [enc setBuffer:partial_buf offset:0 atIndex:1];
            [enc setBuffer:count_buf offset:0 atIndex:2];
        }, n);

    if (st != METAL_OK) return st;

    float* partials = (float*)[partial_buf contents];
    float val = partials[0];
    for (uint32_t i = 1; i < num_groups; ++i)
        val = (partials[i] < val) ? partials[i] : val;
    *result = val;
    return METAL_OK;
}

extern "C" metal_status metal_reduce_max_f32(
    const float* data, size_t count, float* result)
{
    if (!result) return METAL_ERROR;
    if (count == 0) return METAL_ERROR;
    if (!data) return METAL_ERROR;
    if (count == 1) { *result = data[0]; return METAL_OK; }

    id<MTLComputePipelineState> pipeline = metal_get_pipeline("reduce_max_f32");
    if (!pipeline) return METAL_ERROR_NO_DEVICE;

    uint32_t n = (uint32_t)count;
    uint32_t num_groups = (n + 255) / 256;

    id<MTLBuffer> input_buf = [g_device newBufferWithBytes:data
                                length:count * sizeof(float)
                                options:MTLResourceStorageModeShared];
    id<MTLBuffer> partial_buf = [g_device newBufferWithLength:num_groups * sizeof(float)
                                  options:MTLResourceStorageModeShared];
    id<MTLBuffer> count_buf = [g_device newBufferWithBytes:&n
                                length:sizeof(uint32_t)
                                options:MTLResourceStorageModeShared];

    if (!input_buf || !partial_buf || !count_buf) return METAL_ERROR_OOM;

    metal_status st = dispatch_sync(pipeline,
        ^(id<MTLComputeCommandEncoder> enc) {
            [enc setBuffer:input_buf offset:0 atIndex:0];
            [enc setBuffer:partial_buf offset:0 atIndex:1];
            [enc setBuffer:count_buf offset:0 atIndex:2];
        }, n);

    if (st != METAL_OK) return st;

    float* partials = (float*)[partial_buf contents];
    float val = partials[0];
    for (uint32_t i = 1; i < num_groups; ++i)
        val = (partials[i] > val) ? partials[i] : val;
    *result = val;
    return METAL_OK;
}

extern "C" metal_status metal_reduce_sum_i64(
    const int64_t* data, size_t count, int64_t* result)
{
    if (!result) return METAL_ERROR;
    if (count == 0) { *result = 0; return METAL_OK; }
    if (!data) return METAL_ERROR;
    if (count == 1) { *result = data[0]; return METAL_OK; }

    id<MTLComputePipelineState> pipeline = metal_get_pipeline("reduce_sum_i64");
    if (!pipeline) return METAL_ERROR_NO_DEVICE;

    uint32_t n = (uint32_t)count;
    uint32_t num_groups = (n + 255) / 256;

    id<MTLBuffer> input_buf = [g_device newBufferWithBytes:data
                                length:count * sizeof(int64_t)
                                options:MTLResourceStorageModeShared];
    id<MTLBuffer> partial_buf = [g_device newBufferWithLength:num_groups * sizeof(int64_t)
                                  options:MTLResourceStorageModeShared];
    id<MTLBuffer> count_buf = [g_device newBufferWithBytes:&n
                                length:sizeof(uint32_t)
                                options:MTLResourceStorageModeShared];

    if (!input_buf || !partial_buf || !count_buf) return METAL_ERROR_OOM;

    metal_status st = dispatch_sync(pipeline,
        ^(id<MTLComputeCommandEncoder> enc) {
            [enc setBuffer:input_buf offset:0 atIndex:0];
            [enc setBuffer:partial_buf offset:0 atIndex:1];
            [enc setBuffer:count_buf offset:0 atIndex:2];
        }, n);

    if (st != METAL_OK) return st;

    int64_t* partials = (int64_t*)[partial_buf contents];
    int64_t sum = 0;
    for (uint32_t i = 0; i < num_groups; ++i) sum += partials[i];
    *result = sum;
    return METAL_OK;
}

extern "C" metal_status metal_reduce_count(
    const uint8_t* mask, size_t count, size_t* result)
{
    if (!result) return METAL_ERROR;
    if (count == 0) { *result = 0; return METAL_OK; }
    if (!mask) return METAL_ERROR;

    id<MTLComputePipelineState> pipeline = metal_get_pipeline("reduce_count");
    if (!pipeline) return METAL_ERROR_NO_DEVICE;

    uint32_t n = (uint32_t)count;
    uint32_t num_groups = (n + 255) / 256;

    id<MTLBuffer> input_buf = [g_device newBufferWithBytes:mask
                                length:count * sizeof(uint8_t)
                                options:MTLResourceStorageModeShared];
    id<MTLBuffer> partial_buf = [g_device newBufferWithLength:num_groups * sizeof(uint32_t)
                                  options:MTLResourceStorageModeShared];
    id<MTLBuffer> count_buf = [g_device newBufferWithBytes:&n
                                length:sizeof(uint32_t)
                                options:MTLResourceStorageModeShared];

    if (!input_buf || !partial_buf || !count_buf) return METAL_ERROR_OOM;

    metal_status st = dispatch_sync(pipeline,
        ^(id<MTLComputeCommandEncoder> enc) {
            [enc setBuffer:input_buf offset:0 atIndex:0];
            [enc setBuffer:partial_buf offset:0 atIndex:1];
            [enc setBuffer:count_buf offset:0 atIndex:2];
        }, n);

    if (st != METAL_OK) return st;

    uint32_t* partials = (uint32_t*)[partial_buf contents];
    size_t total = 0;
    for (uint32_t i = 0; i < num_groups; ++i) total += partials[i];
    *result = total;
    return METAL_OK;
}

extern "C" metal_status metal_reduce_multi_f32(
    const float* data, size_t count,
    float* out_sum, float* out_min, float* out_max, int64_t* out_count)
{
    if (!out_sum || !out_min || !out_max || !out_count) return METAL_ERROR;
    if (count == 0) {
        *out_sum = 0.0f; *out_min = 0.0f; *out_max = 0.0f; *out_count = 0;
        return METAL_OK;
    }
    if (!data) return METAL_ERROR;

    id<MTLComputePipelineState> pipeline = metal_get_pipeline("reduce_multi_f32");
    if (!pipeline) return METAL_ERROR_NO_DEVICE;

    uint32_t n = (uint32_t)count;
    uint32_t num_groups = (n + 255) / 256;

    id<MTLBuffer> input_buf = [g_device newBufferWithBytes:data
                                length:count * sizeof(float)
                                options:MTLResourceStorageModeShared];
    id<MTLBuffer> psum_buf = [g_device newBufferWithLength:num_groups * sizeof(float)
                               options:MTLResourceStorageModeShared];
    id<MTLBuffer> pmin_buf = [g_device newBufferWithLength:num_groups * sizeof(float)
                               options:MTLResourceStorageModeShared];
    id<MTLBuffer> pmax_buf = [g_device newBufferWithLength:num_groups * sizeof(float)
                               options:MTLResourceStorageModeShared];
    id<MTLBuffer> count_buf = [g_device newBufferWithBytes:&n
                                length:sizeof(uint32_t)
                                options:MTLResourceStorageModeShared];

    if (!input_buf || !psum_buf || !pmin_buf || !pmax_buf || !count_buf)
        return METAL_ERROR_OOM;

    metal_status st = dispatch_sync(pipeline,
        ^(id<MTLComputeCommandEncoder> enc) {
            [enc setBuffer:input_buf offset:0 atIndex:0];
            [enc setBuffer:psum_buf offset:0 atIndex:1];
            [enc setBuffer:pmin_buf offset:0 atIndex:2];
            [enc setBuffer:pmax_buf offset:0 atIndex:3];
            [enc setBuffer:count_buf offset:0 atIndex:4];
        }, n);

    if (st != METAL_OK) return st;

    float* ps = (float*)[psum_buf contents];
    float* pm = (float*)[pmin_buf contents];
    float* px = (float*)[pmax_buf contents];

    float sum = 0.0f, mn = ps[0], mx = ps[0];
    // Use min/max partials, not sum partials, for min/max init
    mn = pm[0]; mx = px[0];
    for (uint32_t i = 0; i < num_groups; ++i) {
        sum += ps[i];
        if (pm[i] < mn) mn = pm[i];
        if (px[i] > mx) mx = px[i];
    }
    *out_sum = sum;
    *out_min = mn;
    *out_max = mx;
    *out_count = (int64_t)count;
    return METAL_OK;
}

extern "C" metal_status metal_reduce_multi_i64(
    const int64_t* data, size_t count,
    int64_t* out_sum, int64_t* out_min, int64_t* out_max, int64_t* out_count)
{
    if (!out_sum || !out_min || !out_max || !out_count) return METAL_ERROR;
    if (count == 0) {
        *out_sum = 0; *out_min = 0; *out_max = 0; *out_count = 0;
        return METAL_OK;
    }
    if (!data) return METAL_ERROR;

    id<MTLComputePipelineState> pipeline = metal_get_pipeline("reduce_multi_i64");
    if (!pipeline) return METAL_ERROR_NO_DEVICE;

    uint32_t n = (uint32_t)count;
    uint32_t num_groups = (n + 255) / 256;

    id<MTLBuffer> input_buf = [g_device newBufferWithBytes:data
                                length:count * sizeof(int64_t)
                                options:MTLResourceStorageModeShared];
    id<MTLBuffer> psum_buf = [g_device newBufferWithLength:num_groups * sizeof(int64_t)
                               options:MTLResourceStorageModeShared];
    id<MTLBuffer> pmin_buf = [g_device newBufferWithLength:num_groups * sizeof(int64_t)
                               options:MTLResourceStorageModeShared];
    id<MTLBuffer> pmax_buf = [g_device newBufferWithLength:num_groups * sizeof(int64_t)
                               options:MTLResourceStorageModeShared];
    id<MTLBuffer> count_buf = [g_device newBufferWithBytes:&n
                                length:sizeof(uint32_t)
                                options:MTLResourceStorageModeShared];

    if (!input_buf || !psum_buf || !pmin_buf || !pmax_buf || !count_buf)
        return METAL_ERROR_OOM;

    metal_status st = dispatch_sync(pipeline,
        ^(id<MTLComputeCommandEncoder> enc) {
            [enc setBuffer:input_buf offset:0 atIndex:0];
            [enc setBuffer:psum_buf offset:0 atIndex:1];
            [enc setBuffer:pmin_buf offset:0 atIndex:2];
            [enc setBuffer:pmax_buf offset:0 atIndex:3];
            [enc setBuffer:count_buf offset:0 atIndex:4];
        }, n);

    if (st != METAL_OK) return st;

    int64_t* ps = (int64_t*)[psum_buf contents];
    int64_t* pm = (int64_t*)[pmin_buf contents];
    int64_t* px = (int64_t*)[pmax_buf contents];

    int64_t sum = 0, mn = pm[0], mx = px[0];
    for (uint32_t i = 0; i < num_groups; ++i) {
        sum += ps[i];
        if (pm[i] < mn) mn = pm[i];
        if (px[i] > mx) mx = px[i];
    }
    *out_sum = sum;
    *out_min = mn;
    *out_max = mx;
    *out_count = (int64_t)count;
    return METAL_OK;
}
