#!/usr/bin/env bash
set -euo pipefail

DB_HOST="${DB_HOST:-localhost}"
DB_PORT="${DB_PORT:-5488}"
DB_USER="${DB_USER:-postgres}"

for i in $(seq 0 9); do
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -c \
        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = 'pgaccel_a${i}';" 2>/dev/null || true
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -c \
        "DROP DATABASE IF EXISTS pgaccel_a${i};" 2>/dev/null || true
    psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -c \
        "CREATE DATABASE pgaccel_a${i} TEMPLATE pgaccel_shared;"
    echo "Created pgaccel_a${i}"
done
echo "All 10 agent databases ready"
