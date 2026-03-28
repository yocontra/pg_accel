#!/usr/bin/env bash
set -euo pipefail

LOCK_FILE="/tmp/.pgaccel_reload.lock"
CONTAINER_NAME="pg_accel-postgres-1"
DEBOUNCE=5
WATCHED_DIR="pg_accel/src"

echo "pg_accel dev watcher: monitoring $WATCHED_DIR (debounce ${DEBOUNCE}s)"

fswatch -r -l "$DEBOUNCE" "$WATCHED_DIR" | while read -r _; do
    echo "[$(date +%H:%M:%S)] Change detected, rebuilding..."

    # Acquire exclusive flock (waits for running tests)
    exec 9>"$LOCK_FILE"
    flock -x 9
    echo "[$(date +%H:%M:%S)] Lock acquired, building..."

    # Rebuild
    cargo pgrx package --pg-config "$(pg_config)" 2>&1 | tail -3

    # Copy .so into container and restart PG
    SO_PATH=$(find target/release -name "pg_accel.so" -o -name "pg_accel.dylib" | head -1)
    if [ -n "$SO_PATH" ]; then
        docker cp "$SO_PATH" "$CONTAINER_NAME:/usr/lib/postgresql/17/lib/"
        docker exec "$CONTAINER_NAME" pg_ctl restart -D /var/lib/postgresql/data -m fast -w
        echo "[$(date +%H:%M:%S)] Reloaded successfully"
    else
        echo "[$(date +%H:%M:%S)] WARNING: .so not found, skipping reload"
    fi

    # Release exclusive flock
    exec 9>&-
done
