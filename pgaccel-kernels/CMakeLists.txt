cmake_minimum_required(VERSION 3.21)
project(pgaccel_kernels LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

option(PGACCEL_ENABLE_COVERAGE
    "Instrument pg_accel-owned C++/SYCL sources with Clang source coverage"
    OFF)

if(PGACCEL_ENABLE_COVERAGE)
    if(NOT CMAKE_CXX_COMPILER_ID MATCHES "Clang")
        message(FATAL_ERROR
            "PGACCEL_ENABLE_COVERAGE requires Clang; got ${CMAKE_CXX_COMPILER_ID}")
    endif()
    add_compile_options(
        $<$<COMPILE_LANGUAGE:CXX>:-fprofile-instr-generate>
        $<$<COMPILE_LANGUAGE:CXX>:-fcoverage-mapping>)
    add_link_options(-fprofile-instr-generate)
    message(STATUS
        "pgaccel-kernels: Clang source coverage instrumentation enabled")
endif()

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
    src/grouped_agg.cpp
    src/grouped_agg_workspace.cpp
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
#   raster_ops         — resident metadata and NODATA validation use isfinite
#   spatial_predicates — isfinite gates NaN/Inf → UNCERTAIN
#   spatial_dispatch   — resident fp64 validation and uncertainty gates depend
#                         on finite intermediates and signed-zero semantics.
#   sort               — pad_value<float>() uses +infinity as the bitonic
#                         sentinel; -ffast-math's no-infinities assumption
#                         makes pad elements leak into the output with
#                         index = UINT32_MAX.
#   hash_agg           — sort-based path uses +infinity as the NULL-key
#                         sentinel (sorts NULLs to the end); MIN/MAX
#                         accumulators initialise to ±infinity so the
#                         first real value supersedes the sentinel.
#   fused_ops          — MIN/MAX fused reductions initialise to ±infinity.
#   bbox_ops_f64       — raw IEEE-754 ordering preserves conservative NaN/Inf
#                         behavior without invoking Metal fp64 lowering.
#   window             — sycl_window_sum uses Kahan compensated summation
#                         (window.cpp `comp` term); -ffast-math licenses
#                         reassociation that algebraically deletes the
#                         compensation term, silently degrading SUM
#                         accuracy over large partitions.
#   reduce             — the fused multi-reduce NaN-propagation contract
#                         (pgaccel_ffi.h "NaN handling matches PG's
#                         SUM/MIN/MAX semantics") is void under
#                         -ffinite-math-only; MIN/MAX identities are ±inf.
set_source_files_properties(
    src/raster_ops.cpp
    src/spatial_predicates.cpp
    src/spatial_dispatch.cpp
    src/sort.cpp
    src/hash_agg.cpp
    src/fused_ops.cpp
    src/bbox_ops_f64.cpp
    src/window.cpp
    src/reduce.cpp
    src/grouped_agg.cpp
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

add_executable(test_spatial_pairwise_boundary test/test_spatial_pairwise_boundary.cpp)
target_link_libraries(test_spatial_pairwise_boundary PRIVATE pgaccel_kernels_shared)

add_executable(test_spatial_resident test/test_spatial_resident.cpp)
target_link_libraries(test_spatial_resident PRIVATE pgaccel_kernels_shared)

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

add_executable(test_raster_resident test/test_raster_resident.cpp)
target_link_libraries(test_raster_resident PRIVATE pgaccel_kernels_shared)

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

add_executable(test_fork_cold_barrier test/test_fork_cold_barrier.cpp)
target_link_libraries(test_fork_cold_barrier PRIVATE pgaccel_kernels_shared)

add_executable(test_reduce_minimal test/test_reduce_minimal.cpp)
target_link_libraries(test_reduce_minimal PRIVATE pgaccel_kernels_shared)

add_executable(test_grouped_agg test/test_grouped_agg.cpp)
target_link_libraries(test_grouped_agg PRIVATE pgaccel_kernels_shared)

add_executable(test_grouped_agg_hash test/test_grouped_agg_hash.cpp)
target_link_libraries(test_grouped_agg_hash PRIVATE pgaccel_kernels_shared)

add_executable(test_grouped_agg_workspace test/test_grouped_agg_workspace.cpp)
target_link_libraries(test_grouped_agg_workspace PRIVATE pgaccel_kernels_shared)

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

# Induced-failure honesty gate: a dispatch whose staging cannot succeed
# must return PGACCEL_OOM / PGACCEL_ERROR — never PGACCEL_OK over unwritten
# output, never SIGABRT through the C ABI, never a fake NO_DEVICE. Pins the
# wait_and_throw + extern-C try/catch fixes (TODO-REVIEW 2026-07).
add_executable(test_kernel_failure_status test/test_kernel_failure_status.cpp)
target_link_libraries(test_kernel_failure_status PRIVATE pgaccel_kernels_shared)

enable_testing()
find_package(Python3 COMPONENTS Interpreter QUIET)
set(PGACCEL_REPO_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/..")
set(PGACCEL_GPU_TEST_FILTER "${PGACCEL_REPO_ROOT}/scripts/filter_gpu_output.py")
set(PGACCEL_GPU_TEST_LOG_DIR "${PGACCEL_REPO_ROOT}/.pgaccel/logs"
    CACHE PATH "Directory for raw GPU CTest logs")

# Optional TIMEOUT <seconds> sets an explicit CTest TIMEOUT property, which
# overrides the suite-wide `ctest --timeout` budget for that one test.
function(add_pgaccel_gpu_test test_target)
    cmake_parse_arguments(PGACCEL_TEST "" "TIMEOUT" "" ${ARGN})
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
    if(PGACCEL_TEST_TIMEOUT)
        set_tests_properties(${test_target} PROPERTIES TIMEOUT ${PGACCEL_TEST_TIMEOUT})
    endif()
endfunction()

add_pgaccel_gpu_test(test_device)
add_pgaccel_gpu_test(test_bbox)
add_pgaccel_gpu_test(test_spatial)
add_pgaccel_gpu_test(test_spatial_dispatch)
add_pgaccel_gpu_test(test_spatial_pairwise_boundary TIMEOUT 1800)
add_pgaccel_gpu_test(test_spatial_resident TIMEOUT 1800)
add_pgaccel_gpu_test(test_h3)
add_pgaccel_gpu_test(test_raster_resident TIMEOUT 600)
add_pgaccel_gpu_test(test_correctness)
add_pgaccel_gpu_test(test_exec_gpu)
add_pgaccel_gpu_test(test_fork)
add_pgaccel_gpu_test(test_fork_warmed)
add_pgaccel_gpu_test(test_fork_cold)
add_pgaccel_gpu_test(test_fork_cold_barrier TIMEOUT 1200)
add_pgaccel_gpu_test(test_sycl_basic)
add_pgaccel_gpu_test(test_reduce_stats)
add_pgaccel_gpu_test(test_reduce_bool_bit)
add_pgaccel_gpu_test(test_grouped_agg)
add_pgaccel_gpu_test(test_grouped_agg_hash)
add_pgaccel_gpu_test(test_grouped_agg_workspace)
add_pgaccel_gpu_test(test_hash_agg_partial)
add_pgaccel_gpu_test(test_hash_agg_keys)
add_pgaccel_gpu_test(test_hash_join)
add_pgaccel_gpu_test(test_window)
add_pgaccel_gpu_test(test_expr_templates)
# test_oom_invariant sweeps 2GB fp64 inputs per family on soft-fp64 devices;
# measured 441.5s warm-JIT on M2 Max (2026-07-07, /usr/bin/time -l, exit 0,
# peak RSS 14.08GB). Budget = measured * ~2 to cover cold-JIT runs
# (gpu-test-cold-all). This is a runtime budget, not a hidden skip.
add_pgaccel_gpu_test(test_oom_invariant TIMEOUT 900)
add_pgaccel_gpu_test(test_fp64_mul_probe)
add_pgaccel_gpu_test(test_nested_loop_ineq)
add_pgaccel_gpu_test(test_kernel_failure_status)
# test_ooo_overlap is intentionally NOT registered with CTest: it asserts the
# acceptance criterion of the never-built out-of-order kernel-overlap feature
# (dropped by the 2026-07 architecture decision). It cannot pass on the pinned
# AdaptiveCpp Metal backend: every dispatch marks all USM buffers Read|Write
# (metal_queue.cpp:225-227) on hazard-tracked storage (metal_allocator.cpp:395),
# so Metal serializes all compute device-wide (observed sub-microsecond gaps
# between "overlapping" kernels across 6/6 runs). The binary still builds as a
# manual diagnostic probe for any future fork work on untracked-hazard buffers.

install(TARGETS pgaccel_kernels LIBRARY DESTINATION lib)
install(FILES include/pgaccel_ffi.h include/pgaccel_expr.h include/pgaccel_fused.h
              include/pgaccel_olap.h
              include/pgaccel_nested_loop_ineq.h
        DESTINATION include)
