#!/bin/bash
set -e
for i in $(seq 0 9); do
    psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" <<-EOSQL
        SELECT 'Creating pgaccel_a${i}...' AS status;
        CREATE DATABASE pgaccel_a${i} TEMPLATE pgaccel_shared;
EOSQL
done
echo "Created 10 agent databases from pgaccel_shared template"
