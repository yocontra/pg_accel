#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
source_root="$task_root/calibration-diagnostic-ce24"
build_dir="$task_root/calibration-companion-build1"
original_prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4"
prefix="$build_dir/postgres18"
test ! -e "$build_dir"
test -d "$source_root/pg_accel/src"
test -z "${PGOPTIONS:-}"
mkdir "$build_dir"
shasum -a256 "$original_prefix/lib/pg_accel.dylib" "$PWD/target/release/libpg_accel.dylib" "$PWD/target/release/pg_accel_bench" "$task_root/control-baseline-build2/postgres18/lib/pg_accel.dylib" > "$build_dir/protected-artifacts.sha256"
shasum -a256 "$task_root/build-calibration-companion.sh" > "$build_dir/harness.sha256"
(
 cd "$source_root"
 { printf '%s\n' Cargo.toml Cargo.lock pg_accel/Cargo.toml pg_accel/build.rs pgaccel-kernels/CMakeLists.txt .acpp-version; rg --files pg_accel/src pgaccel-kernels/src pgaccel-kernels/include; } |
 LC_ALL=C sort -u | while IFS= read -r source_path; do shasum -a256 "$source_path"; done
) > "$build_dir/source.sha256"
cleanup() {
 task_rc=$?
 trap - EXIT
 set +e
 (cd "$source_root" && shasum -a256 -c "$build_dir/source.sha256") > "$build_dir/source-after.log" || task_rc=1
 shasum -a256 -c "$build_dir/protected-artifacts.sha256" > "$build_dir/protected-artifacts-after.log" || task_rc=1
 shasum -a256 -c "$build_dir/harness.sha256" > "$build_dir/harness-after.log" || task_rc=1
 printf 'terminal=%s\n' "$task_rc" > "$build_dir/terminal.log"
 exit "$task_rc"
}
trap cleanup EXIT
# Independent copied PostgreSQL prefix and Cargo/native target. This cannot
# overwrite the original ce24 timed baseline or current qualified installation.
cp -R "$original_prefix" "$prefix"
export PG_CONFIG="$prefix/bin/pg_config"
test "$("$PG_CONFIG" --bindir)" = "$prefix/bin"
test "$("$PG_CONFIG" --pkglibdir)" = "$prefix/lib"
test "$("$PG_CONFIG" --sharedir)" = "$prefix/share"
"$PG_CONFIG" > "$build_dir/pg-config.log"
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CARGO_TARGET_DIR="$build_dir/target"
export CARGO_NET_OFFLINE=true
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
ulimit -n 4096
cd "$source_root"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 40m cargo pgrx install --package pg_accel --pg-config "$PG_CONFIG" --no-default-features --features pg18,count_calibration_diagnostic --release > "$build_dir/install.log" 2>&1
shasum -a256 "$prefix/lib/pg_accel.dylib" "$build_dir/target/release/libpg_accel.dylib" "$prefix/bin/postgres" "$PG_CONFIG" > "$build_dir/module.sha256"
cmp "$prefix/lib/pg_accel.dylib" "$build_dir/target/release/libpg_accel.dylib"
otool -L "$prefix/lib/pg_accel.dylib" > "$build_dir/module-dependencies.log"
rg --files --hidden --no-ignore "$build_dir/target" | rg '/libpgaccel_kernels\.(a|dylib)$' |
 LC_ALL=C sort | while IFS= read -r artifact; do shasum -a256 "$artifact"; done > "$build_dir/native-artifacts.sha256"
test -s "$build_dir/native-artifacts.sha256"
