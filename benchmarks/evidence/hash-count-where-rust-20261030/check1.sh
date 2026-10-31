#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
run="$PWD/.codex/scratch/hash-count-where-rust-20261030/check1"
test ! -e "$run"
mkdir "$run"
trap 'result=$?; printf "terminal=%s\n" "$result" > "$run/terminal.log"' EXIT
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
base="$PWD/.codex/scratch/dense-planning-diagnostic-20261030"
export PGRX_HOME="$base/pgrx" PG_CONFIG="$base/postgres18/bin/pg_config" PGRX_PG_CONFIG_PATH="$base/postgres18/bin/pg_config"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)" BINDGEN_EXTRA_CLANG_ARGS="-isysroot $(xcrun --show-sdk-path)"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 20m cargo test --locked -p pg_accel --lib --no-default-features --features pg18 --no-run > "$run/build.log" 2>&1
