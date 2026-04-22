#!/usr/bin/env bash
set -euo pipefail

LOCK_FILE="/tmp/.pgaccel_reload.lock"
CONTAINER_NAME="${PGACCEL_CONTAINER:-docker-pgaccel-test-1}"
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
    cargo pgrx package --package pg_accel --pg-config "$(pg_config)" 2>&1 | tail -3

    # Find packaged output directory
    PKG_DIR="target/release/pg_accel-pg17"

    # Copy .so and SQL extension files into container, then restart PG
    SO_PATH=$(find "$PKG_DIR" -name "pg_accel.so" 2>/dev/null | head -1)
    if [ -n "$SO_PATH" ]; then
        docker cp "$SO_PATH" "$CONTAINER_NAME:/usr/local/lib/postgresql/"
        # Also copy extension control + SQL files
        while IFS= read -r -d '' f; do
            docker cp "$f" "$CONTAINER_NAME:/usr/local/share/postgresql/extension/"
        done < <(find "$PKG_DIR" -path "*/extension/pg_accel*" -print0 2>/dev/null)
        docker exec -u postgres "$CONTAINER_NAME" pg_ctl restart -D /var/lib/postgresql/data -m fast -w
        echo "[$(date +%H:%M:%S)] Reloaded successfully"
    else
        echo "[$(date +%H:%M:%S)] WARNING: .so not found, skipping reload"
    fi

    # Release exclusive flock
    exec 9>&-
done
