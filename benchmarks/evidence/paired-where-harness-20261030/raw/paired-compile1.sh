#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
run="$PWD/.codex/scratch/hash-count-where-rust-20261030/paired-compile1"
test ! -e "$run"
mkdir "$run"
files=(pg_accel/src/engine/ffi/custom_scan/{mod,paired_driver,paired_backend,paired_runner,paired_population,paired_fixtures}.rs pg_accel/src/engine/ffi/planner_hooks/{mod,generic_groupagg}.rs pg_accel/src/engine/ffi/planner_hooks/generic_groupagg/paired_recipes.rs pg_accel/src/engine/residency/{mod,store}.rs pg_accel/src/engine/residency/store/paired.rs pg_accel/src/engine/executor/agg/{mod,descriptor,execute}.rs pg_accel/src/engine/stats.rs pg_accel/src/gpu/grouped_agg.rs)
shasum -a256 "${files[@]}" > "$run/source.sha256"
shasum -a256 "$0" > "$run/harness.sha256"
finish() {
 result=$?; trap - EXIT; set +e
 shasum -a256 -c "$run/source.sha256" > "$run/source-after.log" || result=1
 printf 'terminal=%s\n' "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
base="$PWD/.codex/scratch/dense-planning-diagnostic-20261030"
export PGRX_HOME="$base/pgrx"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)" BINDGEN_EXTRA_CLANG_ARGS="-isysroot $(xcrun --show-sdk-path)"
export PG_CONFIG="$base/postgres18/bin/pg_config" PGRX_PG_CONFIG_PATH="$base/postgres18/bin/pg_config"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features pg18,pg_test --no-run > "$run/pg18.log" 2>&1
