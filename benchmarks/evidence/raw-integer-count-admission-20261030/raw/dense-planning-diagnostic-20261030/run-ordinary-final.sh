#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/dense-planning-diagnostic-20261030"
run="$task/ordinary-final1"
test ! -e "$run"
mkdir "$run"
shasum -a256 "$task/run-ordinary-final.sh" > "$run/harness.sha256"
finish() {
 local result=$?
 trap - EXIT
 set +e
 shasum -a256 -c "$task/lazy-production18/source.sha256" > "$run/source-after.log" || result=1
 shasum -a256 -c "$task/lazy-production18/module.sha256" > "$run/module-after.log" || result=1
 shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || result=1
 printf 'terminal=%s\n' "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
shasum -a256 -c "$task/lazy-production18/source.sha256" > "$run/source-before.log"
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
export PGRX_HOME="$task/pgrx" PG_CONFIG="$task/postgres18/bin/pg_config" PGRX_PG_CONFIG_PATH="$task/postgres18/bin/pg_config"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)" BINDGEN_EXTRA_CLANG_ARGS="-isysroot $(xcrun --show-sdk-path)"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features pg18 -- --test-threads=1 > "$run/tests.log" 2>&1
