#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/hash-count-where-rust-20261030"
run="$task/qualification4"
test ! -e "$run"
mkdir "$run"
stage=preflight
finish() {
 result=$?
 trap - EXIT
 set +e
 shasum -a256 -c "$task/h3-validate1/source.sha256" > "$run/source-after.log" || result=1
 shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || result=1
 printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
shasum -a256 "$0" "$task/run-live4.sh" "$task/check-live2.py" "$task/raw.expected" "$task/h3-22.expected" "$task/planner.expected" "$task/pgrx/config.toml" "$task/h3-compile1/terminal.log" "$task/h3-compile1/source.sha256" "$task/h3-validate1/terminal.log" "$task/h3-validate1/source.sha256" > "$run/harness.sha256"
rg -qx 'terminal=0' "$task/h3-compile1/terminal.log"
rg -qx 'terminal=0' "$task/h3-validate1/terminal.log"
shasum -a256 -c "$task/h3-compile1/source.sha256" > "$run/compiled-source-before.log"
shasum -a256 -c "$task/h3-validate1/source.sha256" > "$run/source-before.log"
for major in 18 19; do
 for family in raw h3 planner; do
  stage="pg$major-$family"
  shasum -a256 -c "$run/harness.sha256" > "$run/$stage-harness-before.log"
  case "$family" in
   raw) filter=raw_count_tests;;
   h3) filter=h3_count_tests;;
   planner) filter=generic_groupagg::tests::pg_pg_raw_count_;;
  esac
  bash "$task/run-live4.sh" "$major" "$family-qualification4" "$filter"
 done
done
stage=complete
