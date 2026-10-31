#!/usr/bin/env bash
set -euo pipefail
repo=/Users/contra/Projects/pg_accel
source_root="$repo/.codex/scratch/native-count-foundation-20261030"
task_root="$repo/.codex/scratch/native-count-foundation-validation-20261030"
run_root="$task_root/qualification1"
old_task="$repo/.codex/scratch/raw-integer-count-20261026"
timeout=/opt/homebrew/bin/gtimeout
test ! -e "$run_root"
test -z "${PGOPTIONS:-}"
mkdir "$run_root"
active_prefix=
active_data=
active_socket=
stage=preflight
export CARGO_NET_OFFLINE=true
export CMAKE_BUILD_PARALLEL_LEVEL=4
export ACPP_PREFIX="$repo/.pgaccel/acpp/current"
export ACPP_TARGETS=generic
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
export PGRX_HOME="$repo/.codex/scratch/uniform-budget-live-20261012/pgrx"
export ACPP_APPDB_DIR="$run_root/jit-cache"
ulimit -n 4096
shasum -a256 "$task_root/qualify.sh" "$task_root/dense-smoke.sql" > "$run_root/harness.sha256"
(
 cd "$source_root"
 { printf '%s\n' Cargo.toml Cargo.lock pg_accel/Cargo.toml pg_accel/build.rs pg_accel_bench/Cargo.toml pgaccel-kernels/CMakeLists.txt .acpp-version scripts/metadata_build.py scripts/metadata_effects.py scripts/metadata_link.py scripts/cpu_cheat_abi_manifest.txt scripts/cpu_cheat_audit.py scripts/tests/test_cpu_cheat_audit.py; rg --files pg_accel/src pg_accel_bench/src pgaccel-kernels/src pgaccel-kernels/include pgaccel-kernels/test; } |
 LC_ALL=C sort -u | while IFS= read -r file; do shasum -a256 "$file"; done
) > "$run_root/source.sha256"
shasum -a256 "$repo/target/release/libpg_accel.dylib" "$repo/target/release/pg_accel_bench" \
 "$repo/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/lib/pg_accel.dylib" \
 "$repo/.codex/scratch/uniform-budget-live-20261012/postgres/19beta1/lib/pg_accel.dylib" \
 "$old_task/control-baseline-build2/postgres18/lib/pg_accel.dylib" \
 "$old_task/calibration-companion-build1/postgres18/lib/pg_accel.dylib" > "$run_root/protected-artifacts.sha256"
shasum -a256 /usr/bin/clang++ "$ACPP_PREFIX/bin/acpp" "$ACPP_PREFIX/lib/libacpp-rt.dylib" > "$run_root/toolchain.sha256"
stop_owned() {
 local result=0
 if [[ -n "$active_data" && -d "$active_data" ]]; then
  "$timeout" --signal=TERM --kill-after=5s 40s "$active_prefix/bin/pg_ctl" -D "$active_data" -m fast -w -t30 stop > "${active_data%/pgdata}/stop.log" 2>&1 ||
   "$timeout" --signal=TERM --kill-after=5s 40s "$active_prefix/bin/pg_ctl" -D "$active_data" -m immediate -w -t30 stop > "${active_data%/pgdata}/stop-immediate.log" 2>&1 || true
  "$timeout" --signal=TERM --kill-after=5s 10s "$active_prefix/bin/pg_ctl" -D "$active_data" status > "${active_data%/pgdata}/status.log" 2>&1
  [[ $? == 3 ]] || result=1
  [[ ! -e "$active_data/postmaster.pid" ]] || result=1
 fi
 if [[ -n "$active_socket" && -d "$active_socket" ]]; then rmdir "$active_socket" || result=1; fi
 if [[ $result == 0 ]]; then active_data=; active_socket=; active_prefix=; fi
 return "$result"
}
cleanup() {
 local result=$?
 trap - EXIT
 set +e
 stop_owned || result=1
 (cd "$source_root" && shasum -a256 -c "$run_root/source.sha256") > "$run_root/source-after.log" || result=1
 shasum -a256 -c "$run_root/harness.sha256" > "$run_root/harness-after.log" || result=1
 shasum -a256 -c "$run_root/toolchain.sha256" > "$run_root/toolchain-after.log" || result=1
 shasum -a256 -c "$run_root/protected-artifacts.sha256" > "$run_root/protected-artifacts-after.log" || result=1
 printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run_root/terminal.log"
 exit "$result"
}
trap cleanup EXIT
df -h "$repo" > "$run_root/disk-before.log"
{ cmake --version; /usr/bin/clang++ --version; rustc -vV; cargo --version; xcrun --show-sdk-path; "$ACPP_PREFIX/bin/acpp" --version; } > "$run_root/toolchain.log" 2>&1
stage=native_configure
"$timeout" --signal=TERM --kill-after=10s 3m cmake -S "$source_root/pgaccel-kernels" -B "$run_root/native" -DAdaptiveCpp_DIR="$ACPP_PREFIX/lib/cmake/AdaptiveCpp" -DACPP_TARGETS=generic -DCMAKE_BUILD_TYPE=Release > "$run_root/native-configure.log" 2>&1
stage=native_build
"$timeout" --signal=TERM --kill-after=30s 20m cmake --build "$run_root/native" --target test_grouped_agg_hash test_h3 --parallel 4 > "$run_root/native-build.log" 2>&1
shasum -a256 "$run_root/native/test_grouped_agg_hash" "$run_root/native/test_h3" "$run_root/native/libpgaccel_kernels_shared.dylib" > "$run_root/native.sha256"
stage=native_contract
"$timeout" --signal=TERM --kill-after=10s 3m "$run_root/native/test_grouped_agg_hash" --contract-only > "$run_root/native-contract.log" 2>&1
stage=native_hash
"$timeout" --signal=TERM --kill-after=30s 20m "$run_root/native/test_grouped_agg_hash" > "$run_root/native-hash.log" 2>&1
stage=native_h3
"$timeout" --signal=TERM --kill-after=30s 10m "$run_root/native/test_h3" > "$run_root/native-h3.log" 2>&1
shasum -a256 -c "$run_root/native.sha256" > "$run_root/native-after.log"
for major in 18 19; do
 case "$major" in 18) version=18.4;; 19) version=19beta1;; esac
 build="$run_root/pg$major"
 mkdir "$build"
 cp -R "$repo/.codex/scratch/uniform-budget-live-20261012/postgres/$version" "$build/postgres"
 export PG_CONFIG="$build/postgres/bin/pg_config"
 export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
 export CARGO_TARGET_DIR="$build/target"
 test "$("$PG_CONFIG" --pkglibdir)" = "$build/postgres/lib"
 test "$("$PG_CONFIG" --bindir)" = "$build/postgres/bin"
 test "$("$PG_CONFIG" --sharedir)" = "$build/postgres/share"
 "$PG_CONFIG" > "$build/pg-config.log"
 stage="production_build_pg$major"
 (
  cd "$source_root"
  "$timeout" --signal=TERM --kill-after=30s 40m cargo pgrx install --package pg_accel --pg-config "$PG_CONFIG" --no-default-features --features "pg$major" --release
 ) > "$build/install.log" 2>&1
 cmp "$build/postgres/lib/pg_accel.dylib" "$build/target/release/libpg_accel.dylib"
 shasum -a256 "$build/postgres/lib/pg_accel.dylib" "$build/target/release/libpg_accel.dylib" "$build/postgres/bin/postgres" "$PG_CONFIG" > "$build/module.sha256"
 otool -L "$build/postgres/lib/pg_accel.dylib" > "$build/module-dependencies.log"
 rg --files --hidden --no-ignore "$build/target" | rg '/libpgaccel_kernels\.(a|dylib)$' | LC_ALL=C sort | while IFS= read -r file; do shasum -a256 "$file"; done > "$build/native-artifacts.sha256"
 test -s "$build/native-artifacts.sha256"
 active_prefix="$build/postgres"
 active_data="$build/pgdata"
 active_socket="$repo/.codex/scratch/ncf$major-20261030"
 test ! -e "$active_socket"
 mkdir "$active_socket"
 stage="init_pg$major"
 "$timeout" --signal=TERM --kill-after=10s 2m "$active_prefix/bin/initdb" -D "$active_data" --no-locale --encoding=UTF8 --auth=trust > "$build/initdb.log" 2>&1
 "$timeout" --signal=TERM --kill-after=10s 90s "$active_prefix/bin/pg_ctl" -D "$active_data" -l "$build/server.log" -w -t60 -o "-p 341$major -k $active_socket -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$build/start.log" 2>&1
 connection="host=$active_socket port=341$major dbname=postgres connect_timeout=10"
 "$timeout" --signal=TERM --kill-after=10s 2m "$active_prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel; CREATE EXTENSION h3; SELECT extname,extversion FROM pg_extension ORDER BY extname' > "$build/extensions.log" 2>&1
 stage="dense_smoke_pg$major"
 "$timeout" --signal=TERM --kill-after=10s 10m "$active_prefix/bin/psql" "$connection" -X -v ON_ERROR_STOP=1 -f "$task_root/dense-smoke.sql" > "$build/dense-smoke.log" 2>&1
 stage="h3_regression_pg$major"
 (
  cd "$source_root"
  export PG_ACCEL_TEST_CONNECTION="$connection" PG_ACCEL_TEST_PG_MAJOR="$major" PG_ACCEL_PG_MAJOR="$major" RUST_TEST_THREADS=1
  "$timeout" --signal=TERM --kill-after=30s 20m cargo test --locked -p pg_accel_bench --features integration_tests conditional_h3_count_all_types_star_and_predicate_alias_match_native -- --nocapture --test-threads=1
 ) > "$build/h3-regression.log" 2>&1
 rg -q 'test result: ok. 1 passed; 0 failed;' "$build/h3-regression.log"
 set +e
 stop_owned
 stop_rc=$?
 set -e
 test "$stop_rc" = 0
 shasum -a256 -c "$build/module.sha256" > "$build/module-after.log"
 shasum -a256 -c "$build/native-artifacts.sha256" > "$build/native-artifacts-after.log"
 printf 'terminal=0\n' > "$build/terminal.log"
done
stage=complete
