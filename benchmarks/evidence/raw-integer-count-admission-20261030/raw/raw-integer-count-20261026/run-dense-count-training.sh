#!/usr/bin/env bash
# Review-gated training only. No fit and no held-out execution.
set -euo pipefail
cd /Users/contra/Projects/pg_accel
repo="$PWD"
task_root="$repo/.codex/scratch/raw-integer-count-20261026"
run_dir="$task_root/dense-count-training3"
test -z "${PGOPTIONS:-}"
test -f "$run_dir/manifest.json"
test ! -e "$run_dir/servers"
builds=("$task_root/control-baseline-build2" "$task_root/calibration-companion-build1" "$task_root/build18-workfacts")
sources=("$task_root/baseline-ce24" "$task_root/calibration-diagnostic-ce24" "$repo")
prefixes=("${builds[0]}/postgres18" "${builds[1]}/postgres18" "$repo/.codex/scratch/uniform-budget-live-20261012/postgres/18.4")
roles=(timed companion current)
sockets=("$repo/.codex/scratch/counttrainold" "$repo/.codex/scratch/counttraindiag" "$repo/.codex/scratch/counttrainnew")
ports=(33918 33919 33920)
for i in 0 1 2; do
 test ! -e "${sockets[$i]}"
 terminal=terminal.log
 if test "$i" = 2; then terminal=final-terminal.log; fi
 test "$(<"${builds[$i]}/$terminal")" = terminal=0
 (cd "${sources[$i]}" && shasum -a256 -c "${builds[$i]}/source.sha256")
 shasum -a256 -c "${builds[$i]}/module.sha256"
 if test "$i" != 2; then shasum -a256 -c "${builds[$i]}/native-artifacts.sha256"; fi
done
mkdir "$run_dir/servers"
shasum -a256 "$task_root/run-dense-count-training.sh" "$task_root/acquire-dense-count-training.py" "$task_root/qualify-companion.py" "$task_root/dense-count-calibration.py" "$task_root/dense-count-calibration-spec.md" "$run_dir/manifest.json" > "$run_dir/runtime-harness.sha256"
ulimit -n 4096
created=0
cleanup() {
 task_rc=$?
 trap - EXIT
 set +e
 for ((i=created-1;i>=0;i--)); do
  server="$run_dir/servers/${roles[$i]}"
  pgdata="$server/pgdata"
  if test -f "$pgdata/PG_VERSION"; then
   if test -e "$pgdata/postmaster.pid"; then
    /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 40s "${prefixes[$i]}/bin/pg_ctl" -D "$pgdata" -m fast -w -t30 stop > "$server/stop.log" 2>&1
    if test "$?" != 0; then
     /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 40s "${prefixes[$i]}/bin/pg_ctl" -D "$pgdata" -m immediate -w -t30 stop >> "$server/stop.log" 2>&1 || task_rc=1
    fi
   fi
   "${prefixes[$i]}/bin/pg_ctl" -D "$pgdata" status > "$server/status.log" 2>&1
   test "$?" = 3 || task_rc=1
   test ! -e "$pgdata/postmaster.pid" || task_rc=1
  fi
  rmdir "${sockets[$i]}" || task_rc=1
 done
 for i in 0 1 2; do
  (cd "${sources[$i]}" && shasum -a256 -c "${builds[$i]}/source.sha256") > "$run_dir/${roles[$i]}-source-after.log" || task_rc=1
  shasum -a256 -c "${builds[$i]}/module.sha256" > "$run_dir/${roles[$i]}-module-after.log" || task_rc=1
  if test "$i" != 2; then shasum -a256 -c "${builds[$i]}/native-artifacts.sha256" > "$run_dir/${roles[$i]}-archive-after.log" || task_rc=1; fi
 done
 shasum -a256 -c "$run_dir/runtime-harness.sha256" > "$run_dir/harness-after.log" || task_rc=1
 vm_stat > "$run_dir/vm-after.log"
 memory_pressure -Q > "$run_dir/memory-after.log"
 df -h "$run_dir" > "$run_dir/disk-after.log"
 ps -axo pid,ppid,%cpu,%mem,command > "$run_dir/processes-after.log"
 printf 'terminal=%s\n' "$task_rc" > "$run_dir/terminal.log"
 if test "$task_rc" = 0; then
  /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=5s 1m python3 "$task_root/acquire-dense-count-training.py" complete --run-dir "$run_dir" > "$run_dir/completion.log" 2>&1 || task_rc=1
  if test "$task_rc" != 0; then printf 'terminal=1\n' > "$run_dir/terminal.log"; fi
 fi
 exit "$task_rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
vm_stat > "$run_dir/vm-before.log"
memory_pressure -Q > "$run_dir/memory-before.log"
df -h "$run_dir" > "$run_dir/disk-before.log"
ps -axo pid,ppid,%cpu,%mem,command > "$run_dir/processes-before.log"
for i in 0 1 2; do
 server="$run_dir/servers/${roles[$i]}"
 mkdir "$server" "${sockets[$i]}"
 created=$((i+1))
 export ACPP_APPDB_DIR="$server/jit-cache"
 /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "${prefixes[$i]}/bin/initdb" -D "$server/pgdata" --no-locale --encoding=UTF8 --auth=trust > "$server/initdb.log" 2>&1
 /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 90s "${prefixes[$i]}/bin/pg_ctl" -D "$server/pgdata" -l "$server/server.log" -w -t60 -o "-p ${ports[$i]} -k ${sockets[$i]} -c listen_addresses='' -c shared_preload_libraries=pg_accel -c shared_buffers=16GB -c max_worker_processes=16" start > "$server/start.log" 2>&1
 /opt/homebrew/bin/gtimeout --signal=TERM --kill-after=10s 2m "${prefixes[$i]}/bin/psql" "host=${sockets[$i]} port=${ports[$i]} dbname=postgres connect_timeout=10" -X -v ON_ERROR_STOP=1 -c 'CREATE EXTENSION pg_accel' > "$server/extensions.log" 2>&1
done
# One finite acquisition, with every warmup and measured interval retained.
/opt/homebrew/bin/gtimeout --signal=TERM --kill-after=30s 30m python3 "$task_root/acquire-dense-count-training.py" acquire --run-dir "$run_dir" > "$run_dir/acquisition.log" 2>&1
rg -q '^TRAINING_12_CELLS_COMPLETE_NO_FIT_NO_HOLDOUTS$' "$run_dir/acquisition.log"
