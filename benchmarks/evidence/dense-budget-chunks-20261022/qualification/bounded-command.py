"""Bound only the directly owned psql child; retain its actual exit status."""
import subprocess
import sys

try:
    result = subprocess.run(sys.argv[1:], timeout=1800, check=False)
except subprocess.TimeoutExpired:
    print("OWNED_PSQL_TIMEOUT: 1800 seconds; child killed and reaped", file=sys.stderr)
    sys.exit(124)
sys.exit(result.returncode if result.returncode >= 0 else 128 - result.returncode)
