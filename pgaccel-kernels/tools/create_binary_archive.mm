// create_binary_archive — Build-time tool to create MTLBinaryArchive.
//
// Usage: create_binary_archive <metallib_path> [-o output_path]
//
// Creates a binary archive containing pre-compiled pipeline states for
// all kernels in the metallib. The archive can be loaded in forked PG
// backends without needing MTLCompilerService.

#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
#include <cstdio>

// All kernel function names that need pipeline states
static const char* KERNEL_NAMES[] = {
    "reduce_sum_f32",
    "reduce_min_f32",
    "reduce_max_f32",
    "reduce_sum_i64",
    "reduce_count",
    "reduce_multi_f32",
    "reduce_multi_i64",
    nullptr
};

int main(int argc, char** argv) {
    if (argc < 2) {
        fprintf(stderr, "Usage: %s <metallib_path> [-o output_path]\n", argv[0]);
        return 1;
    }

    const char* metallib_path = argv[1];
    const char* output_path = nullptr;

    for (int i = 2; i < argc; i++) {
        if (strcmp(argv[i], "-o") == 0 && i + 1 < argc) {
            output_path = argv[++i];
        }
    }

    // Default output: same path with -archive suffix
    NSString* metallibStr = [NSString stringWithUTF8String:metallib_path];
    NSString* outputStr = output_path
        ? [NSString stringWithUTF8String:output_path]
        : [metallibStr stringByAppendingString:@"-archive"];

    @autoreleasepool {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) {
            fprintf(stderr, "No Metal device found\n");
            return 1;
        }
        printf("Device: %s\n", [[device name] UTF8String]);

        // Load metallib
        NSError* error = nil;
        NSURL* libUrl = [NSURL fileURLWithPath:metallibStr];
        id<MTLLibrary> lib = [device newLibraryWithURL:libUrl error:&error];
        if (!lib) {
            fprintf(stderr, "Failed to load metallib: %s\n",
                    [[error localizedDescription] UTF8String]);
            return 1;
        }
        printf("Loaded metallib: %s\n", metallib_path);

        // Create empty binary archive
        MTLBinaryArchiveDescriptor* archiveDesc =
            [[MTLBinaryArchiveDescriptor alloc] init];
        id<MTLBinaryArchive> archive =
            [device newBinaryArchiveWithDescriptor:archiveDesc error:&error];
        if (!archive) {
            fprintf(stderr, "Failed to create archive: %s\n",
                    [[error localizedDescription] UTF8String]);
            return 1;
        }

        // Create pipeline for each kernel and add to archive
        int count = 0;
        int failed = 0;
        for (int i = 0; KERNEL_NAMES[i] != nullptr; ++i) {
            const char* name = KERNEL_NAMES[i];
            id<MTLFunction> func = [lib newFunctionWithName:
                [NSString stringWithUTF8String:name]];
            if (!func) {
                fprintf(stderr, "  SKIP: function '%s' not found\n", name);
                failed++;
                continue;
            }

            MTLComputePipelineDescriptor* desc =
                [[MTLComputePipelineDescriptor alloc] init];
            desc.computeFunction = func;
            desc.label = [NSString stringWithUTF8String:name];

            // Create pipeline (compiles for this GPU)
            id<MTLComputePipelineState> pipeline =
                [device newComputePipelineStateWithFunction:func error:&error];
            if (!pipeline) {
                fprintf(stderr, "  FAIL: pipeline '%s': %s\n",
                        name, [[error localizedDescription] UTF8String]);
                failed++;
                continue;
            }

            // Add to archive
            BOOL added = [archive addComputePipelineFunctionsWithDescriptor:desc
                           error:&error];
            if (!added) {
                fprintf(stderr, "  FAIL: archive add '%s': %s\n",
                        name, [[error localizedDescription] UTF8String]);
                failed++;
                continue;
            }

            printf("  OK: %s\n", name);
            count++;
        }

        // Serialize
        NSURL* outputUrl = [NSURL fileURLWithPath:outputStr];
        BOOL written = [archive serializeToURL:outputUrl error:&error];
        if (!written) {
            fprintf(stderr, "Failed to serialize archive: %s\n",
                    [[error localizedDescription] UTF8String]);
            return 1;
        }

        printf("\nBinary archive: %s\n", [outputStr UTF8String]);
        printf("Pipelines: %d OK, %d failed\n", count, failed);
        return failed > 0 ? 1 : 0;
    }
}
