#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features pg18 engine::ffi::planner_hooks::shape:: -- --test-threads=1
