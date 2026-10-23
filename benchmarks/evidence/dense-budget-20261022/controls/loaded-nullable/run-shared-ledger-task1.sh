#!/usr/bin/env bash
set -euo pipefail
cd /Users/contra/Projects/pg_accel
task_root="$PWD/.codex/scratch/hosted-budget-20261018"
psql_bin=$1
connection=$2
run_dir=$3
mkdir -p "$run_dir"
test ! -e "$run_dir/session-a.log"
"$psql_bin" "$connection" -X -v ON_ERROR_STOP=1 -c "DO \$\$ BEGIN IF EXISTS(SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='tests') OR (SELECT gpu_available FROM pg_accel_device_info()) IS DISTINCT FROM true THEN RAISE EXCEPTION 'requires production module with actual GPU'; END IF; END \$\$;" > "$run_dir/identity.log" 2>&1
baseline=$("$psql_bin" "$connection" -XAt -v ON_ERROR_STOP=1 -c 'SELECT pg_accel_resident_live_bytes()')
test "$baseline" -eq 0
mkfifo "$run_dir/a.fifo" "$run_dir/b.fifo"
exec 3<>"$run_dir/a.fifo"
exec 4<>"$run_dir/b.fifo"
"$psql_bin" "$connection" -X -v ON_ERROR_STOP=1 <"$run_dir/a.fifo" >"$run_dir/session-a.log" 2>&1 &
client_a=$!
"$psql_bin" "$connection" -X -v ON_ERROR_STOP=1 <"$run_dir/b.fifo" >"$run_dir/session-b.log" 2>&1 &
client_b=$!
wait_client() {
  local variable_name=$1 client=${!1} deadline=$((SECONDS+15)) result
  if [ -z "$client" ]; then return 0; fi
  while kill -0 "$client" 2>/dev/null; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      printf 'Owned client %s did not exit; terminating before cluster cleanup\n' "$client" >&2
      kill "$client" 2>/dev/null || true
      local grace=$((SECONDS+3))
      while kill -0 "$client" 2>/dev/null && [ "$SECONDS" -lt "$grace" ]; do sleep 0.1; done
      if kill -0 "$client" 2>/dev/null; then kill -KILL "$client" 2>/dev/null || true; fi
      wait "$client" 2>/dev/null || true
      printf -v "$variable_name" '%s' ''
      return 1
    fi
    sleep 0.1
  done
  if wait "$client"; then result=0; else result=$?; fi
  printf -v "$variable_name" '%s' ''
  return "$result"
}
cleanup() {
  local original_status=$?
  set +e
  printf '\\q\n' >&3 || true
  printf '\\q\n' >&4 || true
  exec 3>&-
  exec 4>&-
  wait_client client_a || true
  wait_client client_b || true
  exit "$original_status"
}
trap cleanup EXIT
wait_marker() {
  local file=$1 marker=$2 deadline=$((SECONDS+90))
  while ! rg -q "^$marker( |$)" "$file"; do
    if rg -q 'ERROR:|FATAL:' "$file" || [ "$SECONDS" -ge "$deadline" ]; then
      printf 'Missing marker %s in %s\n' "$marker" "$file" >&2
      return 1
    fi
    sleep 0.1
  done
}
wait_live() {
  local expected=$1 deadline=$((SECONDS+30)) actual
  while true; do
    actual=$("$psql_bin" "$connection" -XAt -v ON_ERROR_STOP=1 -c 'SELECT pg_accel_resident_live_bytes()')
    if [ "$actual" -eq "$expected" ]; then return; fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      printf 'Ledger release timeout expected=%s actual=%s\n' "$expected" "$actual" >&2
      return 1
    fi
    sleep 0.1
  done
}
printf '\\i %s/ledger-b-setup.sql\n' "$task_root" >&4
wait_marker "$run_dir/session-b.log" B_READY
read -r _ b_live b_idle b_workspace_hits b_output_hits < <(rg '^B_READY ' "$run_dir/session-b.log")
mib=1048576
local_mib=$(((b_live+mib-1)/mib))
local_budget=$((local_mib*mib))
test "$b_idle" -gt 0
test "$b_live" -le "$local_budget"
test "$local_budget" -lt "$((b_live+b_idle))"
printf 'SET pg_accel.resident_memory_budget_mb=%s; SELECT pg_temp.check_b(true,%s);\n' "$local_mib" "$b_live" >&4
printf "SELECT (SELECT hits FROM pg_accel_grouped_workspace_pool_stats())>%s AND (SELECT hits FROM pg_accel_grouped_output_pool_stats())>%s AS pool_reused \\gset\n\\if :pool_reused\n\\echo B_LOCAL_IDLE_OK\n\\else\n\\quit 3\n\\endif\n" "$b_workspace_hits" "$b_output_hits" >&4
wait_marker "$run_dir/session-b.log" B_LOCAL_IDLE_OK
printf '\\i %s/ledger-a-setup.sql\n' "$task_root" >&3
wait_marker "$run_dir/session-a.log" A_READY
read -r _ combined a_resident a_idle a_generation a_before a_after a_pid < <(rg '^A_READY ' "$run_dir/session-a.log")
a_owned=$((combined-b_live))
a_active=$((a_owned-a_resident-a_idle))
test "$a_after" -gt "$a_before"
test "$a_owned" -gt "$mib"
test "$a_active" -gt 0
foreign_mib=$(((combined-1)/mib))
test "$((foreign_mib*mib))" -ge "$b_live"
test "$((foreign_mib*mib))" -lt "$combined"
printf 'SET pg_accel.resident_memory_budget_mb=%s; SELECT pg_temp.check_b(false,%s);\n\\echo B_FOREIGN_ACTIVE_OK\n' "$foreign_mib" "$combined" >&4
wait_marker "$run_dir/session-b.log" B_FOREIGN_ACTIVE_OK
printf "SELECT pg_backend_pid()=%s AND pg_accel_resident_live_bytes()=%s AND (SELECT raw_bytes+derived_bytes=%s AND generation=%s AND pinned FROM pg_accel_resident_status() WHERE relid='budget_a'::regclass) AS protected \\gset\n\\if :protected\n\\echo A_ACTIVE_PROTECTED\n\\else\n\\quit 3\n\\endif\nCLOSE budget_owner; COMMIT;\n" "$a_pid" "$combined" "$a_resident" "$a_generation" >&3
printf "SELECT pg_accel_resident_live_bytes() AS closed_live,(SELECT retained_bytes FROM pg_accel_grouped_workspace_pool_stats())+(SELECT retained_bytes FROM pg_accel_grouped_output_pool_stats()) AS closed_idle,(SELECT raw_bytes+derived_bytes FROM pg_accel_resident_status() WHERE relid='budget_a'::regclass) AS closed_resident \\gset\n\\echo A_CLOSED :closed_live :closed_idle :closed_resident\n" >&3
wait_marker "$run_dir/session-a.log" A_CLOSED
read -r _ closed_live closed_idle closed_resident < <(rg '^A_CLOSED ' "$run_dir/session-a.log")
test "$closed_live" -eq "$combined"
test "$closed_resident" -eq "$a_resident"
test "$closed_idle" -eq "$((a_idle+a_active))"
printf 'SELECT pg_temp.check_b(false,%s);\n\\echo B_FOREIGN_IDLE_OK\n' "$combined" >&4
wait_marker "$run_dir/session-b.log" B_FOREIGN_IDLE_OK
printf '\\q\n' >&3
wait_client client_a
wait_live "$b_live"
printf 'SELECT pg_temp.check_b(true,%s);\n\\echo B_AFTER_FOREIGN_EXIT_OK\n' "$b_live" >&4
wait_marker "$run_dir/session-b.log" B_AFTER_FOREIGN_EXIT_OK
printf '\\q\n' >&4
wait_client client_b
wait_live "$baseline"
final=$("$psql_bin" "$connection" -XAt -v ON_ERROR_STOP=1 -c 'SELECT pg_accel_resident_live_bytes()')
test "$final" -eq "$baseline"
printf 'SHARED_LEDGER_OK b_live=%s b_idle=%s local_budget=%s foreign_owned=%s foreign_active=%s foreign_budget=%s final=%s\n' "$b_live" "$b_idle" "$local_budget" "$a_owned" "$a_active" "$((foreign_mib*mib))" "$final"
