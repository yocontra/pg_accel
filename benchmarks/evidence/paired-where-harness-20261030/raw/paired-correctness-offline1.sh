#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task="$PWD/.codex/scratch/hash-count-where-rust-20261030"
run="$task/paired-correctness-offline1"
test ! -e "$run"
mkdir "$run"
cp "$task/paired-validate4/source.sha256" "$run/source.sha256"
shasum -a256 "$0" "$task/check-paired.py" "$task/test-paired-checker.py" "$task/paired-sink.expected" > "$run/harness.sha256"
stage=preflight
finish() {
 result=$?
 trap - EXIT
 set +e
 shasum -a256 -c "$run/source.sha256" > "$run/source-after.log" || result=1
 shasum -a256 -c "$run/harness.sha256" > "$run/harness-after.log" || result=1
 printf 'stage=%s\nterminal=%s\n' "$stage" "$result" > "$run/terminal.log"
 exit "$result"
}
trap finish EXIT
shasum -a256 -c "$run/source.sha256" > "$run/source-before.log"
stage=checker-negative
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 60s /usr/bin/python3 -I -O "$task/test-paired-checker.py" > "$run/checker.log" 2>&1
stage=sink-units
export ACPP_PREFIX="$PWD/.pgaccel/acpp/current" ACPP_TARGETS=generic CMAKE_BUILD_PARALLEL_LEVEL=4 CARGO_NET_OFFLINE=true
base="$PWD/.codex/scratch/dense-planning-diagnostic-20261030"
export PGRX_HOME="$base/pgrx" PG_CONFIG="$base/postgres18/bin/pg_config" PGRX_PG_CONFIG_PATH="$base/postgres18/bin/pg_config"
export CFLAGS="-isysroot $(xcrun --show-sdk-path)" BINDGEN_EXTRA_CLANG_ARGS="-isysroot $(xcrun --show-sdk-path)"
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 15m cargo test --locked -p pg_accel --lib --no-default-features --features pg18,pg_test 'engine::ffi::custom_scan::paired_sink::tests::' -- --test-threads=1 > "$run/sink.log" 2>&1
sed -n 's/^test \(engine::ffi::custom_scan::paired_sink::tests::[^ ]*\) \.\.\. ok$/\1/p' "$run/sink.log" | sort > "$run/actual.expected"
sort "$task/paired-sink.expected" > "$run/wanted.expected"
cmp "$run/actual.expected" "$run/wanted.expected"
rg '^test result: ok\. 4 passed; 0 failed; 0 ignored; 0 measured;' "$run/sink.log" > "$run/inventory-check.log"
stage=complete
