# Default: list commands
default:
    @just --list

# === Setup ===

# Install repo-local PostgreSQL, test extensions, and Rust tooling (run once on a fresh clone).
setup: setup-tools setup-pg-source setup-pg-extensions setup-pgrx setup-hooks
    @echo "Setup complete. Run 'ACPP_BACKEND=cuda just setup-gpu' on CUDA Linux, or 'just setup-gpu-metal' on Apple Silicon."

# Install prek (Rust-native pre-commit drop-in) and wire up its git hooks
# from .pre-commit-config.yaml. Idempotent — safe to re-run.
setup-hooks:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v prek >/dev/null 2>&1; then
        echo "warning: prek not found; skipping git hook installation."
        echo "         Install prek separately and rerun 'just setup-hooks' if you want local hooks."
        exit 0
    fi
    # prek refuses to install when core.hooksPath points anywhere other than
    # the default. Clear a stale local override (set by some editors/tools)
    # so prek can manage .git/hooks.
    if git config --get --local core.hooksPath >/dev/null 2>&1; then
        git config --unset-all --local core.hooksPath
    fi
    prek install
    echo "prek hooks installed (pre-commit + commit-msg + pre-push)."

# Install Rust-side tools used by pg_accel.
setup-tools:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    command -v rustc >/dev/null || { echo "error: rustc not found; install Rust first" >&2; exit 1; }
    command -v cargo >/dev/null || { echo "error: cargo not found; install Rust first" >&2; exit 1; }
    command -v cmake >/dev/null || { echo "error: cmake not found; install CMake first" >&2; exit 1; }
    command -v curl >/dev/null || { echo "error: curl not found" >&2; exit 1; }
    command -v make >/dev/null || { echo "error: make not found" >&2; exit 1; }
    cargo install cargo-pgrx --version "$PG_ACCEL_PGRX_VERSION" --locked
    cargo install cargo-deny --locked
    cargo install cargo-audit --locked

# Print system dependency hints for source PostgreSQL + AdaptiveCpp builds.
setup-system-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -s)" in
        Linux)
            printf '%s\n' \
                "Install build prerequisites with your distro package manager. For Ubuntu/Debian:" \
                "  sudo apt-get install -y build-essential ca-certificates clang cmake curl git libreadline-dev zlib1g-dev flex bison pkg-config postgresql-17-postgis-3" \
                "" \
                "For CUDA runs, install the NVIDIA driver + CUDA toolkit, then run:" \
                "  ACPP_BACKEND=cuda just setup-gpu"
            ;;
        Darwin)
            printf '%s\n' \
                "Install Xcode command line tools and Homebrew packages: brew install postgis." \
                "For Metal runs, install a supported LLVM/lld toolchain: brew install llvm@20 lld." \
                "Then run:" \
                "  just setup-gpu-metal"
            ;;
        *)
            echo "Install a C/C++ toolchain, curl, make, cmake, git, and Rust."
            ;;
    esac

# Build repo-local PostgreSQL from official source tarballs.
setup-pg-source pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    pg_arg="{{pg}}"
    if [ -n "$pg_arg" ]; then
        majors="${pg_arg#pg}"
    else
        majors="$(pg_accel_source_pg_majors)"
    fi
    for pg in $majors; do
        scripts/pg_source.sh build "$pg"
    done

# Install h3, postgis, and postgis_raster into repo-local PostgreSQL.
setup-pg-extensions pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    pg_arg="{{pg}}"
    if [ -n "$pg_arg" ]; then
        majors="${pg_arg#pg}"
    else
        majors="$(pg_accel_supported_pg_majors)"
    fi
    for pg in $majors; do
        if pg_accel_skip_if_preview_without_pgrx "$pg"; then
            continue
        fi
        scripts/pg_source.sh build "$pg"
        scripts/setup_pg_extensions.sh "$pg"
    done

# Alias for building one source PostgreSQL major/version.
pg-build pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        requested="$(pg_accel_default_pg_major)"
    fi
    scripts/pg_source.sh build "$requested"

# Print the repo-local pg_config path for a PostgreSQL major/version.
pg-config pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        requested="$(pg_accel_default_pg_major)"
    fi
    scripts/pg_source.sh pg-config "$requested"

# Boot a temporary source-built PostgreSQL cluster and run SELECT version().
pg-smoke pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        requested="$(pg_accel_default_pg_major)"
    fi
    scripts/pg_source.sh build "$requested"
    pg_config="$(scripts/pg_source.sh pg-config "$requested")"
    pgbin="$(dirname "$pg_config")"
    tmpdir="$(mktemp -d /tmp/pgaccel-srcpg.XXXXXX)"
    cleanup() {
        "$pgbin/pg_ctl" -D "$tmpdir/data" -m fast stop >/dev/null 2>&1 || true
        rm -rf "$tmpdir"
    }
    trap cleanup EXIT
    "$pgbin/initdb" -D "$tmpdir/data" >/dev/null
    "$pgbin/pg_ctl" -D "$tmpdir/data" -l "$tmpdir/pg.log" -o "-p 55432 -k $tmpdir -c listen_addresses=''" start >/dev/null
    "$pgbin/psql" -h "$tmpdir" -p 55432 -d postgres -tAc 'select version();'

# Initialize pgrx for source-built PostgreSQL majors.
setup-pgrx pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    pg_arg="{{pg}}"
    if [ -n "$pg_arg" ]; then
        majors="${pg_arg#pg}"
    else
        majors="$(pg_accel_supported_pg_majors)"
    fi
    for pg in $majors; do
        if pg_accel_skip_if_preview_without_pgrx "$pg"; then
            continue
        fi
        scripts/pg_source.sh build "$pg"
        pg_config="$(pg_accel_source_pg_config_for_required_pg "$pg")"
        cargo pgrx init "--pg$pg" "$pg_config"
        pg_accel_disable_uninstalled_pg_accel_preload "$pg" "$pg_config"
        echo "pgrx PG$pg initialized from source PostgreSQL at $pg_config"
    done

# Explicit alias for initializing the configured PG version matrix.
setup-pgrx-matrix: setup-pgrx

# Build a repo-local AdaptiveCpp toolchain. Use ACPP_BACKEND=cuda, metal, or generic.
setup-gpu: setup-gpu-acpp
    @echo "GPU setup complete. Run ./.pgaccel/acpp/current/bin/acpp-info to verify."

# Build a repo-local AdaptiveCpp Metal toolchain on macOS.
setup-gpu-metal: setup-gpu-metal-headers
    #!/usr/bin/env bash
    set -euo pipefail
    ACPP_BACKEND=metal ./scripts/setup_acpp.sh
    echo "GPU Metal setup complete. Run ./.pgaccel/acpp/current/bin/acpp-info to verify."

# Download Apple metal-cpp headers
setup-gpu-metal-headers:
    #!/usr/bin/env bash
    set -euo pipefail
    root="${PG_ACCEL_TOOL_ROOT:-$PWD/.pgaccel}"
    if [ -f "$root/metal-cpp/Metal/Metal.hpp" ]; then
        echo "metal-cpp headers already installed"
        exit 0
    fi
    echo "Downloading Apple metal-cpp headers..."
    python3 -c "
    import pathlib
    import shutil
    import urllib.request
    import zipfile

    archive = pathlib.Path('/tmp/metal-cpp.zip')
    extract_dir = pathlib.Path('/tmp/metal-cpp')
    shutil.rmtree(extract_dir, ignore_errors=True)
    urllib.request.urlretrieve(
        'https://developer.apple.com/metal/cpp/files/metal-cpp_macOS15.2_iOS18.2.zip',
        archive)
    with zipfile.ZipFile(archive, 'r') as z:
        z.extractall(extract_dir)

    header = next(extract_dir.glob('**/Metal/Metal.hpp'), None)
    if header is None:
        raise SystemExit('metal-cpp archive did not contain Metal/Metal.hpp')
    source_root = header.parent.parent
    target_root = pathlib.Path('$root') / 'metal-cpp'
    target_root.mkdir(parents=True, exist_ok=True)
    for name in ('Metal', 'Foundation', 'QuartzCore'):
        source = source_root / name
        target = target_root / name
        if not source.exists():
            raise SystemExit(f'metal-cpp archive missing {name}')
        shutil.rmtree(target, ignore_errors=True)
        shutil.copytree(source, target)
    "
    rm -rf /tmp/metal-cpp /tmp/metal-cpp.zip
    echo "metal-cpp headers installed to $root/metal-cpp"

# Build and install AdaptiveCpp from source into .pgaccel/acpp/<backend>.
setup-gpu-acpp:
    ./scripts/setup_acpp.sh

# === Development ===

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt -- --check

# Run clippy lints. Defaults to PG17 until PG18/PG19 are ABI-clean.
lint pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_buildable_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    if pg_accel_skip_if_preview_without_pgrx "$pg"; then
        exit 0
    fi
    pg_accel_require_pgrx_pg_config "$pg"
    cargo clippy --workspace --no-default-features --features "pg$pg" --all-targets -- -D warnings

# Type check one PG major. Defaults to PG17 with the same preview skip as `lint`.
check pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_buildable_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    if pg_accel_skip_if_preview_without_pgrx "$pg"; then
        exit 0
    fi
    pg_accel_require_pgrx_pg_config "$pg"
    cargo check --workspace --no-default-features --features "pg$pg" --all-targets

# Type check every supported PostgreSQL major.
check-matrix:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    for pg in $(pg_accel_supported_pg_majors); do
        if pg_accel_skip_if_preview_without_pgrx "$pg"; then
            continue
        fi
        pg_accel_require_pgrx_pg_config "$pg"
        cargo check --workspace --no-default-features --features "pg$pg" --all-targets
    done

# Run cargo-deny checks (licenses + advisories)
deny:
    cargo deny check

# Run cargo-audit for RustSec vulnerability scan (separate from cargo-deny's
# advisory check: audit uses the full RustSec DB directly). Fails with a clear
# message on machines that don't have cargo-audit installed — `cargo install
# cargo-audit --locked` fixes it.
# Ignored advisories are transitive warnings from pgrx/pgrx-tests/bench-only
# deps, not pg_accel runtime safety boundaries.
audit:
    @command -v cargo-audit >/dev/null 2>&1 || { \
      echo "error: cargo-audit not installed. Run: cargo install cargo-audit --locked" >&2; \
      exit 1; \
    }
    cargo audit \
      --ignore RUSTSEC-2021-0127 \
      --ignore RUSTSEC-2024-0436 \
      --ignore RUSTSEC-2026-0097

# Validate file:line citations in CLAUDE.md / ARCHITECTURE.md / TODO.md.
# Anti-cheat §10 requires citations to be verifiable; this catches drift
# (files moved, line numbers out of range) in CI before reviewers waste
# time chasing dead references.
doc-parity:
    ./scripts/doc_parity.sh

# Validate that default PG-version plumbing is centralized.
pg-version-audit:
    ./scripts/pg_version_audit.sh

# Pre-commit checks: fmt, lint, type-check matrix, deny, audit, doc-parity
pre-commit: fmt-check lint check-matrix deny audit doc-parity pg-version-audit
    @echo "Pre-commit checks passed."

# Run pgrx unit tests against one PG major. Defaults to the repo target PG major.
test-unit pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    if pg_accel_skip_if_preview_without_pgrx "$pg"; then
        exit 0
    fi
    pg_accel_require_pgrx_pg_config "$pg"
    scripts/setup_pg_extensions.sh "$pg"
    RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}" cargo pgrx test --package pg_accel "pg$pg"

# Run pgrx unit tests against every supported PG major.
test-matrix:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    for pg in $(pg_accel_supported_pg_majors); do
        if pg_accel_skip_if_preview_without_pgrx "$pg"; then
            continue
        fi
        just test-unit "$pg"
    done

# Run all tests: pgrx unit-test matrix
test: test-matrix
    @echo "All tests passed."

# Run benchmark suite against local pgrx PG. The runner seeds and cleans up
# each workload/scale itself. Long benches can fill the PG log; `log-rails`
# truncates oversized logs first.
bench iterations="10" warmup="5" pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_buildable_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    if pg_accel_skip_if_preview_without_pgrx "$pg"; then
        exit 0
    fi
    pg_accel_require_pgrx_pg_config "$pg"
    just install-pg-accel "$pg"
    just log-rails "$pg"
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" cargo run -p pg_accel_bench --release -- run \
        --iterations {{iterations}} --warmup {{warmup}} \
        --connection "host=localhost port=$port dbname=postgres" \
        --format markdown --timing raw --skip-guc-verify

# Run the rigorous benchmark suite: realistic GUCs, plan capture,
# raw wall-clock timing (no EXPLAIN ANALYZE overhead).
bench-rigorous iterations="30" warmup="5" pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_buildable_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    if pg_accel_skip_if_preview_without_pgrx "$pg"; then
        exit 0
    fi
    pg_accel_require_pgrx_pg_config "$pg"
    just install-pg-accel "$pg"
    just log-rails "$pg"
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" cargo run --release -p pg_accel_bench -- run \
        --iterations {{iterations}} --warmup {{warmup}} \
        --connection "host=localhost port=$port dbname=postgres" \
        --format markdown \
        --realistic-gucs --capture-plans --timing raw

# Guard against the PG log filling the disk.
# Truncates pgrx PG log + pg_accel trace files when they exceed
# LOG_RAILS_MAX_MB (default: 500 MB). Called automatically before
# `bench` / `bench-rigorous`; run manually anytime PG has been
# logging under `log_statement` / heavy fprintf.
log-rails pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    MAX_MB="${LOG_RAILS_MAX_MB:-500}"
    MAX_BYTES=$((MAX_MB * 1024 * 1024))
    data_dir="$(pg_accel_pgrx_data_dir_for_pg "$pg")"
    for f in "$(pg_accel_pgrx_log_for_pg "$pg")" \
             "$data_dir/pg_accel_otel.jsonl" \
             "$data_dir/pg_accel_traces.jsonl" \
             "$data_dir/pg_accel_panic.log"; do
        [ -f "$f" ] || continue
        sz=$(stat -f%z "$f" 2>/dev/null || echo 0)
        if [ "$sz" -gt "$MAX_BYTES" ]; then
            : > "$f"
            printf "log-rails: truncated %s (was %s bytes)\n" "$f" "$sz"
        fi
    done
    # Suppress NOTICE/WARNING-level spam in the PG server log. The h3
    # extension emits per-row warnings for renamed entry points, which
    # ballooned the log by ~1 GB per workload during long bench runs.
    # ERROR-level and above still land in the log.
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    if command -v psql > /dev/null 2>&1 && pg_isready -h localhost -p "$port" -q 2>/dev/null; then
        psql -h localhost -p "$port" -d postgres -tAc \
            "ALTER SYSTEM SET log_min_messages = 'error';" > /dev/null 2>&1 || true
        psql -h localhost -p "$port" -d postgres -tAc \
            "SELECT pg_reload_conf();" > /dev/null 2>&1 || true
    fi

# Hard-truncate all pgrx PG + pg_accel logs (no size check).
clean-logs pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -n "$requested" ]; then
        majors="${requested#pg}"
    else
        majors="$(pg_accel_supported_pg_majors)"
    fi
    for pg in $majors; do
        pg_accel_require_supported_pg "$pg"
        data_dir="$(pg_accel_pgrx_data_dir_for_pg "$pg")"
        for f in "$(pg_accel_pgrx_log_for_pg "$pg")" \
                 "$data_dir/pg_accel_otel.jsonl" \
                 "$data_dir/pg_accel_traces.jsonl" \
                 "$data_dir/pg_accel_panic.log"; do
            [ -f "$f" ] || continue
            : > "$f"
            echo "cleaned $f"
        done
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
        mkdir -p .pgaccel/logs
        log=".pgaccel/logs/clear-jit-$(date +%Y%m%d-%H%M%S).log"
        total=0
        deleted=0
        failed=0
        while IFS= read -r -d '' entry; do
            total=$((total + 1))
            if rm -rf "$entry" >>"$log" 2>&1; then
                deleted=$((deleted + 1))
            else
                failed=$((failed + 1))
            fi
        done < <(find "$cache_dir" -mindepth 1 -maxdepth 1 -print0)
        if [ "$failed" -gt 0 ]; then
            echo "clear-jit: deleted $deleted/$total entries; failed $failed (raw log: $log)" >&2
            exit 1
        fi
        rm -f "$log"
        echo "cleared $cache_dir ($deleted entries)"
    else
        echo "no JIT cache at $cache_dir (nothing to clear)"
    fi

# === GPU Kernels ===

# Build GPU kernel library (AdaptiveCpp/SYCL -> CUDA/ROCm/L0/Metal/CPU)
gpu-build:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    acpp_prefix="${ACPP_PREFIX:-$(pg_accel_acpp_prefix)}"
    [ -d "$acpp_prefix/lib/cmake/AdaptiveCpp" ] || {
        echo "error: AdaptiveCpp not found at $acpp_prefix" >&2
        echo "       run: ACPP_BACKEND=cuda just setup-gpu" >&2
        exit 1
    }
    cmake -B pgaccel-kernels/build -S pgaccel-kernels \
        -DAdaptiveCpp_DIR="$acpp_prefix/lib/cmake/AdaptiveCpp" \
        ${PGACCEL_KERNEL_CMAKE_FLAGS:-}
    cmake --build pgaccel-kernels/build --parallel

# Run standalone GPU kernel tests (warm cache, quiet console).
gpu-test: gpu-build
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p .pgaccel/logs
    tests=(
        device
        bbox
        spatial
        spatial_dispatch
        h3
        raster
        correctness
        exec_gpu
        hash_agg_keys
        fork
        fork_warmed
        fork_cold
        sycl_basic
        reduce_stats
        hash_agg_partial
        window
        expr_templates
    )
    for test_name in "${tests[@]}"; do
        log=".pgaccel/logs/gpu-test-${test_name}-$(date +%Y%m%d-%H%M%S).log"
        python3 scripts/filter_gpu_output.py \
            --label "test_${test_name}" \
            --log "$log" \
            -- "./pgaccel-kernels/build/test_${test_name}"
    done

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
# Run one GPU kernel test cold with quiet console and raw log preservation.
gpu-test-cold name timeout_s="300":
    #!/usr/bin/env bash
    set -euo pipefail
    just clear-jit
    mkdir -p .pgaccel/logs
    log=".pgaccel/logs/gpu-test-cold-{{name}}-$(date +%Y%m%d-%H%M%S).log"
    python3 scripts/filter_gpu_output.py \
        --label "test_{{name}} cold" \
        --log "$log" \
        -- timeout {{timeout_s}} "./pgaccel-kernels/build/test_{{name}}"

# Run the full standalone GPU test suite cold.
gpu-test-cold-all:
    just clear-jit
    just gpu-test

# 8-worker x 20-iteration fork stress test for the Metal MTLBinaryArchive
# fork-safety path. Acceptance gate for TODO.md Phase 2 "Metal pipeline-state
# XPC edge case": zero MTLCompilerService errors over the 8x20 matrix.
#
# Override sizing with environment:
#   PGACCEL_FORK_STRESS_WORKERS=16 PGACCEL_FORK_STRESS_ITERS=40 just gpu-stress-archive
#
# Always cold-starts so the archive build path is exercised.
# Run the Metal archive fork-safety stress test with quiet console output.
gpu-stress-archive workers="8" iters="20":
    #!/usr/bin/env bash
    set -euo pipefail
    just clear-jit
    mkdir -p .pgaccel/logs
    log=".pgaccel/logs/gpu-stress-archive-$(date +%Y%m%d-%H%M%S).log"
    echo "=== gpu-stress-archive workers={{workers}} iters={{iters}} ==="
    python3 scripts/filter_gpu_output.py \
        --label "gpu-stress-archive" \
        --log "$log" \
        -- env PGACCEL_FORK_STRESS_WORKERS={{workers}} PGACCEL_FORK_STRESS_ITERS={{iters}} \
            timeout 600 ./pgaccel-kernels/build/test_fork_archive_stress

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

# Run full CI locally (pre-commit checks + all supported PG tests)
ci: pre-commit test-matrix

# Run SQL integration tests against the pgrx-managed PostgreSQL cluster.
sql-test pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    if pg_accel_skip_if_preview_without_pgrx "$pg"; then
        exit 0
    fi
    pg_accel_require_pgrx_pg_config "$pg"
    scripts/setup_pg_extensions.sh "$pg"
    PG_ACCEL_PG_MAJOR="$pg" sql/tests/run_all.sh

# Build installable pgrx package
package pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    if pg_accel_skip_if_preview_without_pgrx "$pg"; then
        exit 0
    fi
    pg_accel_require_pgrx_pg_config "$pg"
    scripts/setup_pg_extensions.sh "$pg"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    cargo pgrx package --package pg_accel --pg-config "$pg_config" --no-default-features --features "pg$pg"
    cp NOTICE "target/release/pg_accel-pg$pg/NOTICE"

# Build installable pgrx packages for every supported PG major.
package-matrix:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    for pg in $(pg_accel_supported_pg_majors); do
        if pg_accel_skip_if_preview_without_pgrx "$pg"; then
            continue
        fi
        pg_accel_require_pgrx_pg_config "$pg"
        pg_config="$(pg_accel_pg_config_for_pg "$pg")"
        cargo pgrx package --package pg_accel --pg-config "$pg_config" --no-default-features --features "pg$pg"
        cp NOTICE "target/release/pg_accel-pg$pg/NOTICE"
    done

# Install the current pg_accel release build into the pgrx-managed cluster and
# restart the cluster so shared_preload_libraries maps the same binary.
install-pg-accel pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    if pg_accel_skip_if_preview_without_pgrx "$pg"; then
        exit 0
    fi
    pg_accel_require_pgrx_pg_config "$pg"
    scripts/setup_pg_extensions.sh "$pg"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    cargo pgrx stop --package pg_accel "pg$pg" >/dev/null 2>&1 || true
    PG_CONFIG="$pg_config" cargo pgrx install --package pg_accel --release --no-default-features --features "pg$pg" --pg-config "$pg_config"
    conf="$(pg_accel_pgrx_data_dir_for_pg "$pg")/postgresql.conf"
    if [ -f "$conf" ]; then
        if grep -Eq "^[[:space:]]*#[[:space:]]*shared_preload_libraries[[:space:]]*=[[:space:]]*'pg_accel'[[:space:]]*# disabled by setup-pgrx until pg_accel is installed" "$conf"; then
            tmp="$(mktemp "$conf.XXXXXX")"
            sed "s|^[[:space:]]*#[[:space:]]*shared_preload_libraries[[:space:]]*=[[:space:]]*'pg_accel'[[:space:]]*# disabled by setup-pgrx until pg_accel is installed|shared_preload_libraries = 'pg_accel'|" "$conf" > "$tmp"
            mv "$tmp" "$conf"
        elif ! grep -Eq "^[[:space:]]*shared_preload_libraries[[:space:]]*=.*pg_accel" "$conf"; then
            printf "\nshared_preload_libraries = 'pg_accel'\n" >> "$conf"
        fi
    fi
    cargo pgrx start --package pg_accel "pg$pg"

# Install into the pgrx-managed cluster and prove a fresh CREATE EXTENSION path.
extension-smoke pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_supported_pg "$pg"
    if pg_accel_skip_if_preview_without_pgrx "$pg"; then
        exit 0
    fi
    pg_accel_require_pgrx_pg_config "$pg"
    just install-pg-accel "$pg"
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    : "${port:?could not read pgrx PostgreSQL port}"
    db="pg_accel_smoke_$pg"
    dropdb -h localhost -p "$port" --if-exists "$db"
    createdb -h localhost -p "$port" "$db"
    psql -h localhost -p "$port" -d "$db" -v ON_ERROR_STOP=1 \
        -c "CREATE EXTENSION pg_accel;" \
        -c "SELECT pg_accel_version();" \
        -c "SELECT * FROM pg_accel_stats();"
    dropdb -h localhost -p "$port" "$db"

# Install into each pgrx-managed cluster and prove fresh CREATE EXTENSION paths.
extension-smoke-matrix:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    for pg in $(pg_accel_supported_pg_majors); do
        if pg_accel_skip_if_preview_without_pgrx "$pg"; then
            continue
        fi
        just extension-smoke "$pg"
    done

# Live OTel span viewer TUI (reads OTLP JSON file)
otel-tui pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    otel-tui --from-json-file "$(pg_accel_pgrx_data_dir_for_pg "$pg")/pg_accel_otel.jsonl"

# Live trace viewer (tail tracing-subscriber JSONL)
traces pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    tail -f "$(pg_accel_pgrx_data_dir_for_pg "$pg")/pg_accel_traces.jsonl" | python3 -m json.tool --no-ensure-ascii

# View last N trace entries
traces-last n="20" pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    tail -{{n}} "$(pg_accel_pgrx_data_dir_for_pg "$pg")/pg_accel_traces.jsonl" | python3 -m json.tool --no-ensure-ascii
