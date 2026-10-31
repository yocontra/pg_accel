#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
export PGRX_HOME="$PWD/.codex/scratch/dense-planning-diagnostic-20261030/pgrx"
export PG_CONFIG="$PWD/.codex/scratch/dense-planning-diagnostic-20261030/postgres18/bin/pg_config" PGRX_PG_CONFIG_PATH="$PWD/.codex/scratch/dense-planning-diagnostic-20261030/postgres18/bin/pg_config"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)" BINDGEN_EXTRA_CLANG_ARGS="-isysroot $(xcrun --show-sdk-path)"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 15m cargo clippy --locked -p pg_accel --no-default-features --features pg18 --all-targets -- -D warnings
