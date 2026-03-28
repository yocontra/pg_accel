#!/usr/bin/env bash
set -euo pipefail

AGENT_ID="${1:?Usage: run_agent_tests.sh <agent_id> [test_glob]}"
TEST_GLOB="${2:-*.sql}"
DB_NAME="pgaccel_a${AGENT_ID}"

export DB_NAME
exec "$(dirname "$0")/run_integration_tests.sh"
