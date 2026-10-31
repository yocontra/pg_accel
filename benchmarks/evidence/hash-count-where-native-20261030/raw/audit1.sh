#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
run=.codex/scratch/hash-count-where-native-20261030/audit1
test ! -e "$run"
mkdir "$run"
shasum -a256 pgaccel-kernels/include/pgaccel_olap.h pgaccel-kernels/src/grouped_agg.cpp pgaccel-kernels/test/test_grouped_agg_hash.cpp scripts/cpu_cheat_abi_manifest.txt scripts/cpu_cheat_audit.py scripts/tests/test_cpu_cheat_audit.py > "$run/source.sha256"
finish() {
  result=$?
  trap - EXIT
  set +e
  shasum -a256 -c "$run/source.sha256" > "$run/source-after.log" || result=1
  printf 'terminal=%s\n' "$result" > "$run/terminal.log"
  exit "$result"
}
trap finish EXIT
export PYTHONDONTWRITEBYTECODE=1
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 15m python3 -m unittest -v \
 scripts.tests.test_cpu_cheat_audit.AbiInventoryTests.test_checked_in_manifest_has_literal_integrity_anchor \
 scripts.tests.test_cpu_cheat_audit.ProductionWitnessTests.test_complete_real_abi_baseline_and_violation_floor \
 scripts.tests.test_cpu_cheat_audit.ProductionWitnessTests.test_partition_v2_inventory_does_not_grant_source_proof \
 scripts.tests.test_cpu_cheat_audit.ProductionWitnessTests.test_count_work_inventory_does_not_grant_metadata_proof \
 scripts.tests.test_cpu_cheat_audit.PartitionForwardingObligationTests > "$run/tests.log" 2>&1
