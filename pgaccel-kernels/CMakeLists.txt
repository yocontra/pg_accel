cmake_minimum_required(VERSION 3.21)
project(pgaccel_kernels LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

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

add_library(pgaccel_kernels SHARED
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
)

target_include_directories(pgaccel_kernels PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
)

if(PGACCEL_USE_SYCL AND AdaptiveCpp_FOUND)
    add_sycl_to_target(TARGET pgaccel_kernels)
    target_compile_definitions(pgaccel_kernels PRIVATE PGACCEL_HAS_SYCL=1)
    message(STATUS "pgaccel: Building with AdaptiveCpp SYCL support")
else()
    target_compile_definitions(pgaccel_kernels PRIVATE PGACCEL_HAS_SYCL=0)
    message(STATUS "pgaccel: Building CPU-only (no SYCL)")
endif()

# Standalone test binary
add_executable(test_device test/test_device.cpp)
target_link_libraries(test_device PRIVATE pgaccel_kernels)

add_executable(test_bbox test/test_bbox.cpp)
target_link_libraries(test_bbox PRIVATE pgaccel_kernels)

add_executable(test_spatial test/test_spatial.cpp)
target_link_libraries(test_spatial PRIVATE pgaccel_kernels)

add_executable(test_spatial_dispatch test/test_spatial_dispatch.cpp)
target_link_libraries(test_spatial_dispatch PRIVATE pgaccel_kernels)

add_executable(test_h3 test/test_h3.cpp)
target_link_libraries(test_h3 PRIVATE pgaccel_kernels)

add_executable(test_raster test/test_raster.cpp)
target_link_libraries(test_raster PRIVATE pgaccel_kernels)

install(TARGETS pgaccel_kernels LIBRARY DESTINATION lib)
install(FILES include/pgaccel_ffi.h DESTINATION include)
