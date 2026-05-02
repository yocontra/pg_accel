# Default: list commands
default:
    @just --list

# === Setup ===

# Install all dependencies (run once on fresh clone)
setup: setup-tools setup-brew setup-pgrx setup-hooks
    @echo "Setup complete. Run 'just setup-gpu' if you want GPU acceleration."

# Install prek (Rust-native pre-commit drop-in) and wire up its git hooks
# from .pre-commit-config.yaml. Idempotent — safe to re-run.
setup-hooks:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v prek >/dev/null 2>&1; then
        echo "Installing prek via Homebrew..."
        brew install prek
    fi
    # prek refuses to install when core.hooksPath points anywhere other than
    # the default. Clear a stale local override (set by some editors/tools)
    # so prek can manage .git/hooks.
    if git config --get --local core.hooksPath >/dev/null 2>&1; then
        git config --unset-all --local core.hooksPath
    fi
    prek install
    echo "prek hooks installed (pre-commit + commit-msg + pre-push)."

# Install asdf-managed tools (rust, cmake)
setup-tools:
    @command -v asdf > /dev/null || (echo "Install asdf first: https://asdf-vm.com" && exit 1)
    asdf plugin add rust 2>/dev/null || true
    asdf plugin add cmake 2>/dev/null || true
    asdf plugin add just 2>/dev/null || true
    asdf install

# Install Homebrew dependencies (including PostGIS + h3 for pgrx)
setup-brew:
    brew install postgresql@17 postgis h3
    cargo install cargo-pgrx --locked
    cargo install cargo-deny --locked

# Initialize pgrx for PG 17 and link all required extensions into it
setup-pgrx:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo pgrx init --pg17 "$(brew --prefix postgresql@17)/bin/pg_config"

    # Symlink PostGIS, h3, and other extensions into pgrx-managed PG.
    # Without this, CREATE EXTENSION fails inside the pgrx instance.
    PGRX_LIB="$HOME/.pgrx/17.9/pgrx-install/lib/postgresql"
    PGRX_EXT="$HOME/.pgrx/17.9/pgrx-install/share/postgresql/extension"

    # PostGIS
    for f in "$(brew --prefix postgis)"/lib/postgresql@17/*.dylib; do
        ln -sf "$f" "$PGRX_LIB/$(basename "$f")"
    done
    for f in "$(brew --prefix postgis)"/share/postgresql@17/extension/*; do
        ln -sf "$f" "$PGRX_EXT/$(basename "$f")"
    done

    # h3 + h3_postgis
    for f in /opt/homebrew/lib/postgresql@17/h3*.dylib; do
        ln -sf "$f" "$PGRX_LIB/$(basename "$f")"
    done
    for f in /opt/homebrew/share/postgresql@17/extension/h3*; do
        ln -sf "$f" "$PGRX_EXT/$(basename "$f")"
    done

    echo "pgrx PG17 initialized with postgis, postgis_raster, h3, h3_postgis"

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

# Build and install AdaptiveCpp from the local fork-safe-metal checkout at $ACPP_SRC
setup-gpu-acpp:
    #!/usr/bin/env bash
    set -euo pipefail
    ACPP_PREFIX="$HOME/local"
    ACPP_SRC="${ACPP_SRC:-$HOME/Projects/AdaptiveCpp}"
    REQUIRED_BRANCH="fork-safe-metal"
    if [ ! -d "$ACPP_SRC/.git" ]; then
        echo "$ACPP_SRC not found; cloning fork-safe-metal from yocontra/AdaptiveCpp"
        git clone -b "$REQUIRED_BRANCH" https://github.com/yocontra/AdaptiveCpp.git "$ACPP_SRC"
    fi
    ACPP_REQUIRED_SHA="4f3cde11a302eebac28aa1ccc79ad3399cb8183c"
    if ! git -C "$ACPP_SRC" merge-base --is-ancestor "$ACPP_REQUIRED_SHA" HEAD 2>/dev/null; then
        echo "error: AdaptiveCpp at $ACPP_SRC must include SHA $ACPP_REQUIRED_SHA"
        echo "       run: git -C $ACPP_SRC fetch origin fork-safe-metal && git -C $ACPP_SRC checkout fork-safe-metal && git -C $ACPP_SRC pull --ff-only"
        exit 1
    fi
    LLVM_PREFIX=$(brew --prefix llvm@20)
    # soft-fp64 is consumed directly from its source tree by AdaptiveCpp's
    # libkernel build (no flatten-and-stage step — the staging adapter was
    # removed in soft-fp64 v1.2.0 and absorbed into AdaptiveCpp's own
    # `src/libkernel/sscp/metal/float64/`). All we need is a checkout at
    # the pinned tag — AdaptiveCpp's CMake reads `src/`, `src/sleef/`,
    # `include/` directly via `ACPP_SOFT_FP64_SRC_DIR`.
    SOFT_FP64_SRC="${SOFT_FP64_SRC:-$HOME/Projects/soft-fp64}"
    SOFT_FP64_REQUIRED_TAG="v1.2.0"
    if [ ! -d "$SOFT_FP64_SRC/.git" ]; then
        git clone --depth 1 --branch "$SOFT_FP64_REQUIRED_TAG" \
            https://github.com/yocontra/soft-fp.git "$SOFT_FP64_SRC"
    fi
    SOFT_FP64_DESC="$(git -C "$SOFT_FP64_SRC" describe --tags --always)"
    if [ "$SOFT_FP64_DESC" != "$SOFT_FP64_REQUIRED_TAG" ]; then
        echo "error: soft-fp64 at '$SOFT_FP64_DESC', expected '$SOFT_FP64_REQUIRED_TAG'"
        echo "       run: git -C $SOFT_FP64_SRC fetch --tags && git -C $SOFT_FP64_SRC checkout $SOFT_FP64_REQUIRED_TAG"
        exit 1
    fi
    test -f "$SOFT_FP64_SRC/include/soft_fp64/soft_f64.h" \
        || { echo "soft-fp64 src layout broken at $SOFT_FP64_SRC"; exit 1; }
    mkdir -p "$ACPP_SRC/build"
    cd "$ACPP_SRC/build"
    cmake \
        -DCMAKE_INSTALL_PREFIX="$ACPP_PREFIX" \
        -DWITH_METAL_BACKEND=ON \
        -DWITH_OPENCL_BACKEND=OFF \
        -DWITH_LEVEL_ZERO_BACKEND=OFF \
        -DWITH_CUDA_BACKEND=OFF \
        -DWITH_ROCM_BACKEND=OFF \
        -DWITH_SSCP_COMPILER=ON \
        -DBUILD_CLANG_PLUGIN=ON \
        -DMETAL_INCLUDE_DIR="$ACPP_PREFIX/include" \
        -DACPP_LLD_PATH=$(brew --prefix lld@20)/bin/ld64.lld \
        -DCLANG_EXECUTABLE_PATH="$LLVM_PREFIX/bin/clang++" \
        -DLLVM_DIR="$LLVM_PREFIX/lib/cmake/llvm" \
        -DCMAKE_C_COMPILER="$LLVM_PREFIX/bin/clang" \
        -DCMAKE_CXX_COMPILER="$LLVM_PREFIX/bin/clang++" \
        -DCMAKE_OSX_SYSROOT="$(xcrun --sdk macosx --show-sdk-path)" \
        -DDEFAULT_TARGETS=generic \
        -DACPP_SOFT_FP64_SRC_DIR="$SOFT_FP64_SRC" \
        ..
    make -j4
    make install
    echo "AdaptiveCpp ($REQUIRED_BRANCH) installed to $ACPP_PREFIX from $ACPP_SRC"
    "$ACPP_PREFIX/bin/acpp-info" | awk 'NR<=8'
    # Confirm SSCP + archive helper are in place — without these the fork
    # path breaks silently. Better to fail here than at first bench crash.
    "$ACPP_PREFIX/bin/acpp" --acpp-version | grep -q "plugin-with-sscp-compiler: true" || {
        echo "ERROR: acpp was built without SSCP; pg_accel requires --acpp-targets=generic"
        exit 1
    }
    [ -x "$ACPP_PREFIX/bin/acpp-metal-archive-build" ] || {
        echo "ERROR: acpp-metal-archive-build helper missing; fork-safety fix did not install"
        exit 1
    }

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

# Run cargo-deny checks (licenses + advisories)
deny:
    cargo deny check

# Run cargo-audit for RustSec vulnerability scan (separate from cargo-deny's
# advisory check: audit uses the full RustSec DB directly). Fails with a clear
# message on machines that don't have cargo-audit installed — `cargo install
# cargo-audit --locked` fixes it.
audit:
    @command -v cargo-audit >/dev/null 2>&1 || { \
      echo "error: cargo-audit not installed. Run: cargo install cargo-audit --locked" >&2; \
      exit 1; \
    }
    cargo audit

# Validate file:line citations in CLAUDE.md / ARCHITECTURE.md / TODO.md.
# Anti-cheat §10 requires citations to be verifiable; this catches drift
# (files moved, line numbers out of range) in CI before reviewers waste
# time chasing dead references.
doc-parity:
    ./scripts/doc_parity.sh

# Pre-commit checks: fmt, lint, type-check, deny, audit, doc-parity
pre-commit: fmt-check lint check deny audit doc-parity
    @echo "Pre-commit checks passed."

# Run pgrx unit tests against PG 17
test-unit pg="17":
    cargo pgrx test --package pg_accel pg{{pg}}

# Run all tests: pgrx unit tests
test pg="17": (test-unit pg)
    @echo "All tests passed."

# Run benchmark suite against local pgrx PG. Seeds data (1M rows) and runs
# all workloads. Long benches can fill the PG log; `log-rails` truncates
# oversized logs first.
bench iterations="10" warmup="5": log-rails
    cargo run -p pg_accel_bench --release -- setup \
        --rows 1000000 \
        --connection "host=localhost port=28817 dbname=postgres"
    cargo run -p pg_accel_bench --release -- run \
        --iterations {{iterations}} --warmup {{warmup}} \
        --connection "host=localhost port=28817 dbname=postgres" \
        --format markdown --timing raw --skip-guc-verify

# Run the rigorous benchmark suite: realistic GUCs, plan capture,
# raw wall-clock timing (no EXPLAIN ANALYZE overhead).
bench-rigorous iterations="30" warmup="5": log-rails
    cargo run --release -p pg_accel_bench -- run \
        --iterations {{iterations}} --warmup {{warmup}} \
        --connection "host=localhost port=28817 dbname=postgres" \
        --format markdown \
        --realistic-gucs --capture-plans --timing raw

# Guard against the PG log filling the disk.
# Truncates pgrx PG log + pg_accel trace files when they exceed
# LOG_RAILS_MAX_MB (default: 500 MB). Called automatically before
# `bench` / `bench-rigorous`; run manually anytime PG has been
# logging under `log_statement` / heavy fprintf.
log-rails:
    #!/usr/bin/env bash
    set -euo pipefail
    MAX_MB="${LOG_RAILS_MAX_MB:-500}"
    MAX_BYTES=$((MAX_MB * 1024 * 1024))
    for f in "$HOME/.pgrx/17.log" \
             "$HOME/.pgrx/data-17/pg_accel_otel.jsonl" \
             "$HOME/.pgrx/data-17/pg_accel_traces.jsonl" \
             "$HOME/.pgrx/data-17/pg_accel_panic.log"; do
        [ -f "$f" ] || continue
        sz=$(stat -f%z "$f" 2>/dev/null || echo 0)
        if [ "$sz" -gt "$MAX_BYTES" ]; then
            : > "$f"
            printf "log-rails: truncated %s (was %s bytes)\n" "$f" "$sz"
        fi
    done
    # Suppress NOTICE/WARNING-level spam in the PG server log. The h3
    # extension emits a deprecation WARNING per-row for legacy function
    # names, which ballooned the log by ~1 GB per workload during long
    # bench runs. ERROR-level and above still land in the log.
    if command -v psql > /dev/null 2>&1 && pg_isready -h localhost -p 28817 -q 2>/dev/null; then
        psql -h localhost -p 28817 -d postgres -tAc \
            "ALTER SYSTEM SET log_min_messages = 'error';" > /dev/null 2>&1 || true
        psql -h localhost -p 28817 -d postgres -tAc \
            "SELECT pg_reload_conf();" > /dev/null 2>&1 || true
    fi

# Hard-truncate all pgrx PG + pg_accel logs (no size check).
clean-logs:
    #!/usr/bin/env bash
    set -euo pipefail
    for f in "$HOME/.pgrx/17.log" \
             "$HOME/.pgrx/data-17/pg_accel_otel.jsonl" \
             "$HOME/.pgrx/data-17/pg_accel_traces.jsonl" \
             "$HOME/.pgrx/data-17/pg_accel_panic.log"; do
        [ -f "$f" ] || continue
        : > "$f"
        echo "cleaned $f"
    done

# Clear the AdaptiveCpp Metal SSCP JIT cache. Forces a cold-cache run on
# the next kernel dispatch — useful when verifying fork-safety, the
# `.metalar` archive path, or that a kernel-side change actually
# rebuilds. Does NOT touch the kernel cache index files; AdaptiveCpp
# rebuilds those on demand.
#
# Use this instead of `rm -rf ~/.acpp/apps/global/jit-cache/*` so the
# command is auto-allowed by the harness (the bare rm prompts each
# time for permission).
clear-jit:
    #!/usr/bin/env bash
    set -euo pipefail
    cache_dir="$HOME/.acpp/apps/global/jit-cache"
    if [ -d "$cache_dir" ]; then
        find "$cache_dir" -mindepth 1 -delete
        echo "cleared $cache_dir"
    else
        echo "no JIT cache at $cache_dir (nothing to clear)"
    fi

# === GPU Kernels ===

# Build GPU kernel library (AdaptiveCpp/SYCL -> CUDA/ROCm/L0/Metal/CPU)
gpu-build:
    cmake -B pgaccel-kernels/build -S pgaccel-kernels \
        -DCMAKE_PREFIX_PATH="$HOME/local" \
        -DCMAKE_C_COMPILER=$(brew --prefix llvm@20)/bin/clang \
        -DCMAKE_CXX_COMPILER=$(brew --prefix llvm@20)/bin/clang++ \
        -DCMAKE_CXX_FLAGS="-I$(brew --prefix libomp)/include" \
        -DCMAKE_SHARED_LINKER_FLAGS="-L$(brew --prefix libomp)/lib" \
        -DCMAKE_EXE_LINKER_FLAGS="-L$(brew --prefix libomp)/lib" \
    && cmake --build pgaccel-kernels/build --parallel

# Run GPU kernel tests
gpu-test: gpu-build
    ./pgaccel-kernels/build/test_device \
    && ./pgaccel-kernels/build/test_bbox \
    && ./pgaccel-kernels/build/test_spatial \
    && ./pgaccel-kernels/build/test_spatial_dispatch \
    && ./pgaccel-kernels/build/test_h3 \
    && ./pgaccel-kernels/build/test_raster \
    && ./pgaccel-kernels/build/test_correctness \
    && ./pgaccel-kernels/build/test_exec_gpu \
    && ./pgaccel-kernels/build/test_fork \
    && ./pgaccel-kernels/build/test_fork_warmed \
    && ./pgaccel-kernels/build/test_fork_cold \
    && ./pgaccel-kernels/build/test_sycl_basic \
    && ./pgaccel-kernels/build/test_reduce_stats

# Wipe the AdaptiveCpp Metal SSCP JIT cache, then run a single named
# kernel test binary with a 5-minute timeout. Use this from the
# autonomous loop / agents instead of stringing together a bare
# `rm -rf ~/.acpp/...` and `./test_X` — those forms each prompt the
# harness for permission separately and break the loop. This recipe is
# allowlisted as a single `just gpu-test-cold` invocation.
#
# Usage:
#   just gpu-test-cold spatial         # runs test_spatial cold
#   just gpu-test-cold correctness     # runs test_correctness cold
#   just gpu-test-cold spatial 60      # custom timeout in seconds
#
# Output is teed to /tmp/gpu-test-cold-<name>.log and the FAIL/Results
# summary is grep'd at the end so you can see pass/fail without
# scrolling through JIT compile warnings.
gpu-test-cold name timeout_s="300":
    #!/usr/bin/env bash
    set -euo pipefail
    just clear-jit
    log="/tmp/gpu-test-cold-{{name}}.log"
    cd pgaccel-kernels/build
    timeout {{timeout_s}} ./test_{{name}} >"$log" 2>&1 || rc=$?
    echo "--- summary (last 15 + Results/FAIL) ---"
    tail -15 "$log" || true
    echo "---"
    grep -E "Results:|FAIL:" "$log" || echo "(no Results/FAIL lines found)"
    exit "${rc:-0}"

# Wipe JIT cache and run the full standalone GPU test suite cold.
# Same single-invocation, single-prompt benefit as gpu-test-cold for the
# whole suite. Use this for "is the kernel layer healthy" checks
# between large changes.
gpu-test-cold-all: clear-jit gpu-test

# Audit pgaccel-kernels/src/*.cpp for `extern "C" pgaccel_*` symbols
# that are labelled GPU-accelerated but whose body is a host-side
# `for` loop with no `q.submit` / `parallel_for` / sycl_ helper call.
# This is the post-cheat-audit hygiene check called out in TODO.md
# Next Up — re-run after every kernel-layer change.
#
# A clean run prints only `pgaccel_shutdown` (queue teardown — not a
# kernel; whitelist below). Any other match is a CPU cheat that
# violates CLAUDE.md rules 11 / 12 and must be either converted to
# SYCL or surfaced as PGACCEL_ERROR_NO_DEVICE.
audit-cpu-cheats:
    #!/usr/bin/env bash
    set -euo pipefail
    found_any=0
    for f in pgaccel-kernels/src/*.cpp; do
        matches=$(awk '
            /^extern "C" pgaccel_status pgaccel_/ {
                name = $0; in_fn = 1; body = ""; start_line = NR
            }
            in_fn { body = body "\n" $0 }
            in_fn && /^}/ {
                in_fn = 0
                if (body !~ /q\.submit|q->submit|parallel_for|return PGACCEL_ERROR|sycl_/) {
                    if (name !~ /pgaccel_shutdown/) {
                        print FILENAME ":" start_line " — " name
                    }
                }
                body = ""
            }
        ' "$f")
        if [ -n "$matches" ]; then
            echo "$matches"
            found_any=1
        fi
    done
    if [ "$found_any" -eq 0 ]; then
        echo "audit-cpu-cheats: PASS — every extern \"C\" pgaccel_* symbol dispatches via SYCL."
    else
        echo "audit-cpu-cheats: FAIL — see hits above. CLAUDE.md rules 11/12 violated."
        exit 1
    fi

# === CI ===

# Run full CI locally (pre-commit checks + all tests)
ci: pre-commit test-unit

# Build installable pgrx package
package pg="17":
    cargo pgrx package --pg-config $(which pg_config) --features pg{{pg}}

# Live OTel span viewer TUI (reads OTLP JSON file)
otel-tui:
    otel-tui --from-json-file ~/.pgrx/data-17/pg_accel_otel.jsonl

# Live trace viewer (tail tracing-subscriber JSONL)
traces:
    tail -f ~/.pgrx/data-17/pg_accel_traces.jsonl | python3 -m json.tool --no-ensure-ascii

# View last N trace entries
traces-last n="20":
    tail -{{n}} ~/.pgrx/data-17/pg_accel_traces.jsonl | python3 -m json.tool --no-ensure-ascii
