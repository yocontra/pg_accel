#!/usr/bin/env bash
set -euo pipefail

DB_TEMPLATE="${DB_TEMPLATE:-postgres}"
DB_PREFIX="${DB_PREFIX:-pgaccel_a}"
DB_COUNT="${DB_COUNT:-10}"

for i in $(seq 0 $((DB_COUNT - 1))); do
    db="${DB_PREFIX}${i}"
    psql -v ON_ERROR_STOP=1 <<-EOSQL
        SELECT 'Creating ${db}...' AS status;
        CREATE DATABASE ${db} TEMPLATE ${DB_TEMPLATE};
EOSQL
done
echo "Created $DB_COUNT agent databases from $DB_TEMPLATE template"
