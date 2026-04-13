cmake_minimum_required(VERSION 3.21)
project(pgaccel_kernels LANGUAGES CXX OBJCXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_OBJCXX_STANDARD 17)
set(CMAKE_OBJCXX_STANDARD_REQUIRED ON)

# Default to Release for optimized GPU kernels (-O3).
if(NOT CMAKE_BUILD_TYPE)
    set(CMAKE_BUILD_TYPE Release)
endif()

option(PGACCEL_USE_SYCL "Build with AdaptiveCpp/SYCL support" OFF)
option(PGACCEL_USE_METAL "Build with native Metal backend (recommended)" ON)

if(PGACCEL_USE_SYCL)
    # AdaptiveCpp installs to ~/local by default (see: just setup-gpu).
    # Pass -DCMAKE_PREFIX_PATH=$HOME/local when configuring, or use: just gpu-build
    find_package(AdaptiveCpp QUIET)
    if(NOT AdaptiveCpp_FOUND)
        message(WARNING "AdaptiveCpp not found. Building without SYCL support.")
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

# ---------------------------------------------------------------------------
# Native Metal backend (replaces AdaptiveCpp SYCL)
# ---------------------------------------------------------------------------
if(PGACCEL_USE_METAL AND APPLE)
    list(APPEND KERNEL_SOURCES src/metal_backend.mm)
    message(STATUS "pgaccel: Building with native Metal backend")

    # Compile Metal shaders to .metallib
    set(SHADER_DIR ${CMAKE_CURRENT_SOURCE_DIR}/shaders)
    set(METALLIB_OUTPUT ${CMAKE_CURRENT_BINARY_DIR}/pgaccel_kernels.metallib)

    file(GLOB METAL_SHADERS ${SHADER_DIR}/*.metal)

    # Compile each .metal → .air, then link all into one .metallib
    set(AIR_FILES "")
    foreach(shader ${METAL_SHADERS})
        get_filename_component(shader_name ${shader} NAME_WE)
        set(air_file ${CMAKE_CURRENT_BINARY_DIR}/${shader_name}.air)
        add_custom_command(
            OUTPUT ${air_file}
            COMMAND xcrun metal -c ${shader} -o ${air_file}
            DEPENDS ${shader}
            COMMENT "Compiling Metal shader: ${shader_name}.metal"
        )
        list(APPEND AIR_FILES ${air_file})
    endforeach()

    add_custom_command(
        OUTPUT ${METALLIB_OUTPUT}
        COMMAND xcrun metallib ${AIR_FILES} -o ${METALLIB_OUTPUT}
        DEPENDS ${AIR_FILES}
        COMMENT "Linking Metal library: pgaccel_kernels.metallib"
    )

    add_custom_target(metal_shaders ALL DEPENDS ${METALLIB_OUTPUT})

    # Binary archive creation tool
    add_executable(create_binary_archive tools/create_binary_archive.mm)
    target_link_libraries(create_binary_archive PRIVATE "-framework Metal" "-framework Foundation")
    target_include_directories(create_binary_archive PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
endif()

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

if(PGACCEL_USE_METAL AND APPLE)
    target_compile_definitions(pgaccel_kernels PRIVATE PGACCEL_HAS_METAL=1)
    target_compile_definitions(pgaccel_kernels_shared PRIVATE PGACCEL_HAS_METAL=1)
    target_link_libraries(pgaccel_kernels PRIVATE "-framework Metal" "-framework Foundation")
    target_link_libraries(pgaccel_kernels_shared PRIVATE "-framework Metal" "-framework Foundation")
    add_dependencies(pgaccel_kernels metal_shaders)
    add_dependencies(pgaccel_kernels_shared metal_shaders)
endif()

if(PGACCEL_USE_SYCL AND AdaptiveCpp_FOUND)
    add_sycl_to_target(TARGET pgaccel_kernels)
    target_compile_definitions(pgaccel_kernels PRIVATE PGACCEL_HAS_SYCL=1)
    add_sycl_to_target(TARGET pgaccel_kernels_shared)
    target_compile_definitions(pgaccel_kernels_shared PRIVATE PGACCEL_HAS_SYCL=1)
    message(STATUS "pgaccel: Building with AdaptiveCpp SYCL support")
else()
    target_compile_definitions(pgaccel_kernels PRIVATE PGACCEL_HAS_SYCL=0)
    target_compile_definitions(pgaccel_kernels_shared PRIVATE PGACCEL_HAS_SYCL=0)
    if(NOT PGACCEL_USE_METAL OR NOT APPLE)
        message(STATUS "pgaccel: Building CPU-only (no SYCL, no Metal)")
    endif()
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

add_executable(test_fork_cold test/test_fork_cold.cpp)
target_link_libraries(test_fork_cold PRIVATE pgaccel_kernels_shared)

add_executable(test_reduce_minimal test/test_reduce_minimal.cpp)
target_link_libraries(test_reduce_minimal PRIVATE pgaccel_kernels_shared)

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
if(PGACCEL_USE_SYCL AND AdaptiveCpp_FOUND)
    add_sycl_to_target(TARGET test_sycl_basic)
    target_compile_definitions(test_sycl_basic PRIVATE PGACCEL_HAS_SYCL=1)
else()
    target_compile_definitions(test_sycl_basic PRIVATE PGACCEL_HAS_SYCL=0)
endif()

# Metal backend test
if(PGACCEL_USE_METAL AND APPLE)
    add_executable(test_metal_reduce test/test_metal_reduce.mm)
    target_link_libraries(test_metal_reduce PRIVATE pgaccel_kernels_shared)
    add_test(NAME test_metal_reduce COMMAND test_metal_reduce)

    add_executable(test_metal_sort test/test_metal_sort.mm)
    target_link_libraries(test_metal_sort PRIVATE pgaccel_kernels_shared)
    add_test(NAME test_metal_sort COMMAND test_metal_sort)

    add_executable(test_metal_window test/test_metal_window.mm)
    target_link_libraries(test_metal_window PRIVATE pgaccel_kernels_shared)
    add_test(NAME test_metal_window COMMAND test_metal_window)

    add_executable(test_metal_h3 test/test_metal_h3.mm)
    target_link_libraries(test_metal_h3 PRIVATE pgaccel_kernels_shared)
    add_test(NAME test_metal_h3 COMMAND test_metal_h3)

    add_executable(test_metal_bbox test/test_metal_bbox.mm)
    target_link_libraries(test_metal_bbox PRIVATE pgaccel_kernels_shared)
    add_test(NAME test_metal_bbox COMMAND test_metal_bbox)

    add_executable(test_metal_fused test/test_metal_fused.mm)
    target_link_libraries(test_metal_fused PRIVATE pgaccel_kernels_shared)
    add_test(NAME test_metal_fused COMMAND test_metal_fused)
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
install(FILES include/pgaccel_ffi.h include/pgaccel_expr.h include/pgaccel_fused.h include/metal_backend.h DESTINATION include)

# Install metallib alongside the library
if(PGACCEL_USE_METAL AND APPLE)
    install(FILES ${METALLIB_OUTPUT} DESTINATION lib)
endif()
