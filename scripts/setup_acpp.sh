#!/usr/bin/env bash
# Build a repo-local AdaptiveCpp toolchain for Metal, CUDA, or generic CPU.

set -euo pipefail

PG_ACCEL_REPO_ROOT="${PG_ACCEL_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PG_ACCEL_TOOL_ROOT="${PG_ACCEL_TOOL_ROOT:-$PG_ACCEL_REPO_ROOT/.pgaccel}"
ACPP_REQUIRED_SHA="${ACPP_REQUIRED_SHA:-$(cat "$PG_ACCEL_REPO_ROOT/.acpp-version")}"
SOFT_FP64_REQUIRED_TAG="${SOFT_FP64_REQUIRED_TAG:-v2.0.0}"
ACPP_SRC="${ACPP_SRC:-$PG_ACCEL_TOOL_ROOT/src/AdaptiveCpp}"
SOFT_FP64_SRC="${SOFT_FP64_SRC:-$PG_ACCEL_TOOL_ROOT/src/soft-fp64}"
SOFT_FP64_DEVICE_PATCH_RELATIVE="patches/soft-fp/metal-constexpr-bitcast.patch"
SOFT_FP64_DEVICE_PATCH="$PG_ACCEL_REPO_ROOT/$SOFT_FP64_DEVICE_PATCH_RELATIVE"
SOFT_FP64_APPLIED_PATCH=""
ACPP_BACKEND="${ACPP_BACKEND:-auto}"
ACPP_AUTO_CMAKE_ARGS=()
ACPP_REQUIRED_CMAKE_ARGS=()
SOFT_FP64_CMAKE_ARGS=()
ACPP_MACOS_SDK_PATH="${ACPP_MACOS_SDK_PATH:-}"

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
        "$brew_root/opt/lld@20/bin/ld64.lld" \
        "$brew_root/opt/lld@20/bin/lld" \
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
    echo "       install Homebrew lld@20 or set ACPP_LLD_PATH=/path/to/ld64.lld" >&2
    exit 1
}

configure_macos_metal_defaults() {
    [ "$(uname -s)" = "Darwin" ] || return 0

    select_macos_llvm
    select_macos_lld

    if printf '%s\n' "${ACPP_CMAKE_FLAGS:-}" | grep -q 'CMAKE_OSX_SYSROOT' &&
        [ -z "${CMAKE_OSX_SYSROOT:-}" ] &&
        [ -z "$ACPP_MACOS_SDK_PATH" ]; then
        echo "error: set CMAKE_OSX_SYSROOT or ACPP_MACOS_SDK_PATH instead of passing only -DCMAKE_OSX_SYSROOT through ACPP_CMAKE_FLAGS" >&2
        exit 1
    fi

    if [ -z "$ACPP_MACOS_SDK_PATH" ]; then
        if [ -n "${CMAKE_OSX_SYSROOT:-}" ]; then
            case "$CMAKE_OSX_SYSROOT" in
                /*) ACPP_MACOS_SDK_PATH="$CMAKE_OSX_SYSROOT" ;;
                *) ACPP_MACOS_SDK_PATH="$(xcrun --sdk "$CMAKE_OSX_SYSROOT" --show-sdk-path)" ;;
            esac
        else
            ACPP_MACOS_SDK_PATH="$(xcrun --show-sdk-path)"
        fi
    fi
    if [ ! -d "$ACPP_MACOS_SDK_PATH/usr/include/c++/v1" ]; then
        echo "error: macOS SDK libc++ headers are missing: $ACPP_MACOS_SDK_PATH/usr/include/c++/v1" >&2
        exit 1
    fi

    if [ -z "${CMAKE_OSX_SYSROOT:-}" ] &&
        ! printf '%s\n' "${ACPP_CMAKE_FLAGS:-}" | grep -q 'CMAKE_OSX_SYSROOT'; then
        ACPP_AUTO_CMAKE_ARGS+=("-DCMAKE_OSX_SYSROOT=$ACPP_MACOS_SDK_PATH")
    fi
    ACPP_REQUIRED_CMAKE_ARGS+=(
        "-DCMAKE_CXX_FLAGS=-nostdinc++ -isystem $ACPP_MACOS_SDK_PATH/usr/include/c++/v1"
    )
    echo "Using CMAKE_OSX_SYSROOT=$ACPP_MACOS_SDK_PATH"
    echo "Using matching SDK libc++ headers from $ACPP_MACOS_SDK_PATH/usr/include/c++/v1"
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
SOFT_FP64_BUILD_DIR="${SOFT_FP64_BUILD_DIR:-$SOFT_FP64_SRC/build-pgaccel-$backend}"
SOFT_FP64_PREFIX="${SOFT_FP64_PREFIX:-$SOFT_FP64_BUILD_DIR/install}"
SOFT_FP64_PACKAGE_DIR=""
SOFT_FP_PACKAGE_DIR=""
SOFT_FP64_PACKAGE_VERSION="${SOFT_FP64_REQUIRED_TAG#v}"

if [ ! -d "$ACPP_SRC/.git" ]; then
    git clone --branch fork-safe-metal https://github.com/yocontra/AdaptiveCpp.git "$ACPP_SRC"
fi
git -C "$ACPP_SRC" fetch origin fork-safe-metal
git -C "$ACPP_SRC" checkout "$ACPP_REQUIRED_SHA"
ACPP_HEAD="$(git -C "$ACPP_SRC" rev-parse HEAD)"
if [ "$ACPP_HEAD" != "$ACPP_REQUIRED_SHA" ]; then
    echo "error: AdaptiveCpp checkout resolved to $ACPP_HEAD, expected $ACPP_REQUIRED_SHA" >&2
    exit 1
fi
echo "Using AdaptiveCpp commit $ACPP_HEAD"

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

apply_default_targets_json_patch() {
    local patch cmake_file
    patch="$PG_ACCEL_REPO_ROOT/patches/adaptivecpp/default-targets-json.patch"
    cmake_file="$ACPP_SRC/CMakeLists.txt"
    [ -f "$patch" ] || return 0
    [ -f "$cmake_file" ] || return 0
    if grep -q 'ACPP_DEFAULT_TARGETS_JSON' "$cmake_file"; then
        return 0
    fi
    echo "Applying AdaptiveCpp DEFAULT_TARGETS JSON serialization patch"
    git -C "$ACPP_SRC" apply "$patch"
}

apply_sleef_helper_address_space_patch() {
    local patch emitter_file cmake_file emitter_patched
    patch="$PG_ACCEL_REPO_ROOT/patches/adaptivecpp/sleef-helper-address-space-specialization.patch"
    emitter_file="$ACPP_SRC/src/compiler/llvm-to-backend/metal/Emitter.hpp"
    cmake_file="$ACPP_SRC/src/libkernel/sscp/metal/CMakeLists.txt"
    [ -f "$patch" ] || return 0
    [ -f "$emitter_file" ] || return 0
    [ -f "$cmake_file" ] || return 0

    emitter_patched=0
    if grep -q 'functionAddressSpaceSpecializations' "$emitter_file"; then
        emitter_patched=1
    fi

    if [ "$emitter_patched" -eq 1 ] &&
        grep -q 'SF64_DISABLE_SLEEF_INLINE' "$cmake_file"; then
        return 0
    fi

    if [ "$emitter_patched" -eq 0 ]; then
        echo "Applying AdaptiveCpp SLEEF helper address-space specialization patch"
        git -C "$ACPP_SRC" apply --exclude=src/libkernel/sscp/metal/CMakeLists.txt "$patch"
    fi

    if ! grep -q 'SF64_DISABLE_SLEEF_INLINE' "$cmake_file"; then
        echo "Applying AdaptiveCpp SLEEF no-inline compile flag"
        perl -0pi -e 's/-DSOFT_FP64_SNAN_PROPAGATE=0\)/-DSOFT_FP64_SNAN_PROPAGATE=0\n       -DSF64_DISABLE_SLEEF_INLINE)/' "$cmake_file"
    fi
}

apply_soft_fp_2_package_patch() {
    local patch cmake_file marker
    patch="$PG_ACCEL_REPO_ROOT/patches/adaptivecpp/soft-fp-2-package-integration.patch"
    cmake_file="$ACPP_SRC/src/libkernel/sscp/metal/CMakeLists.txt"
    marker="ACPP_SOFT_FP64_PACKAGE_DIR"

    if [ ! -s "$patch" ]; then
        echo "error: required AdaptiveCpp soft-fp 2 integration patch is missing" >&2
        exit 1
    fi
    if [ ! -f "$cmake_file" ]; then
        echo "error: AdaptiveCpp soft-fp integration target is missing: $cmake_file" >&2
        exit 1
    fi
    if grep -q "$marker" "$cmake_file"; then
        if ! git -C "$ACPP_SRC" apply --reverse --check "$patch"; then
            echo "error: applied AdaptiveCpp soft-fp 2 integration patch has drifted" >&2
            exit 1
        fi
        return 0
    fi
    if ! git -C "$ACPP_SRC" apply --check "$patch"; then
        echo "error: AdaptiveCpp soft-fp 2 integration patch does not apply to pinned source" >&2
        exit 1
    fi
    echo "Applying AdaptiveCpp soft-fp 2 package integration patch"
    git -C "$ACPP_SRC" apply "$patch"
    if ! git -C "$ACPP_SRC" apply --reverse --check "$patch"; then
        echo "error: AdaptiveCpp soft-fp 2 integration patch verification failed" >&2
        exit 1
    fi
}

apply_sscp_host_coverage_patch() {
    local patch marker target
    local targets=(
        "include/hipSYCL/runtime/metal/metal_code_object.hpp"
        "src/compiler/llvm-to-backend/metal/Emitter.cpp"
        "src/compiler/llvm-to-backend/metal/Emitter.hpp"
        "src/compiler/sscp/TargetSeparationPass.cpp"
        "src/runtime/metal/metal_code_object.cpp"
        "src/runtime/metal/metal_queue.cpp"
    )
    patch="$PG_ACCEL_REPO_ROOT/patches/adaptivecpp/sscp-host-coverage.patch"
    marker="lowerDeviceProfileInstrumentation"

    if [ ! -s "$patch" ]; then
        echo "error: required AdaptiveCpp SSCP coverage patch is missing" >&2
        exit 1
    fi
    for target in "${targets[@]}"; do
        if [ ! -f "$ACPP_SRC/$target" ]; then
            echo "error: required AdaptiveCpp SSCP coverage target is missing: $target" >&2
            exit 1
        fi
    done

    if grep -q "$marker" \
        "$ACPP_SRC/src/compiler/sscp/TargetSeparationPass.cpp"; then
        if ! git -C "$ACPP_SRC" apply --unidiff-zero --reverse --check "$patch"; then
            echo "error: applied AdaptiveCpp SSCP coverage patch has drifted" >&2
            exit 1
        fi
        return 0
    fi

    for target in "${targets[@]}"; do
        if ! git -C "$ACPP_SRC" diff --quiet -- "$target"; then
            echo "error: AdaptiveCpp SSCP coverage target differs from pinned source before patching: $target" >&2
            exit 1
        fi
    done
    if ! git -C "$ACPP_SRC" apply --unidiff-zero --check "$patch"; then
        echo "error: AdaptiveCpp SSCP coverage patch does not apply to pinned source" >&2
        exit 1
    fi
    echo "Applying AdaptiveCpp SSCP host and Metal device coverage patch"
    git -C "$ACPP_SRC" apply --unidiff-zero "$patch"
    if ! git -C "$ACPP_SRC" apply --unidiff-zero --reverse --check "$patch"; then
        echo "error: AdaptiveCpp SSCP coverage patch verification failed" >&2
        exit 1
    fi
}

apply_metal_cpp_compat_patch
apply_default_targets_json_patch
apply_sleef_helper_address_space_patch
apply_soft_fp_2_package_patch
apply_sscp_host_coverage_patch

unapply_soft_fp64_device_patch() {
    [ -d "$SOFT_FP64_SRC/.git" ] || return 0

    local header
    header="$SOFT_FP64_SRC/src/sleef/sleef_internal.h"
    if [ -f "$header" ] && grep -q 'SF64_DEVICE_CONSTEXPR_BITCAST' "$header"; then
        if [ ! -s "$SOFT_FP64_DEVICE_PATCH" ] ||
            ! git -C "$SOFT_FP64_SRC" apply --reverse --check "$SOFT_FP64_DEVICE_PATCH"; then
            echo "error: applied soft-fp Metal compatibility patch has drifted" >&2
            exit 1
        fi
        git -C "$SOFT_FP64_SRC" apply --reverse "$SOFT_FP64_DEVICE_PATCH"
    fi
}

apply_soft_fp64_device_patch() {
    [ "$backend" = "metal" ] || return 0

    if [ ! -s "$SOFT_FP64_DEVICE_PATCH" ]; then
        echo "error: required soft-fp Metal compatibility patch is missing" >&2
        exit 1
    fi
    if ! git -C "$SOFT_FP64_SRC" apply --check "$SOFT_FP64_DEVICE_PATCH"; then
        echo "error: soft-fp Metal compatibility patch does not apply to $SOFT_FP64_REQUIRED_TAG" >&2
        exit 1
    fi
    echo "Applying soft-fp Metal constexpr-bitcast compatibility patch"
    git -C "$SOFT_FP64_SRC" apply "$SOFT_FP64_DEVICE_PATCH"
    if ! git -C "$SOFT_FP64_SRC" apply --reverse --check "$SOFT_FP64_DEVICE_PATCH"; then
        echo "error: soft-fp Metal compatibility patch verification failed" >&2
        exit 1
    fi
    SOFT_FP64_APPLIED_PATCH="$SOFT_FP64_DEVICE_PATCH_RELATIVE"
}

if [ ! -d "$SOFT_FP64_SRC/.git" ]; then
    git clone --depth 1 --branch "$SOFT_FP64_REQUIRED_TAG" https://github.com/yocontra/soft-fp.git "$SOFT_FP64_SRC"
else
    unapply_soft_fp64_device_patch
    if ! git -C "$SOFT_FP64_SRC" diff --quiet ||
        ! git -C "$SOFT_FP64_SRC" diff --cached --quiet; then
        echo "error: soft-fp checkout has tracked local changes: $SOFT_FP64_SRC" >&2
        exit 1
    fi
    git -C "$SOFT_FP64_SRC" fetch --depth 1 origin \
        "refs/tags/$SOFT_FP64_REQUIRED_TAG:refs/tags/$SOFT_FP64_REQUIRED_TAG"
    git -C "$SOFT_FP64_SRC" -c advice.detachedHead=false checkout --detach \
        "$SOFT_FP64_REQUIRED_TAG^{commit}"
fi
soft_fp64_desc="$(git -C "$SOFT_FP64_SRC" describe --tags --exact-match HEAD)"
if [ "$soft_fp64_desc" != "$SOFT_FP64_REQUIRED_TAG" ]; then
    echo "error: soft-fp64 at $soft_fp64_desc, expected $SOFT_FP64_REQUIRED_TAG" >&2
    exit 1
fi
SOFT_FP64_HEAD="$(git -C "$SOFT_FP64_SRC" rev-parse HEAD)"
echo "Using soft-fp64 $soft_fp64_desc ($SOFT_FP64_HEAD)"
apply_soft_fp64_device_patch

if [ "$backend" = "metal" ]; then
    SOFT_FP64_CMAKE_ARGS=(
        -DCMAKE_BUILD_TYPE=Release
        -DCMAKE_INSTALL_PREFIX="$SOFT_FP64_PREFIX"
        -DSOFT_FP_BUILD_FP128=OFF
        -DSOFT_FP_BUILD_FP256=OFF
        -DSOFT_FP64_BUILD_TESTS=OFF
        -DSOFT_FP64_BUILD_EXHAUSTIVE=OFF
        -DSOFT_FP64_BUILD_FUZZ=OFF
        -DSOFT_FP64_BUILD_BENCH=OFF
        -DSOFT_FP64_WERROR=ON
        -DSOFT_FP64_INSTALL=ON
        -DSOFT_FP64_OCL=on
        -DSOFT_FP64_FTZ=off
        -DSOFT_FP64_FENV=disabled
        -DSOFT_FP64_SNAN=quiet
        "${ACPP_AUTO_CMAKE_ARGS[@]}"
    )
    if [ -n "${LLVM_PREFIX:-}" ]; then
        SOFT_FP64_CMAKE_ARGS+=(
            -DCMAKE_C_COMPILER="$LLVM_PREFIX/bin/clang"
            -DCMAKE_CXX_COMPILER="$LLVM_PREFIX/bin/clang++"
        )
    fi
    cmake -S "$SOFT_FP64_SRC" -B "$SOFT_FP64_BUILD_DIR" \
        "${SOFT_FP64_CMAKE_ARGS[@]}" "${ACPP_REQUIRED_CMAKE_ARGS[@]}"
    cmake --build "$SOFT_FP64_BUILD_DIR" --target install --parallel \
        "${SOFT_FP64_BUILD_JOBS:-${ACPP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}}"
    SOFT_FP64_PACKAGE_DIR="$SOFT_FP64_PREFIX/lib/cmake/soft_fp64"
    SOFT_FP_PACKAGE_DIR="$SOFT_FP64_PREFIX/lib/cmake/soft_fp"
    soft_fp64_config="$SOFT_FP64_PREFIX/include/soft_fp64/config.h"
    if [ ! -f "$SOFT_FP64_PACKAGE_DIR/soft_fp64Config.cmake" ] ||
        [ ! -f "$SOFT_FP_PACKAGE_DIR/soft_fpConfig.cmake" ] ||
        [ ! -f "$soft_fp64_config" ]; then
        echo "error: configured soft-fp 2 package is incomplete: $SOFT_FP64_PREFIX" >&2
        exit 1
    fi
    for expected_contract in \
        '#define SOFT_FP64_VERSION_MAJOR 2' \
        '#define SOFT_FP64_VERSION_MINOR 0' \
        '#define SOFT_FP64_VERSION_PATCH 0' \
        '#define SOFT_FP_BUILD_FP128 0' \
        '#define SOFT_FP_BUILD_FP256 0' \
        '#define SOFT_FP64_HAS_OCL_ABI 1' \
        '#define SOFT_FP64_FENV_MODE 0' \
        '#define SOFT_FP64_SNAN_PROPAGATE 0' \
        '#define SOFT_FP64_FTZ_MODE 0'; do
        if ! grep -Fqx "$expected_contract" "$soft_fp64_config"; then
            echo "error: configured soft-fp64 contract is missing: $expected_contract" >&2
            exit 1
        fi
    done
    if ! grep -Fq 'sleef_special.cpp' \
        "$SOFT_FP64_PACKAGE_DIR/soft_fp64Config.cmake" ||
        grep -Fq 'sleef_stubs.cpp' \
            "$SOFT_FP64_PACKAGE_DIR/soft_fp64Config.cmake"; then
        echo "error: configured soft-fp64 source manifest does not match v2" >&2
        exit 1
    fi
fi

common_args=(
    -DCMAKE_BUILD_TYPE=Release
    -DCMAKE_INSTALL_PREFIX="$ACPP_PREFIX"
    -DACPP_COMPILER_FEATURE_PROFILE=full
    -DBUILD_CLANG_PLUGIN=ON
    -DACPP_SOFT_FP64_SRC_DIR="$SOFT_FP64_SRC"
    "${ACPP_AUTO_CMAKE_ARGS[@]}"
)

if [ "$backend" = "metal" ]; then
    common_args+=(
        -DACPP_SOFT_FP64_PACKAGE_DIR="$SOFT_FP64_PACKAGE_DIR"
    )
fi

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

# ACPP_CMAKE_FLAGS is intentionally a caller-provided shell-style flag list.
# shellcheck disable=SC2086
cmake -S "$ACPP_SRC" -B "$ACPP_BUILD_DIR" \
    "${common_args[@]}" ${ACPP_CMAKE_FLAGS:-} "${ACPP_REQUIRED_CMAKE_ARGS[@]}"
cmake --build "$ACPP_BUILD_DIR" --target install --parallel "${ACPP_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

if [ "$backend" = "metal" ]; then
    metal_bitcode="$ACPP_PREFIX/lib/hipSYCL/bitcode/libkernel-sscp-metal-full.bc"
    if [ ! -f "$metal_bitcode" ]; then
        echo "error: installed AdaptiveCpp Metal libkernel bitcode is missing" >&2
        exit 1
    fi
    if "$LLVM_PREFIX/bin/llvm-dis" "$metal_bitcode" -o - |
        awk '/^@llvm\.global_ctors = appending/ { found = 1 } END { exit(found ? 0 : 1) }'; then
        echo "error: soft-fp introduced unsupported global constructors into Metal device bitcode" >&2
        exit 1
    fi
fi

provenance_file="$ACPP_PREFIX/pg_accel-acpp-provenance.txt"
{
    echo "backend=$backend"
    echo "targets=$resolved_targets"
    echo "acpp_required_sha=$ACPP_REQUIRED_SHA"
    echo "acpp_head=$ACPP_HEAD"
    echo "acpp_src=$ACPP_SRC"
    echo "soft_fp64_required_tag=$SOFT_FP64_REQUIRED_TAG"
    echo "soft_fp64_desc=$soft_fp64_desc"
    echo "soft_fp64_head=$SOFT_FP64_HEAD"
    echo "soft_fp64_package_version=$SOFT_FP64_PACKAGE_VERSION"
    echo "soft_fp64_device_patch=$SOFT_FP64_APPLIED_PATCH"
    echo "soft_fp64_src=$SOFT_FP64_SRC"
    echo "soft_fp64_build_dir=$SOFT_FP64_BUILD_DIR"
    echo "soft_fp64_install_prefix=$SOFT_FP64_PREFIX"
    echo "soft_fp_package_dir=$SOFT_FP_PACKAGE_DIR"
    echo "soft_fp64_package_dir=$SOFT_FP64_PACKAGE_DIR"
    printf 'soft_fp64_cmake_args='
    printf '%s ' "${SOFT_FP64_CMAKE_ARGS[@]}"
    printf '%s\n' "${ACPP_REQUIRED_CMAKE_ARGS[@]}"
    echo "soft_fp64_git_status_start"
    git -C "$SOFT_FP64_SRC" status --short || true
    echo "soft_fp64_git_status_end"
    echo "llvm_prefix=${LLVM_PREFIX:-}"
    echo "acpp_lld_path=${ACPP_LLD_PATH:-}"
    echo "macos_sdk_path=${ACPP_MACOS_SDK_PATH:-}"
    echo "metal_include_dir=${METAL_INCLUDE_DIR:-$PG_ACCEL_TOOL_ROOT/metal-cpp}"
    echo "cmake_build_dir=$ACPP_BUILD_DIR"
    echo "cmake_install_prefix=$ACPP_PREFIX"
    printf 'cmake_args='
    printf '%s ' "${common_args[@]}"
    # Preserve the same intentional flag-list splitting in provenance.
    # shellcheck disable=SC2086
    printf '%s ' ${ACPP_CMAKE_FLAGS:-}
    printf '%s\n' "${ACPP_REQUIRED_CMAKE_ARGS[@]}"
    echo "acpp_git_status_start"
    git -C "$ACPP_SRC" status --short || true
    echo "acpp_git_status_end"
} > "$provenance_file"

mkdir -p "$PG_ACCEL_TOOL_ROOT/acpp"
ln -sfn "$ACPP_PREFIX" "$PG_ACCEL_TOOL_ROOT/acpp/current"
printf '%s\n' "$resolved_targets" > "$PG_ACCEL_TOOL_ROOT/acpp/current-targets"
"$ACPP_PREFIX/bin/acpp" --acpp-version | grep -q "plugin-with-sscp-compiler: true"
"$ACPP_PREFIX/bin/acpp-info" | sed -n '1,80p'
echo "AdaptiveCpp $backend toolchain installed at $ACPP_PREFIX"
echo "AdaptiveCpp provenance written to $provenance_file"
