#!/usr/bin/env bash
set -euo pipefail
source /Users/contra/Projects/pg_accel/.codex/scratch/hash-count-where-rust-20261030/paired-exit-status.sh
test "$(paired_exit_status 124 1)" = 124
test "$(paired_exit_status 3 1)" = 3
test "$(paired_exit_status 0 1)" = 1
test "$(paired_exit_status 0 0)" = 0
printf '4 status-precedence cases passed\n'
