cmake_minimum_required(VERSION 3.21)
project(pgaccel_kernels LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Default to Release for optimized GPU kernels (-O3).
if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

# AdaptiveCpp installs to ~/local by default (see: just setup-gpu-acpp).
# Pass -DCMAKE_PREFIX_PATH=$HOME/local when configuring, or use: just gpu-build
find_package(AdaptiveCpp REQUIRED)

set(KERNEL_SOURCES
    src/device_manager.cpp
    src/platform_caps.cpp
    src/bbox_ops.cpp
    src/mem_pool.cpp
    src/sort.cpp
    src/reduce.cpp
    src/spatial_predicates.cpp
    src/spatial_dispatch.cpp
    src/h3_ops.cpp
    src/raster_ops.cpp
    src/expr_eval.cpp
    src/expr_templates.cpp
    src/hash_join.cpp
    src/hash_agg.cpp
    src/window.cpp
    src/fused_ops.cpp
)

# Static library for embedding into the Rust extension.
add_library(pgaccel_kernels STATIC ${KERNEL_SOURCES})

target_include_directories(pgaccel_kernels PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
)

# Shared library for standalone test executables.
add_library(pgaccel_kernels_shared SHARED ${KERNEL_SOURCES})

target_include_directories(pgaccel_kernels_shared PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
)

add_sycl_to_target(TARGET pgaccel_kernels)
add_sycl_to_target(TARGET pgaccel_kernels_shared)
message(STATUS "pgaccel_kernels: SYCL (AdaptiveCpp) is the only supported backend; build fails without AdaptiveCpp")

# -ffast-math enables vectorizer-friendly float math (reciprocal throughput,
# FMA contraction, reassociation) across all kernels. IEEE 754-2008 NaN +
# signed-zero semantics for fmin/fmax are preserved by the AdaptiveCpp
# Metal libkernel (src/libkernel/sscp/metal/math.cpp) and the emitter's
# inline NaN sequence (Emitter.cpp, __acpp_sscp_fmin/fmax_f32 branch), so
# fast-math here does not silently drop those contracts.
target_compile_options(pgaccel_kernels        PRIVATE -ffast-math)
target_compile_options(pgaccel_kernels_shared PRIVATE -ffast-math)

# Files that rely on IEEE NaN/Inf semantics opt out of -ffast-math:
#   raster_ops         — isnan/isinf drive NODATA propagation
#   spatial_predicates — isfinite gates NaN/Inf → UNCERTAIN
#   sort               — pad_value<float>() uses +infinity as the bitonic
#                         sentinel; -ffast-math's no-infinities assumption
#                         makes pad elements leak into the output with
#                         index = UINT32_MAX.
set_source_files_properties(
    src/raster_ops.cpp
    src/spatial_predicates.cpp
    src/sort.cpp
    PROPERTIES COMPILE_FLAGS "-fno-fast-math -fhonor-nans -fhonor-infinities")

# Standalone test binary
add_executable(test_device test/test_device.cpp)
target_link_libraries(test_device PRIVATE pgaccel_kernels_shared)

add_executable(test_bbox test/test_bbox.cpp)
target_link_libraries(test_bbox PRIVATE pgaccel_kernels_shared)

add_executable(test_spatial test/test_spatial.cpp)
target_link_libraries(test_spatial PRIVATE pgaccel_kernels_shared)

add_executable(test_spatial_dispatch test/test_spatial_dispatch.cpp)
target_link_libraries(test_spatial_dispatch PRIVATE pgaccel_kernels_shared)

add_executable(test_h3 test/test_h3.cpp)
target_link_libraries(test_h3 PRIVATE pgaccel_kernels_shared)

add_executable(test_raster test/test_raster.cpp)
target_link_libraries(test_raster PRIVATE pgaccel_kernels_shared)

add_executable(test_correctness test/test_correctness.cpp)
target_link_libraries(test_correctness PRIVATE pgaccel_kernels_shared)

add_executable(test_fork test/test_fork.cpp)
target_link_libraries(test_fork PRIVATE pgaccel_kernels_shared)

add_executable(test_fork_cold test/test_fork_cold.cpp)
target_link_libraries(test_fork_cold PRIVATE pgaccel_kernels_shared)

add_executable(test_fork_warmed test/test_fork_warmed.cpp)
target_link_libraries(test_fork_warmed PRIVATE pgaccel_kernels_shared)

add_executable(test_reduce_minimal test/test_reduce_minimal.cpp)
target_link_libraries(test_reduce_minimal PRIVATE pgaccel_kernels_shared)

add_executable(test_reduce_stats test/test_reduce_stats.cpp)
target_link_libraries(test_reduce_stats PRIVATE pgaccel_kernels_shared)

add_executable(test_exec_gpu test/test_exec_gpu.cpp)
target_link_libraries(test_exec_gpu PRIVATE pgaccel_kernels_shared)

add_executable(test_sort_bench test/test_sort_bench.cpp)
target_link_libraries(test_sort_bench PRIVATE pgaccel_kernels_shared)

add_executable(test_spatial_bench test/test_spatial_bench.cpp)
target_link_libraries(test_spatial_bench PRIVATE pgaccel_kernels_shared)

add_executable(test_raster_bench test/test_raster_bench.cpp)
target_link_libraries(test_raster_bench PRIVATE pgaccel_kernels_shared)

add_executable(test_sycl_basic test/test_sycl_basic.cpp)
target_link_libraries(test_sycl_basic PRIVATE pgaccel_kernels_shared)
add_sycl_to_target(TARGET test_sycl_basic)

add_executable(test_fp64_mul_probe test/test_fp64_mul_probe.cpp)
target_link_libraries(test_fp64_mul_probe PRIVATE pgaccel_kernels_shared)
add_sycl_to_target(TARGET test_fp64_mul_probe)

# OOM-never invariant test (W5 fp64-unlock plan §5). Exercises every
# fp64 kernel family with an input > 2 × caps.max_alloc_bytes / sizeof(double)
# and asserts completion, correctness, and peak RSS < 3 × max_alloc.
add_executable(test_oom_invariant test/test_oom_invariant.cpp)
target_link_libraries(test_oom_invariant PRIVATE pgaccel_kernels_shared)

enable_testing()
add_test(NAME test_device COMMAND test_device)
add_test(NAME test_bbox COMMAND test_bbox)
add_test(NAME test_spatial COMMAND test_spatial)
add_test(NAME test_spatial_dispatch COMMAND test_spatial_dispatch)
add_test(NAME test_h3 COMMAND test_h3)
add_test(NAME test_raster COMMAND test_raster)
add_test(NAME test_correctness COMMAND test_correctness)
add_test(NAME test_fork COMMAND test_fork)
add_test(NAME test_oom_invariant COMMAND test_oom_invariant)
add_test(NAME test_fp64_mul_probe COMMAND test_fp64_mul_probe)

install(TARGETS pgaccel_kernels LIBRARY DESTINATION lib)
install(FILES include/pgaccel_ffi.h include/pgaccel_expr.h include/pgaccel_fused.h DESTINATION include)
