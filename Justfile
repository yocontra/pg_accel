# Default: list commands
default:
    @just --list

# === Development ===

# Format code
fmt:
    cargo fmt

# Check formatting
fmt-check:
    cargo fmt -- --check

# Run clippy lints
lint:
    cargo clippy -- -D warnings

# Type check all features
check:
    cargo check --all-features

# Run cargo-deny checks (licenses + advisories)
deny:
    cargo deny check

# Run pgrx tests against PG 17
test pg="17":
    cargo pgrx test pg{{pg}}

# === Docker Dev Environment ===

# Start dev PG (PostGIS + h3 + pg_accel, 10 agent DBs)
dev-up:
    docker compose -f docker/docker-compose.test.yml up -d
    @echo "Waiting for PG to be ready..."
    @until docker compose -f docker/docker-compose.test.yml exec -T pgaccel-test pg_isready -U postgres > /dev/null 2>&1; do sleep 1; done
    @echo "PG ready. Initializing agent databases..."
    docker compose -f docker/docker-compose.test.yml exec -T pgaccel-test bash /docker-entrypoint-initdb.d/02-create-agent-dbs.sh || true
    @echo "Dev environment ready on port 5488"

# Stop dev PG and remove volumes
dev-down:
    docker compose -f docker/docker-compose.test.yml down -v

# Watch for source changes and hot-reload (run in separate terminal)
dev-watch:
    docker/scripts/dev_reload.sh

# Run integration tests for a specific agent
dev-test agent="0":
    docker/scripts/run_agent_tests.sh {{agent}}

# Run all integration tests (all agents)
dev-test-all:
    docker/scripts/run_integration_tests.sh

# Connect to an agent's database via psql
dev-psql agent="0":
    psql -h localhost -p 5488 -U postgres pgaccel_a{{agent}}

# Reset an agent's database to clean fixtures
dev-reset agent="0":
    psql -h localhost -p 5488 -U postgres -c "DROP DATABASE IF EXISTS pgaccel_a{{agent}};" -c "CREATE DATABASE pgaccel_a{{agent}} TEMPLATE pgaccel_shared;"

# === GPU Kernels ===

# Build GPU kernel library
gpu-build:
    cmake -B pgaccel-kernels/build -S pgaccel-kernels && cmake --build pgaccel-kernels/build

# Run GPU kernel tests
gpu-test: gpu-build
    ./pgaccel-kernels/build/test_device

# === CI ===

# Run full CI locally (lint + test + integration)
ci: fmt-check lint deny test dev-up dev-test-all dev-down

# Build installable pgrx package
package pg="17":
    cargo pgrx package --pg-config $(which pg_config) --features pg{{pg}}
