#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/hash-count-where-rust-20261030"
major=${1:?major}
stage=${2:?fresh stage}
filter=${3:?test filter}
case "$filter" in
 paired_cleanup_tests) expected="$task/paired-cleanup.expected";;
 *) exit 2;;
esac
case "$major" in 18) version=18.4;;19) version=19beta1;;*) exit 2;;esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;;esac
run="$task/pgtests$major-$stage"
prefix="$task/postgres$major"
timeout=/opt/homebrew/bin/gtimeout
test ! -e "$run"
test -z "${PGOPTIONS:-}"
mkdir "$run"
if test ! -e "$prefix"; then cp -R "$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version" "$prefix"; fi
test "$("$prefix/bin/pg_config" --pkglibdir)" = "$prefix/lib"
files=(pg_accel/src/engine/ffi/custom_scan/{mod,paired_driver,paired_backend,paired_runner,paired_population,paired_fixtures,paired_sink,paired_cleanup_tests}.rs pg_accel/src/engine/ffi/planner_hooks/{mod,generic_groupagg}.rs pg_accel/src/engine/ffi/planner_hooks/generic_groupagg/paired_recipes.rs pg_accel/src/engine/residency/{mod,store}.rs pg_accel/src/engine/residency/store/paired.rs pg_accel/src/engine/executor/agg/{mod,descriptor,execute}.rs pg_accel/src/engine/stats.rs pg_accel/src/gpu/grouped_agg.rs)
files+=(pgaccel-kernels/src/grouped_agg.cpp pgaccel-kernels/include/pgaccel_olap.h)
shasum -a256 "${files[@]}" > "$run/source.sha256"
shasum -a256 "$0" "$task/pgrx/config.toml" "$task/check-live2.py" "$expected" > "$run/harness.sha256"
shasum -a256 "$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/18.4/lib/pg_accel.dylib" "$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/19beta1/lib/pg_accel.dylib" "$PWD/.codex/scratch/raw-integer-count-20261026/control-baseline-build2/postgres18/lib/pg_accel.dylib" "$PWD/.codex/scratch/dense-planning-diagnostic-20261030/lazy-production18/postgres/lib/pg_accel.dylib" "$PWD/target/release/libpg_accel.dylib" > "$run/protected.sha256"
cleanup() {
 local result=$?
 trap - EXIT
 set +e
 for child in "$run/pgdata/""$major"-*; do
  test -d "$child" || continue
  test "$(cat "$child/PG_VERSION")" = "$major" || { result=1; continue; }
  "$timeout" --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$child" -m fast -w -t30 stop >> "$run/stop.log" 2>&1 ||
   "$timeout" --signal=TERM --kill-after=5s 40s "$prefix/bin/pg_ctl" -D "$child" -m immediate -w -t30 stop >> "$run/stop.log" 2>&1 || true
  "$timeout" --signal=TERM --kill-after=5s 10s "$prefix/bin/pg_ctl" -D "$child" status >> "$run/status.log" 2>&1
  test "$?" = 3 || result=1
  test ! -e "$child/postmaster.pid" || result=1
 done
 shasum -a256 -c "$run/source.sha256" > "$run/source-after.log" || result=1
 shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || result=1
 shasum -a256 -c "$run/protected.sha256" > "$run/protected-after.log" || result=1
 shasum -a256 "$prefix/lib/pg_accel.dylib" > "$run/pg-test-module.sha256" || result=1
 otool -L "$prefix/lib/pg_accel.dylib" > "$run/module-links.log" || result=1
 printf 'filter=%s\nterminal=%s\n' "$filter" "$result" > "$run/terminal.log"
 exit "$result"
}
trap cleanup EXIT
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
export PGRX_HOME="$task/pgrx" PG_CONFIG="$prefix/bin/pg_config" PGRX_PG_CONFIG_PATH="$prefix/bin/pg_config"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)" BINDGEN_EXTRA_CLANG_ARGS="-isysroot $(xcrun --show-sdk-path)"
export ACPP_APPDB_DIR="$task/pgtest-jit-cache" RUST_TEST_THREADS=1
export PG_ACCEL_CANDIDATE_OBSERVATION_PATH="$run/candidate-observations.txt"
ulimit -n 4096
"$timeout" --signal=TERM --kill-after=30s 30m cargo pgrx test "pg$major" "$filter" --no-default-features --manifest-path pg_accel/Cargo.toml --pgdata "$run/pgdata" > "$run/live.log" 2>&1

/usr/bin/python3 -I "$task/check-live2.py" "$run/live.log" "$expected" > "$run/test-inventory-check.log"
