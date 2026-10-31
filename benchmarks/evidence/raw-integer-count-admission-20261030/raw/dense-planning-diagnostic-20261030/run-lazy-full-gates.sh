#!/usr/bin/env bash
set -euo pipefail
repo=/Users/contra/Projects/pg_accel
task="$repo/.codex/scratch/dense-planning-diagnostic-20261030"
run="$task/full-gates1"
test ! -e "$run"
mkdir "$run"
stage=preflight
shasum -a256 "$task/run-lazy-full-gates.sh" "$task/run-lazy-benchmark.sh" "$task/run-lazy-control-comparison.sh" "$task/verify-retained-bench.sh" "$task/check-lazy-matrix.py" "$task/summarize-control-comparison.py" > "$run/harness.sha256"
finish() {
 local result=$?
 trap - EXIT
 set +e
 shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || result=1
 printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
test "$(cat "$task/run5/terminal.log")" = $'stage=complete\nterminal=0'
df -h "$repo" > "$run/disk-before.log"
runs=()
for lane in raw_int2_count_star raw_int2_count_bool_filter raw_int4_count_star raw_int4_count_bool_filter raw_int8_count_star raw_int8_count_bool_filter dense_int2_count_control dense_int4_count_control dense_int8_count_control h3_parent_count_int4_res0 h3_parent_count_bool_res0; do
 stage="matrix_$lane"
 bash "$task/run-lazy-benchmark.sh" "$lane" "$lane" > "$run/$stage.log" 2>&1
 runs+=("$task/performance18-$lane")
done
for block in 1 2 3 4; do
 for lane in dense_int2_count_control dense_int4_count_control dense_int8_count_control; do
  stage="abba_${block}_$lane"
  bash "$task/run-lazy-control-comparison.sh" "$block" "$lane" > "$run/$stage.log" 2>&1
 done
done
stage=check
set +e
python3 "$task/check-lazy-matrix.py" --repo "$repo" --build-dir "$task/lazy-production18" "${runs[@]}" > "$run/matrix.json"
matrix_rc=$?
set -e
python3 "$task/summarize-control-comparison.py" > "$run/abba.json"
printf 'matrix_exit=%s\n' "$matrix_rc" > "$run/checks.log"
stage=complete
test "$matrix_rc" = 0
python3 -c 'import json,sys; sys.exit(not json.load(open(sys.argv[1]))["all_six_no_median_slowdown"])' "$run/abba.json"
