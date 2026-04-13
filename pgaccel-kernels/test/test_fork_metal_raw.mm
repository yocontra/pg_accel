// test_fork_metal_raw: Tests raw Metal API after fork (no AdaptiveCpp).
//
// Tests whether we can:
// 1. Create MTLDevice after fork (already proven: YES)
// 2. Allocate Metal buffers after fork
// 3. Load pre-compiled .metallib after fork (bypasses MTLCompilerService)
// 4. Run a compute kernel after fork
//
// If pre-compiled metallib loading works, we can eliminate IPC by:
// - Pre-compiling all SYCL kernels to .metallib at build time
// - Loading them in forked PG backends without needing MTLCompilerService

#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <sys/wait.h>
#include <unistd.h>

// Simple reduce_sum kernel in MSL
static const char* REDUCE_SUM_MSL = R"(
#include <metal_stdlib>
using namespace metal;

kernel void reduce_sum(
    device const float* input [[buffer(0)]],
    device float* output [[buffer(1)]],
    uint tid [[thread_position_in_grid]],
    uint threads [[threads_per_grid]])
{
    // Simple parallel reduction: each thread writes its element,
    // then thread 0 sums (naive but tests the pipeline)
    threadgroup float shared[1024];
    uint lid = tid % 1024;
    shared[lid] = (tid < *reinterpret_cast<device const uint*>(output + 1)) ? input[tid] : 0.0f;
    threadgroup_barrier(mem_flags::mem_threadgroup);

    // Tree reduction in threadgroup
    for (uint s = 512; s > 0; s >>= 1) {
        if (lid < s) {
            shared[lid] += shared[lid + s];
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
    if (lid == 0) {
        atomic_fetch_add_explicit(
            reinterpret_cast<device atomic_uint*>(output),
            as_type<uint>(shared[0]),
            memory_order_relaxed);
    }
}
)";

// Pre-compiled metallib approach: compile MSL to metallib data BEFORE fork
static NSData* precompile_metallib(id<MTLDevice> device) {
    NSError* error = nil;
    MTLCompileOptions* opts = [[MTLCompileOptions alloc] init];
    id<MTLLibrary> lib = [device newLibraryWithSource:
        [NSString stringWithUTF8String:REDUCE_SUM_MSL]
        options:opts error:&error];
    if (!lib) {
        fprintf(stderr, "Pre-compile failed: %s\n",
                [[error localizedDescription] UTF8String]);
        return nil;
    }
    printf("Pre-compiled Metal library OK\n");
    // We can't easily serialize MTLLibrary to data, but we can test
    // if newLibraryWithSource works. The real test is whether it works
    // post-fork.
    return nil;  // placeholder
}

static int run_in_child_with_jit() {
    printf("\n--- Child: Testing raw Metal API after fork ---\n");

    // Step 1: Create device
    id<MTLDevice> device = MTLCreateSystemDefaultDevice();
    if (!device) {
        fprintf(stderr, "Child: MTLCreateSystemDefaultDevice FAILED\n");
        return 1;
    }
    printf("Child: Device = %s\n", [[device name] UTF8String]);

    // Step 2: Allocate buffers
    const size_t N = 1024;
    const float VAL = 3.0f;
    float* host_data = (float*)malloc(N * sizeof(float));
    for (size_t i = 0; i < N; i++) host_data[i] = VAL;

    id<MTLBuffer> input_buf = [device newBufferWithBytes:host_data
                                length:N * sizeof(float)
                                options:MTLResourceStorageModeShared];
    if (!input_buf) {
        fprintf(stderr, "Child: Buffer allocation FAILED\n");
        free(host_data);
        return 2;
    }
    printf("Child: Buffer allocation OK (%zu bytes)\n", N * sizeof(float));

    // Output buffer: first float is result, second uint is count
    float output_init[2] = {0.0f, 0.0f};
    memcpy(&output_init[1], &N, sizeof(uint32_t));  // store count as float bits
    id<MTLBuffer> output_buf = [device newBufferWithBytes:output_init
                                 length:2 * sizeof(float)
                                 options:MTLResourceStorageModeShared];
    if (!output_buf) {
        fprintf(stderr, "Child: Output buffer allocation FAILED\n");
        free(host_data);
        return 2;
    }
    printf("Child: Output buffer OK\n");

    // Step 3: Try JIT compile (expected to FAIL due to MTLCompilerService)
    NSError* error = nil;
    MTLCompileOptions* opts = [[MTLCompileOptions alloc] init];
    id<MTLLibrary> lib = [device newLibraryWithSource:
        [NSString stringWithUTF8String:REDUCE_SUM_MSL]
        options:opts error:&error];

    if (!lib) {
        printf("Child: JIT compile FAILED (expected): %s\n",
               [[error localizedDescription] UTF8String]);
        printf("Child: This confirms MTLCompilerService is unreachable after fork.\n");
        printf("Child: Buffer alloc works, only shader JIT is broken.\n");
        free(host_data);
        return 10;  // Special code: buffers work, JIT doesn't
    }

    printf("Child: JIT compile SUCCEEDED! (unexpected — MTLCompilerService works after fork!)\n");

    // Step 4: If we got here, try running the kernel
    id<MTLFunction> func = [lib newFunctionWithName:@"reduce_sum"];
    if (!func) {
        fprintf(stderr, "Child: Function lookup failed\n");
        free(host_data);
        return 3;
    }

    id<MTLComputePipelineState> pipeline =
        [device newComputePipelineStateWithFunction:func error:&error];
    if (!pipeline) {
        fprintf(stderr, "Child: Pipeline creation failed: %s\n",
                [[error localizedDescription] UTF8String]);
        free(host_data);
        return 4;
    }

    id<MTLCommandQueue> queue = [device newCommandQueue];
    id<MTLCommandBuffer> cmdBuf = [queue commandBuffer];
    id<MTLComputeCommandEncoder> encoder = [cmdBuf computeCommandEncoder];

    [encoder setComputePipelineState:pipeline];
    [encoder setBuffer:input_buf offset:0 atIndex:0];
    [encoder setBuffer:output_buf offset:0 atIndex:1];
    [encoder dispatchThreads:MTLSizeMake(N, 1, 1)
       threadsPerThreadgroup:MTLSizeMake(MIN(N, 1024), 1, 1)];
    [encoder endEncoding];
    [cmdBuf commit];
    [cmdBuf waitUntilCompleted];

    float* result = (float*)[output_buf contents];
    float expected = N * VAL;
    printf("Child: Kernel result = %f (expected %f)\n", result[0], expected);

    free(host_data);

    if (fabsf(result[0] - expected) < 1.0f) {
        printf("\n=== RESULT: FULL SUCCESS ===\n");
        printf("Raw Metal works completely after fork: device, buffers, JIT, kernels.\n");
        return 0;
    } else {
        printf("Child: Wrong result but kernel ran\n");
        return 5;
    }
}

int main() {
    printf("=== Raw Metal Fork Test ===\n");
    printf("Parent PID: %d\n", getpid());
    printf("Parent does NOT touch Metal.\n\n");

    // DO NOT create MTLDevice or any Metal objects here.
    // This simulates PG postmaster which never touches GPU.

    printf("Forking...\n");
    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 1; }

    if (pid == 0) {
        @autoreleasepool {
            int rc = run_in_child_with_jit();
            _exit(rc);
        }
    }

    int wstatus = 0;
    waitpid(pid, &wstatus, 0);

    printf("\n=== Final Result ===\n");
    if (WIFEXITED(wstatus)) {
        int rc = WEXITSTATUS(wstatus);
        switch (rc) {
            case 0:
                printf("PASS: Full Metal works after fork (device + buffers + JIT + kernels)\n");
                printf("→ AdaptiveCpp bug: raw Metal JIT works, ACPP breaks it.\n");
                break;
            case 10:
                printf("PARTIAL: Metal device + buffers work, but JIT fails.\n");
                printf("→ Need pre-compiled .metallib to eliminate IPC.\n");
                printf("→ Or bypass AdaptiveCpp and use Metal API directly.\n");
                break;
            default:
                printf("FAIL: exit code %d\n", rc);
        }
        return rc;
    } else if (WIFSIGNALED(wstatus)) {
        printf("CRASH: signal %d\n", WTERMSIG(wstatus));
        return 1;
    }
    return 1;
}
