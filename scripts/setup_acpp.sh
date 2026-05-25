#!/usr/bin/env bash
# Build a repo-local AdaptiveCpp toolchain for Metal, CUDA, or generic CPU.

set -euo pipefail

PG_ACCEL_REPO_ROOT="${PG_ACCEL_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PG_ACCEL_TOOL_ROOT="${PG_ACCEL_TOOL_ROOT:-$PG_ACCEL_REPO_ROOT/.pgaccel}"
ACPP_REQUIRED_SHA="${ACPP_REQUIRED_SHA:-0ebc10e5a596c4760b29bab1bdae45a4809f2ace}"
SOFT_FP64_REQUIRED_TAG="${SOFT_FP64_REQUIRED_TAG:-v1.3.0}"
ACPP_SRC="${ACPP_SRC:-$PG_ACCEL_TOOL_ROOT/src/AdaptiveCpp}"
SOFT_FP64_SRC="${SOFT_FP64_SRC:-$PG_ACCEL_TOOL_ROOT/src/soft-fp64}"
ACPP_BACKEND="${ACPP_BACKEND:-auto}"
ACPP_AUTO_CMAKE_ARGS=()

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

homebrew_prefix() {
    if command -v brew >/dev/null 2>&1; then
        brew --prefix
    elif [ -d /opt/homebrew ]; then
        printf '%s\n' /opt/homebrew
    else
        printf '%s\n' /usr/local
    fi
}

llvm_major() {
    "$1/bin/llvm-config" --version | sed -E 's/^([0-9]+).*/\1/'
}

select_macos_llvm() {
    if [ -n "${LLVM_PREFIX:-}" ]; then
        return 0
    fi

    local brew_root candidate major
    brew_root="$(homebrew_prefix)"
    for candidate in \
        "$brew_root/opt/llvm@21" \
        "$brew_root/opt/llvm@20" \
        "$brew_root/opt/llvm@19" \
        "$brew_root/opt/llvm"; do
        [ -x "$candidate/bin/clang++" ] || continue
        [ -x "$candidate/bin/llvm-config" ] || continue
        [ -d "$candidate/lib/cmake/llvm" ] || continue
        [ -f "$candidate/include/llvm/Passes/PassPlugin.h" ] || continue
        major="$(llvm_major "$candidate")"
        if [ "$major" -le 21 ]; then
            LLVM_PREFIX="$candidate"
            export LLVM_PREFIX
            echo "Using LLVM_PREFIX=$LLVM_PREFIX"
            return 0
        fi
    done

    echo "error: no supported Homebrew LLVM found for AdaptiveCpp Metal setup" >&2
    echo "       install llvm@20 or set LLVM_PREFIX=/path/to/llvm <= 21" >&2
    exit 1
}

select_macos_lld() {
    if [ -n "${ACPP_LLD_PATH:-}" ]; then
        return 0
    fi

    local brew_root candidate shim
    brew_root="$(homebrew_prefix)"
    for candidate in \
        "$LLVM_PREFIX/bin/ld64.lld" \
        "$LLVM_PREFIX/bin/lld" \
        "$brew_root/opt/lld/bin/ld64.lld" \
        "$brew_root/opt/lld/bin/lld"; do
        [ -x "$candidate" ] || continue
        if [ "$(basename "$candidate")" = "ld64.lld" ]; then
            ACPP_LLD_PATH="$candidate"
        else
            mkdir -p "$PG_ACCEL_TOOL_ROOT/bin"
            shim="$PG_ACCEL_TOOL_ROOT/bin/ld64.lld"
            ln -sfn "$candidate" "$shim"
            ACPP_LLD_PATH="$shim"
        fi
        export ACPP_LLD_PATH
        echo "Using ACPP_LLD_PATH=$ACPP_LLD_PATH"
        return 0
    done

    echo "error: no lld or ld64.lld found for AdaptiveCpp Metal setup" >&2
    echo "       install Homebrew lld or set ACPP_LLD_PATH=/path/to/ld64.lld" >&2
    exit 1
}

configure_macos_metal_defaults() {
    [ "$(uname -s)" = "Darwin" ] || return 0

    select_macos_llvm
    select_macos_lld

    if [ -z "${CMAKE_OSX_SYSROOT:-}" ] &&
        ! printf '%s\n' "${ACPP_CMAKE_FLAGS:-}" | grep -q 'CMAKE_OSX_SYSROOT'; then
        local sdk_path
        sdk_path="$(xcrun --show-sdk-path)"
        ACPP_AUTO_CMAKE_ARGS+=("-DCMAKE_OSX_SYSROOT=$sdk_path")
        echo "Using CMAKE_OSX_SYSROOT=$sdk_path"
    fi
}

if [ "$backend" = "metal" ]; then
    configure_macos_metal_defaults
fi

ACPP_PREFIX="${ACPP_PREFIX:-$PG_ACCEL_TOOL_ROOT/acpp/$backend}"
if [ -z "${ACPP_BUILD_DIR:-}" ]; then
    if [ "$backend" = "metal" ] && [ -n "${LLVM_PREFIX:-}" ] && [ -x "$LLVM_PREFIX/bin/llvm-config" ]; then
        ACPP_BUILD_DIR="$ACPP_SRC/build-$backend-llvm$(llvm_major "$LLVM_PREFIX")"
    else
        ACPP_BUILD_DIR="$ACPP_SRC/build-$backend"
    fi
fi

if [ ! -d "$ACPP_SRC/.git" ]; then
    git clone --branch fork-safe-metal https://github.com/yocontra/AdaptiveCpp.git "$ACPP_SRC"
fi
git -C "$ACPP_SRC" fetch origin fork-safe-metal
git -C "$ACPP_SRC" checkout "$ACPP_REQUIRED_SHA"

apply_metal_cpp_compat_patch() {
    [ "$backend" = "metal" ] || return 0

    local metal_root metal_header metal_code
    metal_root="${METAL_INCLUDE_DIR:-$PG_ACCEL_TOOL_ROOT/metal-cpp}"
    metal_header="$metal_root/Metal/MTLLibrary.hpp"
    metal_code="$ACPP_SRC/src/runtime/metal/metal_code_object.cpp"
    if [ -f "$metal_header" ] &&
        [ -f "$metal_code" ] &&
        ! grep -q 'LanguageVersion4_0' "$metal_header" &&
        grep -q 'MTL::LanguageVersion4_0' "$metal_code"; then
        echo "Applying metal-cpp compatibility patch: LanguageVersion4_0 -> LanguageVersion3_2"
        perl -0pi -e 's/MTL::LanguageVersion4_0/MTL::LanguageVersion3_2/g' "$metal_code"
    fi
}

apply_metal_cpp_compat_patch

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
    "${ACPP_AUTO_CMAKE_ARGS[@]}"
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
