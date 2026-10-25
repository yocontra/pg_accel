#!/usr/bin/env bash
# Verify preload-only, CREATE/DROP EXTENSION and rollback in one backend.
# Own a unique empty database; never drop extension objects from the caller DB.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$repo_root/scripts/pg_versions.sh"
pg="${PG_ACCEL_PG_MAJOR:-$(pg_accel_default_pg_major)}"
pg_accel_require_supported_pg "$pg"
pg_config="${PG_CONFIG:-$(pg_accel_pg_config_for_pg "$pg")}"
psql_bin="${PSQL:-$("$pg_config" --bindir)/psql}"
connection="${1:-host=localhost port=$(pg_accel_pgrx_port_for_pg "$pg") dbname=postgres}"
test_db="pg_accel_catalog_$$_$(date +%s)"
created=0

cleanup() {
    local test_exit=$?
    if [ "$created" = 1 ]; then
        if ! "$psql_bin" "$connection" -X -v ON_ERROR_STOP=1 -c "DROP DATABASE \"$test_db\""; then
            echo "failed to remove owned test database: $test_db" >&2
            test_exit=1
        fi
    fi
    exit "$test_exit"
}
trap cleanup EXIT

"$psql_bin" "$connection" -X -v ON_ERROR_STOP=1 -c "CREATE DATABASE \"$test_db\" TEMPLATE template0"
created=1
{
    printf '\\connect %s\n' "$test_db"
    printf '\\set expected_database %s\n' "$test_db"
    printf '\\include %s/sql/extension_catalog_contract.sql\n' "$repo_root"
} | "$psql_bin" "$connection" -X -v ON_ERROR_STOP=1
