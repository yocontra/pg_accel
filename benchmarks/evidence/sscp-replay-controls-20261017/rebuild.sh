#!/usr/bin/env bash
set -euo pipefail
root=/Users/contra/Projects/pg_accel
owned="$root/.codex/scratch/projection-replay-20261017"
sdk="$(xcrun --show-sdk-path)"
llvm=/opt/homebrew/opt/llvm@20
export PATH="$owned/no-git:$PATH"
cmake -S "$owned/source" -B "$owned/build-make" -G 'Unix Makefiles' \
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
cmake --build "$owned/build-make" --target acpp-clang --parallel 4
cp "$owned/build-make/src/compiler/libacpp-clang.so" "$owned/prefix/lib/libacpp-clang.so"
cd "$root"
shasum -a 256 -c "$owned/logs/installed-prefix.before.sha256" > "$owned/logs/installed-prefix.after-check.txt"
shasum -a 256 -c "$owned/logs/installed-source.before.sha256" > "$owned/logs/installed-source.after-check.txt"
test "$(readlink .pgaccel/acpp/current)" = "$(cat "$owned/logs/current.before.txt")"
