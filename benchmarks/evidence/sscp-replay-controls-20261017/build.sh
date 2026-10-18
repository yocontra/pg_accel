#!/usr/bin/env bash
set -euo pipefail
root=/Users/contra/Projects/pg_accel
owned="$root/.codex/scratch/projection-replay-20261017"
sdk="$(xcrun --show-sdk-path)"
llvm=/opt/homebrew/opt/llvm@20
mkdir -p "$owned/logs"
cd "$root"
find .pgaccel/acpp/metal -type f -exec shasum -a 256 {} + > "$owned/logs/installed-prefix.before.sha256"
find .pgaccel/src/AdaptiveCpp/include .pgaccel/src/AdaptiveCpp/src -type f -exec shasum -a 256 {} + > "$owned/logs/installed-source.before.sha256"
readlink .pgaccel/acpp/current > "$owned/logs/current.before.txt"
patch --batch --fuzz=0 -d "$owned/source" -p1 < patches/adaptivecpp/soft-fp-2-package-integration.patch
patch --batch --fuzz=0 -d "$owned/source" -p1 < patches/adaptivecpp/sscp-host-coverage.patch
for relative in include/hipSYCL/runtime/metal/metal_code_object.hpp src/compiler/llvm-to-backend/metal/Emitter.cpp src/compiler/llvm-to-backend/metal/Emitter.hpp src/compiler/sscp/TargetSeparationPass.cpp src/libkernel/sscp/metal/CMakeLists.txt src/runtime/metal/metal_code_object.cpp src/runtime/metal/metal_queue.cpp; do
    cmp "$owned/source/$relative" ".pgaccel/src/AdaptiveCpp/$relative"
done
patch --batch --fuzz=0 -d "$owned/source" -p1 < patches/adaptivecpp/sscp-replay-controls.patch
cp -R .pgaccel/acpp/metal "$owned/prefix"
cmake -S "$owned/source" -B "$owned/build" -G Ninja \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$owned/prefix" \
  -DACPP_COMPILER_FEATURE_PROFILE=full -DBUILD_CLANG_PLUGIN=ON \
  -DACPP_SOFT_FP64_SRC_DIR="$root/.pgaccel/src/soft-fp64" \
  -DACPP_SOFT_FP64_PACKAGE_DIR="$root/.pgaccel/src/soft-fp64/build-pgaccel-metal/install/lib/cmake/soft_fp64" \
  -DCMAKE_OSX_SYSROOT="$sdk" -DLLVM_DIR="$llvm/lib/cmake/llvm" \
  -DCLANG_EXECUTABLE_PATH="$llvm/bin/clang++" \
  -DCMAKE_C_COMPILER="$llvm/bin/clang" -DCMAKE_CXX_COMPILER="$llvm/bin/clang++" \
  -DWITH_METAL_BACKEND=ON -DWITH_CUDA_BACKEND=OFF -DWITH_ROCM_BACKEND=OFF \
  -DWITH_LEVEL_ZERO_BACKEND=OFF -DWITH_OPENCL_BACKEND=OFF -DDEFAULT_TARGETS=generic \
  -DMETAL_INCLUDE_DIR="$root/.pgaccel/metal-cpp" \
  -DACPP_LLD_PATH=/opt/homebrew/opt/lld@20/bin/ld64.lld \
  "-DCMAKE_CXX_FLAGS=-nostdinc++ -isystem $sdk/usr/include/c++/v1" \
  -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
cmake --build "$owned/build" --target acpp-clang --parallel 4
cp "$owned/build/src/compiler/libacpp-clang.so" "$owned/prefix/lib/libacpp-clang.so"
shasum -a 256 -c "$owned/logs/installed-prefix.before.sha256" > "$owned/logs/installed-prefix.after-check.txt"
shasum -a 256 -c "$owned/logs/installed-source.before.sha256" > "$owned/logs/installed-source.after-check.txt"
test "$(readlink .pgaccel/acpp/current)" = "$(cat "$owned/logs/current.before.txt")"
