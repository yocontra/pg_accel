#!/bin/sh
# Auto-tune Postgres for pg_accel test workloads.
# Detects container memory/CPU limits, computes optimal settings.
#
# Override any value with PG_* env vars:
#   PG_SHARED_BUFFERS=8GB  PG_WORK_MEM=512MB  docker compose up -d

set -e

# --- Hardware detection ---

detect_memory_mb() {
  # cgroup v2 hard limit
  if [ -f /sys/fs/cgroup/memory.max ]; then
    val=$(cat /sys/fs/cgroup/memory.max)
    if [ "$val" != "max" ]; then
      echo $((val / 1024 / 1024))
      return
    fi
  fi

  # cgroup v1
  if [ -f /sys/fs/cgroup/memory/memory.limit_in_bytes ]; then
    val=$(cat /sys/fs/cgroup/memory/memory.limit_in_bytes)
    if [ "$val" -lt 9000000000000000000 ] 2>/dev/null; then
      echo $((val / 1024 / 1024))
      return
    fi
  fi

  # /proc/meminfo fallback
  if [ -f /proc/meminfo ]; then
    kb=$(grep MemTotal /proc/meminfo | awk '{print $2}')
    echo $((kb / 1024))
    return
  fi

  # Default fallback
  echo 4096
}

detect_cpus() {
  # cgroup v2 CPU quota
  if [ -f /sys/fs/cgroup/cpu.max ]; then
    quota=$(cut -d' ' -f1 /sys/fs/cgroup/cpu.max)
    period=$(cut -d' ' -f2 /sys/fs/cgroup/cpu.max)
    if [ "$quota" != "max" ]; then
      cpus=$((quota / period))
      [ "$cpus" -lt 1 ] && cpus=1
      echo "$cpus"
      return
    fi
  fi

  # cgroup v1 CPU quota
  if [ -f /sys/fs/cgroup/cpu/cpu.cfs_quota_us ] && [ -f /sys/fs/cgroup/cpu/cpu.cfs_period_us ]; then
    quota=$(cat /sys/fs/cgroup/cpu/cpu.cfs_quota_us)
    period=$(cat /sys/fs/cgroup/cpu/cpu.cfs_period_us)
    if [ "$quota" -gt 0 ] 2>/dev/null; then
      cpus=$((quota / period))
      [ "$cpus" -lt 1 ] && cpus=1
      echo "$cpus"
      return
    fi
  fi

  # nproc fallback
  if command -v nproc >/dev/null 2>&1; then
    nproc
  else
    echo 4
  fi
}

RAM_MB=$(detect_memory_mb)
CPUS=$(detect_cpus)

# --- Memory settings ---

# shared_buffers: 25% of RAM, capped at 4GB
sb_mb=$((RAM_MB / 4))
[ "$sb_mb" -gt 4096 ] && sb_mb=4096
[ "$sb_mb" -lt 128 ] && sb_mb=128

# effective_cache_size: 75% of RAM
ecs_mb=$((RAM_MB * 3 / 4))

# work_mem: RAM / 128, capped at 256MB
wm_mb=$((RAM_MB / 128))
[ "$wm_mb" -gt 256 ] && wm_mb=256
[ "$wm_mb" -lt 4 ] && wm_mb=4

# --- Parallelism ---

# max_parallel_workers_per_gather: CPUs/2, capped at 8
gather=$((CPUS / 2))
[ "$gather" -gt 8 ] && gather=8
[ "$gather" -lt 1 ] && gather=1

# --- Apply env var overrides ---

SHARED_BUFFERS="${PG_SHARED_BUFFERS:-${sb_mb}MB}"
WORK_MEM="${PG_WORK_MEM:-${wm_mb}MB}"
EFFECTIVE_CACHE_SIZE="${PG_EFFECTIVE_CACHE_SIZE:-${ecs_mb}MB}"
MAX_PARALLEL_WORKERS_PER_GATHER="${PG_MAX_PARALLEL_WORKERS_PER_GATHER:-$gather}"

echo "pg_accel/autotune: ${RAM_MB}MB RAM, ${CPUS} CPUs detected"
echo "  shared_buffers=$SHARED_BUFFERS"
echo "  work_mem=$WORK_MEM"
echo "  effective_cache_size=$EFFECTIVE_CACHE_SIZE"
echo "  max_parallel_workers_per_gather=$MAX_PARALLEL_WORKERS_PER_GATHER"
echo "  random_page_cost=1.1"
echo "  jit=off"
echo "  default_statistics_target=500"

exec docker-entrypoint.sh postgres \
  -c "shared_buffers=$SHARED_BUFFERS" \
  -c "work_mem=$WORK_MEM" \
  -c "effective_cache_size=$EFFECTIVE_CACHE_SIZE" \
  -c "max_parallel_workers_per_gather=$MAX_PARALLEL_WORKERS_PER_GATHER" \
  -c "random_page_cost=1.1" \
  -c "jit=off" \
  -c "default_statistics_target=500"
