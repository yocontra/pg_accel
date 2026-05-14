#!/usr/bin/env bash
# Build a repo-local AdaptiveCpp toolchain for Metal, CUDA, or generic CPU.

set -euo pipefail

PG_ACCEL_REPO_ROOT="${PG_ACCEL_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PG_ACCEL_TOOL_ROOT="${PG_ACCEL_TOOL_ROOT:-$PG_ACCEL_REPO_ROOT/.pgaccel}"
ACPP_REQUIRED_SHA="${ACPP_REQUIRED_SHA:-4f3cde11a302eebac28aa1ccc79ad3399cb8183c}"
SOFT_FP64_REQUIRED_TAG="${SOFT_FP64_REQUIRED_TAG:-v1.3.0}"
ACPP_SRC="${ACPP_SRC:-$PG_ACCEL_TOOL_ROOT/src/AdaptiveCpp}"
SOFT_FP64_SRC="${SOFT_FP64_SRC:-$PG_ACCEL_TOOL_ROOT/src/soft-fp64}"
ACPP_BACKEND="${ACPP_BACKEND:-auto}"

detect_backend() {
    if [ "$ACPP_BACKEND" != "auto" ]; then
        printf '%s\n' "$ACPP_BACKEND"
        return 0
    fi
    case "$(uname -s)" in
        Darwin) printf 'metal\n' ;;
        Linux)
            if command -v nvidia-smi >/dev/null 2>&1 || command -v nvcc >/dev/null 2>&1; then
                printf 'cuda\n'
            else
                printf 'generic\n'
            fi
            ;;
        *) printf 'generic\n' ;;
    esac
}

backend="$(detect_backend)"
ACPP_PREFIX="${ACPP_PREFIX:-$PG_ACCEL_TOOL_ROOT/acpp/$backend}"
ACPP_BUILD_DIR="${ACPP_BUILD_DIR:-$ACPP_SRC/build-$backend}"

if [ ! -d "$ACPP_SRC/.git" ]; then
    git clone --branch fork-safe-metal https://github.com/yocontra/AdaptiveCpp.git "$ACPP_SRC"
fi
git -C "$ACPP_SRC" fetch origin fork-safe-metal
git -C "$ACPP_SRC" checkout "$ACPP_REQUIRED_SHA"

if [ ! -d "$SOFT_FP64_SRC/.git" ]; then
    git clone --depth 1 --branch "$SOFT_FP64_REQUIRED_TAG" https://github.com/yocontra/soft-fp.git "$SOFT_FP64_SRC"
fi
soft_fp64_desc="$(git -C "$SOFT_FP64_SRC" describe --tags --always)"
if [ "$soft_fp64_desc" != "$SOFT_FP64_REQUIRED_TAG" ]; then
    echo "error: soft-fp64 at $soft_fp64_desc, expected $SOFT_FP64_REQUIRED_TAG" >&2
    exit 1
fi

common_args=(
    -DCMAKE_BUILD_TYPE=Release
    -DCMAKE_INSTALL_PREFIX="$ACPP_PREFIX"
    -DWITH_SSCP_COMPILER=ON
    -DBUILD_CLANG_PLUGIN=ON
    -DACPP_SOFT_FP64_SRC_DIR="$SOFT_FP64_SRC"
)

if [ -n "${LLVM_PREFIX:-}" ]; then
    common_args+=(
        -DLLVM_DIR="$LLVM_PREFIX/lib/cmake/llvm"
        -DCLANG_EXECUTABLE_PATH="$LLVM_PREFIX/bin/clang++"
        -DCMAKE_C_COMPILER="$LLVM_PREFIX/bin/clang"
        -DCMAKE_CXX_COMPILER="$LLVM_PREFIX/bin/clang++"
    )
fi

case "$backend" in
    metal)
        resolved_targets="${ACPP_TARGETS:-generic}"
        common_args+=(
            -DWITH_METAL_BACKEND=ON
            -DWITH_CUDA_BACKEND=OFF
            -DWITH_ROCM_BACKEND=OFF
            -DWITH_LEVEL_ZERO_BACKEND=OFF
            -DWITH_OPENCL_BACKEND=OFF
            -DDEFAULT_TARGETS="$resolved_targets"
        )
        if [ -n "${METAL_INCLUDE_DIR:-}" ]; then
            common_args+=(-DMETAL_INCLUDE_DIR="$METAL_INCLUDE_DIR")
        elif [ -d "$PG_ACCEL_TOOL_ROOT/metal-cpp/Metal" ]; then
            common_args+=(-DMETAL_INCLUDE_DIR="$PG_ACCEL_TOOL_ROOT/metal-cpp")
        fi
        if [ -n "${ACPP_LLD_PATH:-}" ]; then
            common_args+=(-DACPP_LLD_PATH="$ACPP_LLD_PATH")
        fi
        ;;
    cuda)
        resolved_targets="${ACPP_TARGETS:-cuda}"
        common_args+=(
            -DWITH_CUDA_BACKEND=ON
            -DWITH_ROCM_BACKEND=OFF
            -DWITH_LEVEL_ZERO_BACKEND=OFF
            -DWITH_OPENCL_BACKEND=OFF
            -DDEFAULT_TARGETS="$resolved_targets"
        )
        ;;
    generic|cpu)
        resolved_targets="${ACPP_TARGETS:-generic}"
        common_args+=(
            -DWITH_CUDA_BACKEND=OFF
            -DWITH_ROCM_BACKEND=OFF
            -DWITH_LEVEL_ZERO_BACKEND=OFF
            -DWITH_OPENCL_BACKEND=OFF
            -DDEFAULT_TARGETS="$resolved_targets"
        )
        ;;
    *)
        echo "error: unsupported ACPP_BACKEND=$backend; use metal, cuda, or generic" >&2
        exit 2
        ;;
esac

cmake -S "$ACPP_SRC" -B "$ACPP_BUILD_DIR" "${common_args[@]}" ${ACPP_CMAKE_FLAGS:-}
cmake --build "$ACPP_BUILD_DIR" --target install --parallel "${ACPP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

mkdir -p "$PG_ACCEL_TOOL_ROOT/acpp"
ln -sfn "$ACPP_PREFIX" "$PG_ACCEL_TOOL_ROOT/acpp/current"
printf '%s\n' "$resolved_targets" > "$PG_ACCEL_TOOL_ROOT/acpp/current-targets"
"$ACPP_PREFIX/bin/acpp" --acpp-version | grep -q "plugin-with-sscp-compiler: true"
"$ACPP_PREFIX/bin/acpp-info" | sed -n '1,80p'
echo "AdaptiveCpp $backend toolchain installed at $ACPP_PREFIX"
