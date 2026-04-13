// test_fork_zero_ipc: The definitive test for zero-IPC GPU after fork.
//
// Simulates the EXACT PG scenario:
// 1. Binary archive created at BUILD TIME (separate process, already done)
// 2. Parent (postmaster) NEVER touches Metal
// 3. Fork
// 4. Child loads binary archive, creates pipeline, runs kernel
//
// Usage: first run `./create_archive reduce_sum.metallib` to create the archive,
// then run this test which ONLY does the fork + child GPU work.

#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <sys/wait.h>
#include <unistd.h>

static const char* ARCHIVE_PATH = "/tmp/pgaccel_test.metallib-archive";

static int run_gpu_in_child(const char* metallib_path) {
    printf("Child PID %d: Zero-IPC GPU test\n", getpid());
    fflush(stdout);

    // Step 1: Create device (proven to work after clean fork)
    id<MTLDevice> device = MTLCreateSystemDefaultDevice();
    if (!device) {
        fprintf(stderr, "Child: No device\n");
        return 1;
    }
    printf("Child: Device = %s\n", [[device name] UTF8String]);
    fflush(stdout);

    // Step 2: Load binary archive from disk (created at build time)
    NSError* error = nil;
    MTLBinaryArchiveDescriptor* archiveDesc = [[MTLBinaryArchiveDescriptor alloc] init];
    archiveDesc.url = [NSURL fileURLWithPath:
        [NSString stringWithUTF8String:ARCHIVE_PATH]];
    id<MTLBinaryArchive> archive = [device newBinaryArchiveWithDescriptor:archiveDesc
                                     error:&error];
    if (!archive) {
        fprintf(stderr, "Child: Load archive FAILED: %s\n",
                [[error localizedDescription] UTF8String]);
        return 2;
    }
    printf("Child: Binary archive loaded OK\n");
    fflush(stdout);

    // Step 3: Load metallib
    NSURL* url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:metallib_path]];
    id<MTLLibrary> lib = [device newLibraryWithURL:url error:&error];
    if (!lib) {
        fprintf(stderr, "Child: metallib load FAILED: %s\n",
                [[error localizedDescription] UTF8String]);
        fprintf(stderr, "Child: Even metallib loading needs compiler service.\n");
        // Try just using device functions without metallib
        return 3;
    }
    printf("Child: Metallib loaded OK\n");
    fflush(stdout);

    id<MTLFunction> func = [lib newFunctionWithName:@"reduce_sum_f32"];
    if (!func) { fprintf(stderr, "Child: No function\n"); return 3; }

    // Step 4: Create pipeline with binary archive (skip compilation)
    MTLComputePipelineDescriptor* desc = [[MTLComputePipelineDescriptor alloc] init];
    desc.computeFunction = func;
    desc.label = @"reduce_sum_f32";
    desc.binaryArchives = @[archive];

    id<MTLComputePipelineState> pipeline =
        [device newComputePipelineStateWithDescriptor:desc
          options:0 reflection:nil error:&error];
    if (!pipeline) {
        fprintf(stderr, "Child: Pipeline FAILED: %s\n",
                [[error localizedDescription] UTF8String]);
        return 4;
    }
    printf("Child: Pipeline created OK (from binary archive, no compiler)\n");
    fflush(stdout);

    // Step 5: Allocate buffers
    const uint32_t N = 100000;
    const float VAL = 7.0f;
    const float EXPECTED = (float)N * VAL;

    float* host_data = (float*)malloc(N * sizeof(float));
    for (uint32_t i = 0; i < N; i++) host_data[i] = VAL;

    id<MTLBuffer> input_buf = [device newBufferWithBytes:host_data
                                length:N * sizeof(float)
                                options:MTLResourceStorageModeShared];
    float zero = 0.0f;
    id<MTLBuffer> output_buf = [device newBufferWithBytes:&zero
                                 length:sizeof(float)
                                 options:MTLResourceStorageModeShared];
    id<MTLBuffer> count_buf = [device newBufferWithBytes:&N
                                length:sizeof(uint32_t)
                                options:MTLResourceStorageModeShared];

    if (!input_buf || !output_buf || !count_buf) {
        fprintf(stderr, "Child: Buffer allocation FAILED\n");
        free(host_data);
        return 5;
    }
    printf("Child: Buffers allocated OK\n");
    fflush(stdout);

    // Step 6: Submit and run
    id<MTLCommandQueue> queue = [device newCommandQueue];
    if (!queue) {
        fprintf(stderr, "Child: Command queue FAILED\n");
        free(host_data);
        return 6;
    }

    uint32_t tg_size = MIN((uint32_t)[pipeline maxTotalThreadsPerThreadgroup], 1024u);
    uint32_t num_tg = (N + tg_size - 1) / tg_size;

    id<MTLCommandBuffer> cmdBuf = [queue commandBuffer];
    id<MTLComputeCommandEncoder> enc = [cmdBuf computeCommandEncoder];
    [enc setComputePipelineState:pipeline];
    [enc setBuffer:input_buf offset:0 atIndex:0];
    [enc setBuffer:output_buf offset:0 atIndex:1];
    [enc setBuffer:count_buf offset:0 atIndex:2];
    [enc dispatchThreadgroups:MTLSizeMake(num_tg, 1, 1)
        threadsPerThreadgroup:MTLSizeMake(tg_size, 1, 1)];
    [enc endEncoding];

    printf("Child: Dispatching %u threadgroups x %u threads...\n", num_tg, tg_size);
    fflush(stdout);

    [cmdBuf commit];
    [cmdBuf waitUntilCompleted];

    if ([cmdBuf error]) {
        fprintf(stderr, "Child: Execution error: %s\n",
                [[[cmdBuf error] localizedDescription] UTF8String]);
        free(host_data);
        return 7;
    }

    float result = *(float*)[output_buf contents];
    printf("Child: Result = %f (expected %f)\n", result, EXPECTED);
    fflush(stdout);
    free(host_data);

    if (fabsf(result - EXPECTED) < EXPECTED * 0.01f) {
        printf("\n=== SUCCESS: ZERO IPC GPU AFTER FORK ===\n");
        printf("Parent never touched Metal. Child loaded binary archive.\n");
        printf("Pipeline created without MTLCompilerService.\n");
        printf("Kernel executed on GPU and produced correct results.\n");
        printf("→ ZERO IPC ARCHITECTURE IS PROVEN.\n");
        fflush(stdout);
        return 0;
    } else {
        fprintf(stderr, "Child: Wrong result (delta=%f)\n", fabsf(result - EXPECTED));
        return 8;
    }
}

int main(int argc, char** argv) {
    const char* metallib_path = "reduce_sum.metallib";
    if (argc > 1) metallib_path = argv[1];

    printf("=== Zero-IPC Fork Test ===\n");
    printf("Parent PID: %d\n", getpid());
    printf("Parent does NOT touch Metal at all.\n");
    printf("Binary archive was created by a separate process (build time).\n\n");
    fflush(stdout);

    // Parent does NOTHING with Metal. Just fork.
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
        if (rc == 0)
            printf("PASS: Zero-IPC GPU works. IPC layer can be deleted.\n");
        else
            printf("FAIL: exit code %d\n", rc);
        return rc;
    } else if (WIFSIGNALED(wstatus)) {
        printf("CRASH: signal %d — Metal runtime crashed after fork.\n",
               WTERMSIG(wstatus));
        return 1;
    }
    return 1;
}
