cmake_minimum_required(VERSION 3.21)
project(pgaccel_kernels LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Default to Release for optimized GPU kernels (-O3).
if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

# AdaptiveCpp installs to .pgaccel/acpp/current by default (see:
# ACPP_BACKEND=cuda just setup-gpu or just setup-gpu-metal). Use
# -DAdaptiveCpp_DIR=... for system toolchains, or use: just gpu-build.
find_package(AdaptiveCpp REQUIRED)

set(KERNEL_SOURCES
    src/device_manager.cpp
    src/archive_stats.cpp
    src/bbox_ops.cpp
    src/bbox_ops_f64.cpp
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
    src/olap_ssbm.cpp
    src/window.cpp
    src/fused_ops.cpp
    src/nested_loop_ineq.cpp
    src/ooo_overlap.cpp
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
# AdaptiveCpp's CMake helper applies RULE_LAUNCH_LINK to every SYCL target.
# For a static library that "link" step is just ar/ranlib; routing it through
# syclcc-launcher can make the launcher hand archive flags to clang++.
set_target_properties(pgaccel_kernels PROPERTIES RULE_LAUNCH_LINK "")
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
#   hash_agg           — sort-based path uses +infinity as the NULL-key
#                         sentinel (sorts NULLs to the end); MIN/MAX
#                         accumulators initialise to ±infinity so the
#                         first real value supersedes the sentinel.
#   fused_ops          — MIN/MAX fused reductions initialise to ±infinity.
#   bbox_ops_f64       — host fallback preserves conservative NaN/Inf
#                         behavior for PG native double boxes.
set_source_files_properties(
    src/raster_ops.cpp
    src/spatial_predicates.cpp
    src/sort.cpp
    src/hash_agg.cpp
    src/fused_ops.cpp
    src/bbox_ops_f64.cpp
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

add_executable(test_window test/test_window.cpp)
target_link_libraries(test_window PRIVATE pgaccel_kernels_shared)

add_executable(test_hash_agg_keys test/test_hash_agg_keys.cpp)
target_link_libraries(test_hash_agg_keys PRIVATE pgaccel_kernels_shared)

add_executable(test_hash_join test/test_hash_join.cpp)
target_link_libraries(test_hash_join PRIVATE pgaccel_kernels_shared)

add_executable(test_hash_agg_partial test/test_hash_agg_partial.cpp)
target_link_libraries(test_hash_agg_partial PRIVATE pgaccel_kernels_shared)

add_executable(test_expr_templates test/test_expr_templates.cpp)
target_link_libraries(test_expr_templates PRIVATE pgaccel_kernels_shared)

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

add_executable(test_fork_archive_stress test/test_fork_archive_stress.cpp)
target_link_libraries(test_fork_archive_stress PRIVATE pgaccel_kernels_shared)

add_executable(test_reduce_minimal test/test_reduce_minimal.cpp)
target_link_libraries(test_reduce_minimal PRIVATE pgaccel_kernels_shared)

add_executable(test_olap_ssbm test/test_olap_ssbm.cpp)
target_link_libraries(test_olap_ssbm PRIVATE pgaccel_kernels_shared)

add_executable(test_reduce_stats test/test_reduce_stats.cpp)
target_link_libraries(test_reduce_stats PRIVATE pgaccel_kernels_shared)

add_executable(test_reduce_bool_bit test/test_reduce_bool_bit.cpp)
target_link_libraries(test_reduce_bool_bit PRIVATE pgaccel_kernels_shared)

add_executable(test_exec_gpu test/test_exec_gpu.cpp)
target_link_libraries(test_exec_gpu PRIVATE pgaccel_kernels_shared)

add_executable(test_sort_bench test/test_sort_bench.cpp)
target_link_libraries(test_sort_bench PRIVATE pgaccel_kernels_shared)

add_executable(test_ooo_overlap test/test_ooo_overlap.cpp)
target_link_libraries(test_ooo_overlap PRIVATE pgaccel_kernels_shared)

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

function(add_fp64_transcendental_probe target_name op_id)
    add_executable(${target_name} test/test_fp64_transcendental_probe.cpp)
    target_link_libraries(${target_name} PRIVATE pgaccel_kernels_shared)
    target_compile_definitions(${target_name}
        PRIVATE
            PGACCEL_FP64_PROBE_OP=${op_id}
            PGACCEL_FP64_PROBE_LABEL="${target_name}")
    add_sycl_to_target(TARGET ${target_name})
endfunction()

add_fp64_transcendental_probe(test_fp64_probe_add 0)
add_fp64_transcendental_probe(test_fp64_probe_mul 1)
add_fp64_transcendental_probe(test_fp64_probe_sqrt 2)
add_fp64_transcendental_probe(test_fp64_probe_sin 3)
add_fp64_transcendental_probe(test_fp64_probe_cos 4)
add_fp64_transcendental_probe(test_fp64_probe_asin 5)
add_fp64_transcendental_probe(test_fp64_probe_atan2 6)
add_fp64_transcendental_probe(test_fp64_probe_haversine 7)

# OOM-never invariant test (W5 fp64-unlock plan §5). Exercises every
# fp64 kernel family with an input > 2 × caps.max_alloc_bytes / sizeof(double)
# and asserts completion, correctness, and peak RSS < 3 × max_alloc.
add_executable(test_oom_invariant test/test_oom_invariant.cpp)
target_link_libraries(test_oom_invariant PRIVATE pgaccel_kernels_shared)

# NLJ scalar-inequality kernel coverage (Phase 4 NestedLoop recognition
# launchpad — pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs
# `observe_nestloop_scalar_opportunity`).
add_executable(test_nested_loop_ineq test/test_nested_loop_ineq.cpp)
target_link_libraries(test_nested_loop_ineq PRIVATE pgaccel_kernels_shared)

enable_testing()
find_package(Python3 COMPONENTS Interpreter QUIET)
set(PGACCEL_REPO_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/..")
set(PGACCEL_GPU_TEST_FILTER "${PGACCEL_REPO_ROOT}/scripts/filter_gpu_output.py")
set(PGACCEL_GPU_TEST_LOG_DIR "${PGACCEL_REPO_ROOT}/.pgaccel/logs"
    CACHE PATH "Directory for raw GPU CTest logs")

function(add_pgaccel_gpu_test test_target)
    if(Python3_Interpreter_FOUND AND EXISTS "${PGACCEL_GPU_TEST_FILTER}")
        add_test(
            NAME ${test_target}
            COMMAND ${Python3_EXECUTABLE} "${PGACCEL_GPU_TEST_FILTER}"
                --label ${test_target}
                --log-dir "${PGACCEL_GPU_TEST_LOG_DIR}"
                -- "$<TARGET_FILE:${test_target}>")
    else()
        add_test(NAME ${test_target} COMMAND ${test_target})
    endif()
endfunction()

add_pgaccel_gpu_test(test_device)
add_pgaccel_gpu_test(test_bbox)
add_pgaccel_gpu_test(test_spatial)
add_pgaccel_gpu_test(test_spatial_dispatch)
add_pgaccel_gpu_test(test_h3)
add_pgaccel_gpu_test(test_raster)
add_pgaccel_gpu_test(test_correctness)
add_pgaccel_gpu_test(test_exec_gpu)
add_pgaccel_gpu_test(test_fork)
add_pgaccel_gpu_test(test_fork_warmed)
add_pgaccel_gpu_test(test_fork_cold)
add_pgaccel_gpu_test(test_sycl_basic)
add_pgaccel_gpu_test(test_reduce_stats)
add_pgaccel_gpu_test(test_reduce_bool_bit)
add_pgaccel_gpu_test(test_olap_ssbm)
add_pgaccel_gpu_test(test_hash_agg_partial)
add_pgaccel_gpu_test(test_hash_agg_keys)
add_pgaccel_gpu_test(test_hash_join)
add_pgaccel_gpu_test(test_window)
add_pgaccel_gpu_test(test_expr_templates)
add_pgaccel_gpu_test(test_oom_invariant)
add_pgaccel_gpu_test(test_fp64_mul_probe)
add_pgaccel_gpu_test(test_nested_loop_ineq)
add_pgaccel_gpu_test(test_ooo_overlap)

install(TARGETS pgaccel_kernels LIBRARY DESTINATION lib)
install(FILES include/pgaccel_ffi.h include/pgaccel_expr.h include/pgaccel_fused.h
              include/pgaccel_nested_loop_ineq.h
        DESTINATION include)
