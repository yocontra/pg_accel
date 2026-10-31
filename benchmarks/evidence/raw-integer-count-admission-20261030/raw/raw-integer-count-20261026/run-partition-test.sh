#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/raw-integer-count-20261026"
major=${1:?major required}
stage=${2:?fresh stage required}
test_kind=${3:-partition}
case "$test_kind" in
 partition) test_name=pg_raw_count_forced_partition_signed_alias_and_predicate_identity;;
 capacity) test_name=pg_raw_count_actual_device_cap_plus_one;;
 currentness) test_name=pg_raw_count_retained_cold_mutation_rollback_and_empty_reload;;
 domain) test_name=pg_raw_count_smallint_complete_domain_and_null;;
 cleanup) test_name=pg_raw_count_spool_failure_cancel_and_actual_rescan_cleanup;;
 mask) test_name=pg_raw_count_mask_interrupt_preserves_sqlstate_and_benign_counts;;
 observer) test_name=pg_raw_count_dense_controls_preserve_independent_candidates;;
 sparse-observer) test_name=pg_raw_count_sparse_star_candidate_cost_observations;;
 decline-cache) test_name=pg_test_groupagg_decline_cache_revalidates_mutation_policy_and_prepared_plans;;
 *) exit 2;;
esac
case "$major" in 18) version=18.4;; 19) version=19beta1;; *) exit 2;; esac
case "$stage" in ''|*[!a-zA-Z0-9_-]*) exit 2;; esac
prefix="$PWD/.codex/scratch/uniform-budget-live-20261012/postgres/$version"
run_dir="$task_root/partition$major-$stage"
test ! -e "$run_dir"
test -z "${PGOPTIONS:-}"
mkdir "$run_dir"
{
 printf '%s\n' Cargo.toml Cargo.lock pg_accel/Cargo.toml pg_accel/build.rs pgaccel-kernels/CMakeLists.txt .acpp-version
 rg --files pg_accel/src pgaccel-kernels/src pgaccel-kernels/include
} | LC_ALL=C sort -u | while IFS= read -r path; do shasum -a256 "$path"; done > "$run_dir/source.sha256"
shasum -a256 "$task_root/run-partition-test.sh" > "$run_dir/harness.sha256"
cleanup() {
 task_rc=$?
 trap - EXIT
 set +e
 for child in "$run_dir/pgdata"/"$major"-[0-9]*; do
  test -d "$child" || continue
  test "$(<"$child/PG_VERSION")" = "$major" || { task_rc=1; continue; }
  "$prefix/bin/pg_ctl" -D "$child" status >> "$run_dir/stop.log" 2>&1
  if test "$?" = 0; then
   "$prefix/bin/pg_ctl" -D "$child" -m fast -w -t30 stop >> "$run_dir/stop.log" 2>&1 || task_rc=1
  fi
  "$prefix/bin/pg_ctl" -D "$child" status >> "$run_dir/status.log" 2>&1
  test "$?" = 3 || task_rc=1
  test ! -e "$child/postmaster.pid" || task_rc=1
 done
 shasum -a256 -c "$run_dir/source.sha256" > "$run_dir/source-after.log" || task_rc=1
 shasum -a256 -c "$run_dir/harness.sha256" > "$run_dir/harness-after.log" || task_rc=1
 shasum -a256 "$prefix/lib/pg_accel.dylib" > "$run_dir/pg-test-module-after.sha256"
 printf 'terminal=%s\n' "$task_rc" > "$run_dir/terminal.log"
 exit "$task_rc"
}
trap cleanup EXIT
ulimit -n 4096
export PGRX_HOME="$PWD/.codex/scratch/uniform-budget-live-20261012/pgrx"
export PG_CONFIG="$prefix/bin/pg_config"
export PGRX_PG_CONFIG_PATH="$PG_CONFIG"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)"
export BINDGEN_EXTRA_CLANG_ARGS="$CFLAGS"
export RUST_TEST_THREADS=1
export ACPP_APPDB_DIR="$task_root/cache-pg-test$major"
if test "$test_kind" = observer || test "$test_kind" = sparse-observer; then
 export PG_ACCEL_CANDIDATE_OBSERVATION_PATH="$run_dir/candidate-observations.txt"
fi
# Deliberately pg_test qualification, not a production-module receipt.
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 20m cargo pgrx test "pg$major" "$test_name" --no-default-features --manifest-path pg_accel/Cargo.toml --pgdata "$run_dir/pgdata" > "$run_dir/live.log" 2>&1
rg -q 'test result: ok. 1 passed; 0 failed;' "$run_dir/live.log"
if test "$test_kind" = observer; then
 test "$(wc -l < "$run_dir/candidate-observations.txt" | tr -d ' ')" = 18
elif test "$test_kind" = sparse-observer; then
 test "$(wc -l < "$run_dir/candidate-observations.txt" | tr -d ' ')" = 6
fi
