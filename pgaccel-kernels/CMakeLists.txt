cmake_minimum_required(VERSION 3.21)
project(pgaccel_kernels LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Default to Release for optimized GPU kernels (-O3).
if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

option(PGACCEL_USE_SYCL "Build with AdaptiveCpp/SYCL support" OFF)

if(PGACCEL_USE_SYCL)
    # AdaptiveCpp installs to ~/local by default (see: just setup-gpu).
    # Pass -DCMAKE_PREFIX_PATH=$HOME/local when configuring, or use: just gpu-build
    find_package(AdaptiveCpp QUIET)
    if(NOT AdaptiveCpp_FOUND)
        message(WARNING "AdaptiveCpp not found. Building without GPU support. "
                        "Install via 'just setup-gpu' and build with 'just gpu-build'.")
        set(PGACCEL_USE_SYCL OFF)
    endif()
endif()

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

if(PGACCEL_USE_SYCL AND AdaptiveCpp_FOUND)
    add_sycl_to_target(TARGET pgaccel_kernels)
    target_compile_definitions(pgaccel_kernels PRIVATE PGACCEL_HAS_SYCL=1)
    add_sycl_to_target(TARGET pgaccel_kernels_shared)
    target_compile_definitions(pgaccel_kernels_shared PRIVATE PGACCEL_HAS_SYCL=1)
    message(STATUS "pgaccel: Building with AdaptiveCpp SYCL support")
else()
    target_compile_definitions(pgaccel_kernels PRIVATE PGACCEL_HAS_SYCL=0)
    target_compile_definitions(pgaccel_kernels_shared PRIVATE PGACCEL_HAS_SYCL=0)
    message(STATUS "pgaccel: Building CPU-only (no SYCL)")
endif()

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

add_executable(test_reduce_minimal test/test_reduce_minimal.cpp)
target_link_libraries(test_reduce_minimal PRIVATE pgaccel_kernels_shared)

add_executable(test_sycl_basic test/test_sycl_basic.cpp)
target_link_libraries(test_sycl_basic PRIVATE pgaccel_kernels_shared)
if(PGACCEL_USE_SYCL AND AdaptiveCpp_FOUND)
    add_sycl_to_target(TARGET test_sycl_basic)
    target_compile_definitions(test_sycl_basic PRIVATE PGACCEL_HAS_SYCL=1)
else()
    target_compile_definitions(test_sycl_basic PRIVATE PGACCEL_HAS_SYCL=0)
endif()

enable_testing()
add_test(NAME test_device COMMAND test_device)
add_test(NAME test_bbox COMMAND test_bbox)
add_test(NAME test_spatial COMMAND test_spatial)
add_test(NAME test_spatial_dispatch COMMAND test_spatial_dispatch)
add_test(NAME test_h3 COMMAND test_h3)
add_test(NAME test_raster COMMAND test_raster)
add_test(NAME test_correctness COMMAND test_correctness)
add_test(NAME test_fork COMMAND test_fork)

install(TARGETS pgaccel_kernels LIBRARY DESTINATION lib)
install(FILES include/pgaccel_ffi.h include/pgaccel_expr.h include/pgaccel_fused.h DESTINATION include)
