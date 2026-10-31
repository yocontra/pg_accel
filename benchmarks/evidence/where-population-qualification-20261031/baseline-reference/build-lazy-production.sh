#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/dense-planning-diagnostic-20261030"
major=${1:?major}
case "$major" in 18) version=18.4;;19) version=19beta1;;*) exit 2;;esac
build="$task/lazy-production$major"
test ! -e "$build"
mkdir "$build"
cp -R "$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version" "$build/postgres"
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
export PGRX_HOME="$task/pgrx" PG_CONFIG="$build/postgres/bin/pg_config" PGRX_PG_CONFIG_PATH="$build/postgres/bin/pg_config" CARGO_TARGET_DIR="$build/target"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)" BINDGEN_EXTRA_CLANG_ARGS="-isysroot $(xcrun --show-sdk-path)"
test "$("$PG_CONFIG" --pkglibdir)" = "$build/postgres/lib"
{ printf '%s\n' Cargo.toml Cargo.lock pg_accel/Cargo.toml pg_accel/build.rs pg_accel_bench/Cargo.toml pgaccel-kernels/CMakeLists.txt .acpp-version; rg --files pg_accel/src pg_accel_bench/src pgaccel-kernels/src pgaccel-kernels/include pgaccel-kernels/test; } | LC_ALL=C sort -u | while IFS= read -r file; do shasum -a256 "$file"; done > "$build/source.sha256"
shasum -a256 "$task/build-lazy-production.sh" > "$build/harness.sha256"
shasum -a256 "$task/production18/postgres/lib/pg_accel.dylib" "$task/production18/postgres/lib/libpq.5.dylib" "$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/lib/pg_accel.dylib" "$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/19beta1/lib/pg_accel.dylib" "$PWD/.codex/scratch/raw-integer-count-20261026/control-baseline-build2/postgres18/lib/pg_accel.dylib" "$PWD/target/release/libpg_accel.dylib" "$PWD/target/release/pg_accel_bench" > "$build/protected.sha256"
cleanup() {
 local result=$?
 trap - EXIT
 set +e
 shasum -a256 -c "$build/source.sha256" > "$build/source-after.log" || result=1
 shasum -a256 -c "$build/harness.sha256" > "$build/harness-after.log" || result=1
 shasum -a256 -c "$build/protected.sha256" > "$build/protected-after.log" || result=1
 printf 'terminal=%s\n' "$result" > "$build/terminal.log"
 exit "$result"
}
trap cleanup EXIT
ulimit -n 4096
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 40m cargo pgrx install --package pg_accel --pg-config "$PG_CONFIG" --no-default-features --features "pg$major" --release > "$build/install.log" 2>&1
cmp "$build/postgres/lib/pg_accel.dylib" "$build/target/release/libpg_accel.dylib"
shasum -a256 "$build/postgres/lib/pg_accel.dylib" "$build/target/release/libpg_accel.dylib" "$build/postgres/bin/postgres" "$PG_CONFIG" > "$build/module.sha256"
otool -L "$build/postgres/lib/pg_accel.dylib" > "$build/module-dependencies.log"
