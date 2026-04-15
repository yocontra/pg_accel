// test_fork_metallib: Tests loading a PRE-COMPILED .metallib after fork.
//
// If this works, we can eliminate ALL IPC:
// - Pre-compile all GPU kernels to .metallib at build time
// - Load them in forked PG backends (no MTLCompilerService needed)
// - Run kernels directly in PG parallel workers
// - ZERO IPC, ZERO shared memory, ZERO pipes

#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <sys/wait.h>
#include <unistd.h>

static int run_gpu_in_child(const char* metallib_path) {
    printf("\n--- Child PID %d: Testing pre-compiled metallib after fork ---\n", getpid());
    fflush(stdout);

    // Step 1: Create device
    id<MTLDevice> device = MTLCreateSystemDefaultDevice();
    if (!device) {
        fprintf(stderr, "Child: MTLCreateSystemDefaultDevice FAILED\n");
        return 1;
    }
    printf("Child: Device = %s\n", [[device name] UTF8String]);
    fflush(stdout);

    // Step 2: Load pre-compiled metallib (NO MTLCompilerService needed)
    NSError* error = nil;
    NSURL* url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:metallib_path]];
    id<MTLLibrary> lib = [device newLibraryWithURL:url error:&error];
    if (!lib) {
        fprintf(stderr, "Child: newLibraryWithURL FAILED: %s\n",
                [[error localizedDescription] UTF8String]);
        return 2;
    }
    printf("Child: Loaded pre-compiled metallib OK!\n");
    fflush(stdout);

    // Step 3: Get kernel function
    id<MTLFunction> func = [lib newFunctionWithName:@"reduce_sum_f32"];
    if (!func) {
        fprintf(stderr, "Child: Function 'reduce_sum_f32' not found\n");
        return 3;
    }
    printf("Child: Got kernel function OK\n");
    fflush(stdout);

    // Step 4: Create compute pipeline
    id<MTLComputePipelineState> pipeline =
        [device newComputePipelineStateWithFunction:func error:&error];
    if (!pipeline) {
        fprintf(stderr, "Child: Pipeline creation FAILED: %s\n",
                [[error localizedDescription] UTF8String]);
        return 4;
    }
    printf("Child: Compute pipeline created OK\n");
    fflush(stdout);

    // Step 5: Allocate buffers and run kernel
    const uint32_t N = 100000;
    const float VAL = 7.0f;
    const float EXPECTED = (float)N * VAL;

    float* host_data = (float*)malloc(N * sizeof(float));
    for (uint32_t i = 0; i < N; i++) host_data[i] = VAL;

    id<MTLBuffer> input_buf = [device newBufferWithBytes:host_data
                                length:N * sizeof(float)
                                options:MTLResourceStorageModeShared];
    // Output: single atomic float, initialized to 0
    float zero = 0.0f;
    id<MTLBuffer> output_buf = [device newBufferWithBytes:&zero
                                 length:sizeof(float)
                                 options:MTLResourceStorageModeShared];
    // Count buffer
    id<MTLBuffer> count_buf = [device newBufferWithBytes:&N
                                length:sizeof(uint32_t)
                                options:MTLResourceStorageModeShared];

    if (!input_buf || !output_buf || !count_buf) {
        fprintf(stderr, "Child: Buffer allocation failed\n");
        free(host_data);
        return 5;
    }
    printf("Child: Buffers allocated OK (%u elements, %zu bytes)\n",
           N, (size_t)N * sizeof(float));
    fflush(stdout);

    // Step 6: Encode and submit compute command
    id<MTLCommandQueue> queue = [device newCommandQueue];
    if (!queue) {
        fprintf(stderr, "Child: Command queue creation FAILED\n");
        free(host_data);
        return 6;
    }

    // Dispatch in threadgroups of 1024
    uint32_t threadgroup_size = MIN((uint32_t)[pipeline maxTotalThreadsPerThreadgroup], 1024u);
    // Round up to cover all elements
    uint32_t num_threadgroups = (N + threadgroup_size - 1) / threadgroup_size;

    id<MTLCommandBuffer> cmdBuf = [queue commandBuffer];
    id<MTLComputeCommandEncoder> encoder = [cmdBuf computeCommandEncoder];
    [encoder setComputePipelineState:pipeline];
    [encoder setBuffer:input_buf offset:0 atIndex:0];
    [encoder setBuffer:output_buf offset:0 atIndex:1];
    [encoder setBuffer:count_buf offset:0 atIndex:2];
    [encoder dispatchThreadgroups:MTLSizeMake(num_threadgroups, 1, 1)
            threadsPerThreadgroup:MTLSizeMake(threadgroup_size, 1, 1)];
    [encoder endEncoding];

    printf("Child: Dispatching kernel (%u threadgroups x %u threads)...\n",
           num_threadgroups, threadgroup_size);
    fflush(stdout);

    [cmdBuf commit];
    [cmdBuf waitUntilCompleted];

    if ([cmdBuf error]) {
        fprintf(stderr, "Child: Command buffer error: %s\n",
                [[[cmdBuf error] localizedDescription] UTF8String]);
        free(host_data);
        return 7;
    }

    float result = *(float*)[output_buf contents];
    printf("Child: Kernel result = %f (expected %f)\n", result, EXPECTED);
    fflush(stdout);

    free(host_data);

    // Allow some floating point imprecision from atomic adds
    if (fabsf(result - EXPECTED) < EXPECTED * 0.01f) {
        printf("\n=== CHILD RESULT: SUCCESS ===\n");
        printf("Pre-compiled Metal kernels work after fork!\n");
        printf("Device: OK | Buffers: OK | Metallib load: OK | Pipeline: OK | Kernel: OK\n");
        printf("→ Metal binary archives work directly in forked PG backends.\n");
        printf("→ GPU kernels can run DIRECTLY in PG parallel workers.\n");
        fflush(stdout);
        return 0;
    } else {
        fprintf(stderr, "Child: Wrong result (delta=%f)\n", fabsf(result - EXPECTED));
        return 8;
    }
}

int main(int argc, char** argv) {
    // Resolve metallib path relative to executable
    const char* metallib_path = "reduce_sum.metallib";
    if (argc > 1) metallib_path = argv[1];

    printf("=== Pre-compiled Metallib Fork Test ===\n");
    printf("Parent PID: %d\n", getpid());
    printf("Metallib: %s\n", metallib_path);
    printf("Parent does NOT touch Metal — clean fork.\n\n");
    fflush(stdout);

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 1; }

    if (pid == 0) {
        @autoreleasepool {
            int rc = run_gpu_in_child(metallib_path);
            fflush(stdout);
            fflush(stderr);
            _exit(rc);
        }
    }

    int wstatus = 0;
    waitpid(pid, &wstatus, 0);

    printf("\n=== Final Result ===\n");
    if (WIFEXITED(wstatus)) {
        int rc = WEXITSTATUS(wstatus);
        if (rc == 0) {
            printf("PASS: Pre-compiled Metal kernels work after fork.\n");
            printf("ZERO IPC architecture is FEASIBLE.\n");
        } else {
            printf("FAIL: exit code %d\n", rc);
        }
        return rc;
    } else if (WIFSIGNALED(wstatus)) {
        printf("CRASH: signal %d\n", WTERMSIG(wstatus));
        return 1;
    }
    return 1;
}
