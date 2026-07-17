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
    cargo install cargo-llvm-cov --locked
    rustup component add llvm-tools-preview

# Print system dependency hints for source PostgreSQL + AdaptiveCpp builds.
setup-system-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$(uname -s)" in
        Linux)
            printf '%s\n' \
                "Install build prerequisites with your distro package manager. For Ubuntu/Debian:" \
                "  sudo apt-get install -y build-essential ca-certificates clang cmake curl git libreadline-dev zlib1g-dev flex bison pkg-config postgis" \
                "" \
                "For CUDA runs, install the NVIDIA driver + CUDA toolkit, then run:" \
                "  ACPP_BACKEND=cuda just setup-gpu"
            ;;
        Darwin)
            printf '%s\n' \
                "Install Xcode command line tools and Homebrew packages:" \
                "  brew install llvm@20 lld@20 libomp boost postgis" \
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
        pg_accel_require_pgrx_support "$pg"
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
        pg_accel_require_pgrx_support "$pg"
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

# Run clippy lints. Defaults to the active supported PostgreSQL major.
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
    pg_accel_require_pgrx_support "$pg"
    pg_accel_require_pgrx_pg_config "$pg"
    cargo clippy --workspace --no-default-features --features "pg$pg" --all-targets -- -D warnings

# Type check one PG major. Defaults to the active supported PostgreSQL major.
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
    pg_accel_require_pgrx_support "$pg"
    pg_accel_require_pgrx_pg_config "$pg"
    cargo check --workspace --no-default-features --features "pg$pg" --all-targets

# Type check every supported PostgreSQL major.
check-matrix:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    for pg in $(pg_accel_supported_pg_majors); do
        pg_accel_require_pgrx_support "$pg"
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
#
# The ignored-advisory set is single-sourced from deny.toml's
# [advisories].ignore table (the authoritative list, with per-advisory reason
# comments) so cargo-audit and cargo-deny can never drift. Ignored advisories
# are transitive warnings from pgrx/opentelemetry/bench-only deps, not
# pg_accel runtime safety boundaries.
audit:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! cargo audit --version >/dev/null 2>&1; then
        echo "error: cargo-audit is not installed or is not runnable for the active Rust toolchain. Run: cargo install cargo-audit --locked" >&2
        exit 1
    fi
    ignore_args=()
    while IFS= read -r advisory_id; do
        ignore_args+=(--ignore "$advisory_id")
    done < <(grep -oE 'RUSTSEC-[0-9]{4}-[0-9]{4}' deny.toml | sort -u)
    if [ "${#ignore_args[@]}" -eq 0 ]; then
        echo "error: no ignored advisories parsed from deny.toml [advisories].ignore" >&2
        exit 1
    fi
    cargo audit "${ignore_args[@]}"

# Validate exact citations, released GUC semantics, registered adapter names,
# the macOS prerequisite set, and the production planner-capability matrix
# across authoritative docs. Run focused adversarial parser tests in the same
# CI gate.
doc-parity:
    ./scripts/doc_parity.sh
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/tests/test_doc_parity.py

# Validate that default PG-version plumbing is centralized.
pg-version-audit:
    ./scripts/pg_version_audit.sh
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/tests/test_pg_source.py

# Validate relocatable package layout, loader metadata, and archive preservation.
package-extension-test:
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/tests/test_package_extension.py -v

# Pre-commit checks: fmt, lint, type-check matrix, deny, audits, doc-parity
pre-commit: fmt-check lint check-matrix deny audit doc-parity pg-version-audit package-extension-test audit-cpu-cheats-test metal-stress-artifact-test
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
    pg_accel_require_pgrx_support "$pg"
    pg_accel_require_pgrx_pg_config "$pg"
    scripts/setup_pg_extensions.sh "$pg"
    RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}" cargo pgrx test --package pg_accel "pg$pg"

# Run pgrx unit tests against every supported PG major.
test-matrix:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    for pg in $(pg_accel_supported_pg_majors); do
        pg_accel_require_pgrx_support "$pg"
        just test-unit "$pg"
    done

# Run all tests: pgrx unit-test matrix
test: test-matrix
    @echo "All tests passed."

# Run the fail-closed Rust production-line, C++/SYCL host-object-line, and SQL
# semantic-assertion gate for one PG major. This starts PostgreSQL and runs the
# complete registered GPU CTest suite, so it requires a qualified Metal runner.
coverage pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    bash scripts/coverage_gate.sh "$pg"

# Validate fixed coverage scopes, the SQL assertion manifest, parsers, aggregate
# negative cases, and shell syntax without starting PostgreSQL or a GPU device.
coverage-audit:
    bash -n scripts/coverage_gate.sh sql/tests/run_all.sh
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p 'test_coverage_tools.py'
    python3 scripts/coverage_tools.py audit-scope --scope coverage/scope.json --repo-root .

# Run the immutable qualified-Metal performance ratchet. The Rust command fixes
# the exact 1M-row winner cells, seed, raw timing, cache-mode-both policy,
# sampling counts, and per-lane thresholds; this recipe supplies only the PG
# installation and optional deterministic CI artifact path.
metal-benchmark-ship-gate pg="" artifacts_dir="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_buildable_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_pgrx_support "$pg"
    pg_accel_require_pgrx_pg_config "$pg"
    just audit-cpu-cheats
    just install-pg-accel "$pg"
    just log-rails "$pg"
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    artifact_args=()
    if [ -n "{{artifacts_dir}}" ]; then
        artifact_args=(--artifacts-dir "{{artifacts_dir}}")
    fi
    PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" cargo run --release -p pg_accel_bench -- \
        metal-ship-gate \
        --connection "host=localhost port=$port dbname=postgres" \
        "${artifact_args[@]}"

# Run benchmark suite against local pgrx PG. The runner seeds and cleans up
# each workload/scale itself. Long benches can fill the PG log; `log-rails`
# truncates oversized logs first.
#
# This is the EVIDENCE recipe: it does NOT pass `--skip-guc-verify`, so the
# harness hard-fails if any PGC_POSTMASTER GUC (e.g. shared_buffers) drifts
# from the requested profile. Use `just bench-dev` for local iteration where
# the postmaster GUC mismatch should be tolerated.
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
    pg_accel_require_pgrx_support "$pg"
    pg_accel_require_pgrx_pg_config "$pg"
    just install-pg-accel "$pg"
    just log-rails "$pg"
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    PG_CONFIG="$pg_config" PG_ACCEL_PG_MAJOR="$pg" cargo run -p pg_accel_bench --release -- run \
        --iterations {{iterations}} --warmup {{warmup}} \
        --connection "host=localhost port=$port dbname=postgres" \
        --format markdown --timing raw

# Developer-iteration benchmark: identical to `just bench` but passes
# `--skip-guc-verify` to bypass the postmaster-GUC mismatch hard-fail. Never
# use for published/evidence runs — a settings table that doesn't match the
# running postmaster is worse than no table at all (see main.rs --skip-guc-verify).
bench-dev iterations="10" warmup="5" pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_buildable_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_pgrx_support "$pg"
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
    pg_accel_require_pgrx_support "$pg"
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
    if [ -n "${ACPP_APPDB_DIR:-}" ]; then
        cache_dir="${ACPP_APPDB_DIR}/global/jit-cache"
    else
        cache_dir="$HOME/.acpp/apps/global/jit-cache"
    fi
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
#
# The test list is discovered from CMake's CTest registration
# (`add_pgaccel_gpu_test` in pgaccel-kernels/CMakeLists.txt) rather than a
# hand-maintained array, so new kernel test targets run automatically instead
# of being silently dropped. Each registered test is already wrapped in
# scripts/filter_gpu_output.py, which preserves the quiet console and writes a
# raw log per test under .pgaccel/logs. gpu-build reconfigures CMake first, so
# the CTest manifest is always current. Runs serially (default), warm cache
# (no clear-jit), with a per-test timeout via GPU_TEST_TIMEOUT_S.
gpu-test: gpu-build
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p .pgaccel/logs
    timeout_s="${GPU_TEST_TIMEOUT_S:-300}"
    ctest --test-dir pgaccel-kernels/build \
        --output-on-failure \
        --timeout "$timeout_s"

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
# fork-safety path. The acceptance condition is zero MTLCompilerService errors
# over the 8x20 matrix.
#
# Override sizing with environment:
#   PGACCEL_FORK_STRESS_WORKERS=16 PGACCEL_FORK_STRESS_ITERS=40 just gpu-stress-archive
#
# Always cold-starts so the archive build path is exercised.
# Run the Metal archive fork-safety stress test with quiet console output.
gpu-stress-archive workers="8" iters="20":
    #!/usr/bin/env bash
    set -euo pipefail
    cache_precleared="${PGACCEL_ARCHIVE_STRESS_CACHE_PRECLEARED:-0}"
    case "$cache_precleared" in
        0) just clear-jit ;;
        1) echo "using caller-verified empty JIT cache" ;;
        *)
            echo "error: PGACCEL_ARCHIVE_STRESS_CACHE_PRECLEARED must be 0 or 1" >&2
            exit 1
            ;;
    esac
    log="${PGACCEL_ARCHIVE_STRESS_RAW_LOG:-.pgaccel/logs/gpu-stress-archive-$(date +%Y%m%d-%H%M%S).log}"
    mkdir -p "$(dirname "$log")"
    echo "=== gpu-stress-archive workers={{workers}} iters={{iters}} ==="
    python3 scripts/filter_gpu_output.py \
        --label "gpu-stress-archive" \
        --log "$log" \
        -- env PGACCEL_FORK_STRESS_WORKERS={{workers}} PGACCEL_FORK_STRESS_ITERS={{iters}} \
            timeout 600 ./pgaccel-kernels/build/test_fork_archive_stress

# Validate the Metal stress parser and shell contract without a GPU device.
metal-stress-artifact-test:
    bash -n scripts/metal_stress_gate.sh
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/tests/test_metal_stress_artifacts.py -v
    python3 scripts/metal_stress_artifacts.py workflow-audit --path .github/workflows/release.yml

# Run the M-series Metal release stress gate with durable artifacts.
metal-stress pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    bash scripts/metal_stress_gate.sh "$pg"

# Run the NVIDIA CUDA release stress gate with durable artifacts.
cuda-stress pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    bash scripts/cuda_stress_gate.sh "$pg"

# Run the release verification matrix with durable artifacts.
release-verify pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    bash scripts/release_verification_matrix.sh "$pg"

# Fail while the v1.0 release checklist still has placeholder evidence. Set
# RELEASE_CHECKLIST_EVIDENCE_PATH to audit a completed external/tag-PR ledger.
release-checklist-audit:
    bash scripts/release_checklist_audit.sh

# Prove the analyzer against synthetic evasions and the real ABI/witness baseline.
# Assertions expect the current production audit to be nonzero, so this stays green.
audit-cpu-cheats-test:
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p 'test_cpu_cheat_audit.py' -v

# Audit every extern-C pgaccel_* definition and its public header declaration.
# Every successful compute path must be dominated by output-producing SYCL work;
# ambiguous control flow, output provenance, templates, or host finalization fail
# closed. Exact source-validated lifecycle/fail-only contracts remain auditable.
audit-cpu-cheats: audit-cpu-cheats-test
    #!/usr/bin/env bash
    set -euo pipefail
    build_marker="$(mktemp "${TMPDIR:-/tmp}/pgaccel-cpu-audit.XXXXXX")"
    trap 'rm -f "$build_marker"' EXIT
    # Delete the shared artifact first. Its reappearance after build_marker is
    # evidence that this invocation relinked the exact library being audited.
    find pgaccel-kernels/build -type f \
        \( -name 'libpgaccel_kernels_shared.dylib' -o \
           -name 'libpgaccel_kernels_shared.so' \) -delete 2>/dev/null || true
    just gpu-build
    objects="$(find pgaccel-kernels/build -type f \
        \( -name 'libpgaccel_kernels_shared.dylib' -o \
           -name 'libpgaccel_kernels_shared.so' \) -print)"
    object_count="$(printf '%s\n' "$objects" | sed '/^$/d' | wc -l | tr -d ' ')"
    if [ "$object_count" -ne 1 ]; then
        echo "error: expected exactly one built pgaccel_kernels_shared object; found $object_count" >&2
        exit 1
    fi
    report="${CPU_CHEAT_AUDIT_REPORT:-target/cpu-cheat-audit.json}"
    python3 scripts/cpu_cheat_audit.py \
        --json-report "$report" \
        --abi-manifest scripts/cpu_cheat_abi_manifest.txt \
        --objects "$objects" \
        --build-marker "$build_marker" \
        --headers pgaccel-kernels/include/*.h -- \
        pgaccel-kernels/src/*.cpp

# Explicit maintainer-only rebaseline. This does not update the literal integrity
# constants; review the manifest diff and hash before changing those constants.
update-cpu-cheat-abi-manifest:
    python3 scripts/cpu_cheat_audit.py \
        --regenerate-abi-manifest scripts/cpu_cheat_abi_manifest.txt \
        --headers \
        pgaccel-kernels/include/pgaccel_ffi.h \
        pgaccel-kernels/include/pgaccel_expr.h \
        pgaccel-kernels/include/pgaccel_fused.h \
        pgaccel-kernels/include/pgaccel_hash_agg.h \
        pgaccel-kernels/include/pgaccel_hash_join.h \
        pgaccel-kernels/include/pgaccel_nested_loop_ineq.h \
        pgaccel-kernels/include/pgaccel_olap.h \
        pgaccel-kernels/include/pgaccel_window.h

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
    pg_accel_require_pgrx_support "$pg"
    pg_accel_require_pgrx_pg_config "$pg"
    scripts/setup_pg_extensions.sh "$pg"
    just install-pg-accel "$pg"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    psql_bin="$("$pg_config" --bindir)/psql"
    if [ ! -x "$psql_bin" ]; then
        psql_bin="psql"
    fi
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    connection="host=localhost port=$port dbname=postgres"
    "$psql_bin" "$connection" -v ON_ERROR_STOP=1 -f sql/init/01-create-extensions.sql
    PG_ACCEL_PG_MAJOR="$pg" PG_ACCEL_SQL_TEST_REQUIRE_EXTENSION=1 sql/tests/run_all.sh "$connection"

# Run the opt-in plan-shape + parallel-stress integration tests (gated behind
# the `integration_tests` cargo feature, so excluded from the default hermetic
# `cargo test -p pg_accel_bench`). Installs pg_accel into the pgrx-managed
# cluster, refreshes the extension SQL, then runs the live suite against it.
# `pg_accel_bench/src/integration_connection.rs` reads PG_ACCEL_TEST_CONNECTION,
# so we point it at the resolved pgrx port explicitly.
plan-shape-tests pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_pgrx_support "$pg"
    pg_accel_require_pgrx_pg_config "$pg"
    scripts/setup_pg_extensions.sh "$pg"
    just install-pg-accel "$pg"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    psql_bin="$("$pg_config" --bindir)/psql"
    if [ ! -x "$psql_bin" ]; then
        psql_bin="psql"
    fi
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    connection="host=localhost port=$port dbname=postgres"
    # The pgrx database persists across runs, and same-version installs do not
    # make newly generated SQL visible to an existing extension. Recreate only
    # pg_accel; CASCADE removes its dependent residency triggers while leaving
    # PostGIS and H3 extensions installed.
    "$psql_bin" "$connection" -v ON_ERROR_STOP=1 \
        -c "DROP EXTENSION IF EXISTS pg_accel CASCADE;" \
        -c "CREATE EXTENSION pg_accel;"
    PG_ACCEL_TEST_CONNECTION="$connection" PG_ACCEL_TEST_PG_MAJOR="$pg" \
        cargo test -p pg_accel_bench --features integration_tests -- --nocapture

# Build installable pgrx package
package pg="":
    #!/usr/bin/env bash
    set -euo pipefail
    # Packaging intentionally remains blocked until the production audit is green.
    just audit-cpu-cheats
    source scripts/pg_versions.sh
    requested="{{pg}}"
    if [ -z "$requested" ]; then
        pg="$(pg_accel_default_pg_major)"
    else
        pg="${requested#pg}"
    fi
    pg_accel_require_pgrx_support "$pg"
    pg_accel_require_pgrx_pg_config "$pg"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    acpp_prefix="$(pg_accel_acpp_prefix)"
    python3 scripts/package_extension.py \
        --pg "$pg" --pg-config "$pg_config" --acpp-prefix "$acpp_prefix"

# Build installable pgrx packages for every supported PG major.
package-matrix:
    #!/usr/bin/env bash
    set -euo pipefail
    # Run once before the matrix creates any release artifact.
    just audit-cpu-cheats
    source scripts/pg_versions.sh
    for pg in $(pg_accel_supported_pg_majors); do
        pg_accel_require_pgrx_support "$pg"
        pg_accel_require_pgrx_pg_config "$pg"
        pg_config="$(pg_accel_pg_config_for_pg "$pg")"
        acpp_prefix="$(pg_accel_acpp_prefix)"
        python3 scripts/package_extension.py \
            --pg "$pg" --pg-config "$pg_config" --acpp-prefix "$acpp_prefix"
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
    pg_accel_require_pgrx_support "$pg"
    pg_accel_require_pgrx_pg_config "$pg"
    scripts/setup_pg_extensions.sh "$pg"
    pg_config="$(pg_accel_pg_config_for_pg "$pg")"
    pkglibdir="$("$pg_config" --pkglibdir)"
    sharedir="$("$pg_config" --sharedir)"
    extension_dir="$sharedir/extension"
    pg_bindir="$("$pg_config" --bindir)"
    psql_bin="$pg_bindir/psql"
    if [ ! -x "$psql_bin" ]; then
        psql_bin="psql"
    fi
    hash_file() {
        if command -v shasum >/dev/null 2>&1; then
            shasum -a 256 "$1" | awk '{print $1}'
            return 0
        fi
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "$1" | awk '{print $1}'
            return 0
        fi
        printf 'unavailable\n'
    }
    cargo pgrx stop --package pg_accel "pg$pg" >/dev/null 2>&1 || true
    PG_CONFIG="$pg_config" cargo pgrx install --package pg_accel --release --no-default-features --features "pg$pg" --pg-config "$pg_config"
    module_file=""
    for candidate in "$pkglibdir/pg_accel.so" "$pkglibdir/pg_accel.dylib" "$pkglibdir/pg_accel.bundle"; do
        if [ -f "$candidate" ]; then
            module_file="$candidate"
            break
        fi
    done
    control_file="$extension_dir/pg_accel.control"
    if [ -d "$extension_dir" ]; then
        sql_file_count="$(find "$extension_dir" -maxdepth 1 -type f -name 'pg_accel--*.sql' 2>/dev/null | wc -l | tr -d ' ')"
    else
        sql_file_count="0"
    fi
    if [ -z "$module_file" ]; then
        echo "error: cargo pgrx install completed but pg_accel shared library was not found in $pkglibdir" >&2
        exit 1
    fi
    if [ ! -f "$control_file" ]; then
        echo "error: cargo pgrx install completed but pg_accel.control was not found at $control_file" >&2
        exit 1
    fi
    if [ "$sql_file_count" = "0" ]; then
        echo "error: cargo pgrx install completed but no pg_accel extension SQL files were found in $extension_dir" >&2
        exit 1
    fi
    default_version="$(awk -F"'" '/^[[:space:]]*default_version[[:space:]]*=/{print $2; exit}' "$control_file" 2>/dev/null || true)"
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
    port="$(pg_accel_pgrx_port_for_pg "$pg")"
    set +e
    preload="$("$psql_bin" -h localhost -p "$port" -d postgres -v ON_ERROR_STOP=1 -tAc "SHOW shared_preload_libraries;" 2>&1)"
    preload_status=$?
    set -e
    if [ "$preload_status" -ne 0 ]; then
        echo "error: installed pg_accel but could not query shared_preload_libraries on port $port" >&2
        echo "$preload" | tail -10 | sed 's/^/       | /' >&2
        exit "$preload_status"
    fi
    if ! printf '%s\n' "$preload" | grep -q 'pg_accel'; then
        echo "error: installed pg_accel but shared_preload_libraries does not include pg_accel" >&2
        echo "       shared_preload_libraries=$preload" >&2
        exit 1
    fi
    echo "install-provenance:"
    echo "  pg=$pg"
    echo "  pg_config=$pg_config"
    echo "  pg_version=$("$pg_config" --version)"
    echo "  pkglibdir=$pkglibdir"
    echo "  extension_dir=$extension_dir"
    echo "  module_file=$module_file"
    echo "  module_sha256=$(hash_file "$module_file")"
    echo "  control_file=$control_file"
    echo "  control_default_version=${default_version:-unknown}"
    echo "  extension_sql_files=$sql_file_count"
    echo "  pgrx_port=$port"
    echo "  shared_preload_libraries=$preload"
    echo "  git_head=$(git rev-parse --verify HEAD 2>/dev/null || printf 'unknown')"
    if [ -z "$(git status --porcelain 2>/dev/null)" ]; then
        echo "  git_tree=clean"
    else
        echo "  git_tree=dirty"
    fi

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
    pg_accel_require_pgrx_support "$pg"
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
        pg_accel_require_pgrx_support "$pg"
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
