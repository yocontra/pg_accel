# Default: list commands
default:
    @just --list

# === Setup ===

# Install all dependencies (run once on fresh clone)
setup: setup-tools setup-brew setup-pgrx
    @echo "Setup complete. Run 'just setup-gpu' if you want GPU acceleration."

# Install asdf-managed tools (rust, cmake)
setup-tools:
    @command -v asdf > /dev/null || (echo "Install asdf first: https://asdf-vm.com" && exit 1)
    asdf plugin add rust 2>/dev/null || true
    asdf plugin add cmake 2>/dev/null || true
    asdf plugin add just 2>/dev/null || true
    asdf install

# Install Homebrew dependencies
setup-brew:
    brew install postgresql@17
    cargo install cargo-pgrx --locked
    cargo install cargo-deny --locked

# Initialize pgrx for PG 17
setup-pgrx:
    cargo pgrx init --pg17 $(brew --prefix postgresql@17)/bin/pg_config

# Install AdaptiveCpp with Metal backend (macOS Apple Silicon)
setup-gpu: setup-gpu-deps setup-gpu-metal-headers setup-gpu-acpp
    @echo "GPU setup complete. Run 'acpp-info' to verify."

# Install GPU build dependencies via Homebrew
setup-gpu-deps:
    brew install llvm@20 lld@20 boost

# Download Apple metal-cpp headers
setup-gpu-metal-headers:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -f "{{env('HOME')}}/local/include/Metal/Metal.hpp" ]; then
        echo "metal-cpp headers already installed"
        exit 0
    fi
    echo "Downloading Apple metal-cpp headers..."
    python3 -c "
    import urllib.request, zipfile, os
    urllib.request.urlretrieve(
        'https://developer.apple.com/metal/cpp/files/metal-cpp_macOS15.2_iOS18.2.zip',
        '/tmp/metal-cpp.zip')
    with zipfile.ZipFile('/tmp/metal-cpp.zip', 'r') as z:
        z.extractall('/tmp/metal-cpp')
    "
    mkdir -p ~/local/include
    cp -r /tmp/metal-cpp/Metal ~/local/include/
    cp -r /tmp/metal-cpp/Foundation ~/local/include/
    cp -r /tmp/metal-cpp/QuartzCore ~/local/include/
    rm -rf /tmp/metal-cpp /tmp/metal-cpp.zip
    echo "metal-cpp headers installed to ~/local/include"

# Build and install AdaptiveCpp from source
setup-gpu-acpp:
    #!/usr/bin/env bash
    set -euo pipefail
    ACPP_PREFIX="$HOME/local"
    if [ -f "$ACPP_PREFIX/bin/acpp-info" ]; then
        echo "AdaptiveCpp already installed at $ACPP_PREFIX"
        "$ACPP_PREFIX/bin/acpp-info" | head -8
        exit 0
    fi
    echo "Building AdaptiveCpp (this takes a few minutes)..."
    cd /tmp
    rm -rf AdaptiveCpp
    git clone --depth 1 --branch develop https://github.com/AdaptiveCpp/AdaptiveCpp.git
    mkdir -p AdaptiveCpp/build && cd AdaptiveCpp/build
    LLVM_PREFIX=$(brew --prefix llvm@20)
    cmake \
        -DCMAKE_INSTALL_PREFIX="$ACPP_PREFIX" \
        -DWITH_METAL_BACKEND=ON \
        -DWITH_OPENCL_BACKEND=OFF \
        -DWITH_LEVEL_ZERO_BACKEND=OFF \
        -DWITH_CUDA_BACKEND=OFF \
        -DWITH_ROCM_BACKEND=OFF \
        -DWITH_SSCP_COMPILER=ON \
        -DMETAL_INCLUDE_DIR="$ACPP_PREFIX/include" \
        -DACPP_LLD_PATH=$(brew --prefix lld@20)/bin/ld64.lld \
        -DCLANG_EXECUTABLE_PATH="$LLVM_PREFIX/bin/clang++" \
        -DLLVM_DIR="$LLVM_PREFIX/lib/cmake/llvm" \
        -DCMAKE_C_COMPILER="$LLVM_PREFIX/bin/clang" \
        -DCMAKE_CXX_COMPILER="$LLVM_PREFIX/bin/clang++" \
        -DCMAKE_OSX_SYSROOT="$(xcrun --sdk macosx --show-sdk-path)" \
        '-DDEFAULT_TARGETS=omp;metal' \
        ..
    make -j1
    make install
    rm -rf /tmp/AdaptiveCpp
    echo "AdaptiveCpp installed to $ACPP_PREFIX"
    "$ACPP_PREFIX/bin/acpp-info" | head -8

# === Development ===

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt -- --check

# Run clippy lints
lint:
    cargo clippy -- -D warnings

# Type check (pg17 + all non-pg features)
check:
    cargo check --features pg17

# Type check with GPU feature
check-gpu:
    cargo check --features gpu

# Run cargo-deny checks (licenses + advisories)
deny:
    cargo deny check

# Pre-commit checks: fmt, lint, type-check, deny
pre-commit: fmt-check lint check deny
    @echo "Pre-commit checks passed."

# Run pgrx unit tests against PG 17
test-unit pg="17":
    cargo pgrx test pg{{pg}}

# Run SQL integration tests against the dev Docker environment
test-integration db="pgaccel_shared":
    PGPASSWORD=pgaccel_test docker/tests/run_all.sh "host=localhost port=5488 user=postgres dbname={{db}}"

# Run all tests: pgrx unit tests + SQL integration tests
test pg="17": (test-unit pg) test-integration
    @echo "All tests passed."

# Run benchmark suite against dev Docker environment
bench rows="1000000" iterations="30" warmup="5" connection="host=localhost port=5488 user=postgres password=pgaccel_test dbname=pgaccel_a9":
    cargo run -p pg_accel_bench --release -- run \
        --rows {{rows}} --iterations {{iterations}} --warmup {{warmup}} \
        --connection "{{connection}}" --format markdown

# === Docker Dev Environment ===

# Start dev PG (PostGIS + h3 + pg_accel, 10 agent DBs)
dev-up:
    docker compose -f docker/docker-compose.test.yml up -d --build
    @echo "Waiting for PG to be ready..."
    @until docker compose -f docker/docker-compose.test.yml exec -T pgaccel-test pg_isready -U postgres > /dev/null 2>&1; do sleep 1; done
    @echo "Dev environment ready on port 5488"

# Stop dev PG and remove volumes
dev-down:
    docker compose -f docker/docker-compose.test.yml down -v

# Watch for source changes and hot-reload (run in separate terminal)
dev-watch:
    docker/scripts/dev_reload.sh

# Run integration tests for a specific agent
dev-test agent="0":
    docker/scripts/run_agent_tests.sh {{agent}}

# Run all integration tests (all agents)
dev-test-all:
    docker/scripts/run_integration_tests.sh

# Run comprehensive integration test suite against dev PG
dev-test-suite db="pgaccel_shared":
    docker/tests/run_all.sh "host=localhost port=5488 user=postgres dbname={{db}}"

# Connect to an agent's database via psql
dev-psql agent="0":
    psql -h localhost -p 5488 -U postgres pgaccel_a{{agent}}

# Reset an agent's database to clean fixtures
dev-reset agent="0":
    psql -h localhost -p 5488 -U postgres -c "DROP DATABASE IF EXISTS pgaccel_a{{agent}};" -c "CREATE DATABASE pgaccel_a{{agent}} TEMPLATE pgaccel_shared;"

# === GPU Kernels ===

acpp_prefix := env("HOME") + "/local"

# Build GPU kernel library
gpu-build:
    cmake -B pgaccel-kernels/build -S pgaccel-kernels \
        -DPGACCEL_USE_SYCL=ON \
        -DCMAKE_PREFIX_PATH={{acpp_prefix}} \
        -DACPP_TARGETS="omp" \
        -DCMAKE_C_COMPILER=$(brew --prefix llvm@20)/bin/clang \
        -DCMAKE_CXX_COMPILER=$(brew --prefix llvm@20)/bin/clang++ \
        -DCMAKE_CXX_FLAGS="-I$(brew --prefix libomp)/include" \
        -DCMAKE_SHARED_LINKER_FLAGS="-L$(brew --prefix libomp)/lib" \
        -DCMAKE_EXE_LINKER_FLAGS="-L$(brew --prefix libomp)/lib" \
    && cmake --build pgaccel-kernels/build

# Run GPU kernel tests
gpu-test: gpu-build
    ./pgaccel-kernels/build/test_device \
    && ./pgaccel-kernels/build/test_bbox \
    && ./pgaccel-kernels/build/test_spatial \
    && ./pgaccel-kernels/build/test_spatial_dispatch \
    && ./pgaccel-kernels/build/test_h3 \
    && ./pgaccel-kernels/build/test_raster \
    && ./pgaccel-kernels/build/test_correctness

# === CI ===

# Run full CI locally (pre-commit checks + all tests)
ci: pre-commit dev-up test-unit test-integration dev-down

# Build installable pgrx package
package pg="17":
    cargo pgrx package --pg-config $(which pg_config) --features pg{{pg}}
