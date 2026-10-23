#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/hosted-budget-20261018"
major=$1
stage=$2
filter=${3-}
shasum -a 256 -c "$task_root/task1-plus-join-retry.sha256" > "$task_root/pg$major-$stage-all-source-before.log"
shasum -a 256 "$task_root/run-final-live-task1.sh" "$task_root/run-live-task1.sh" > "$task_root/pg$major-$stage-harness.sha256"
set +e
bash "$task_root/run-live-task1.sh" "$major" "$stage" "$filter"
result=$?
set -e
shasum -a 256 -c "$task_root/task1-plus-join-retry.sha256" > "$task_root/pg$major-$stage-all-source-after.log"
printf 'PG%s final nine-file gate %s terminal=%s\n' "$major" "$stage" "$result"
exit "$result"
