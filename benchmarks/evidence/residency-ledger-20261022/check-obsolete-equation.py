"""Negative control: original script equation applied to real ready receipts.

This does not rerun the old script or alter observations. Expected failure is
the old relation-only equation, not a production engine failure.
"""

import json
import sys
from pathlib import Path

records = [json.loads(line) for line in Path(sys.argv[1]).read_text().splitlines()]
baseline = records[0]["live_bytes"]
owners = [record for record in records if record["kind"] == "owner" and record["phase"] == "ready"]
observed = next(record for record in records if record["kind"] == "checkpoint" and record["phase"] == "ready")
assert len(owners) == 2
old_expected = baseline + sum(owner["raw_bytes"] + owner["derived_bytes"] for owner in owners)
omitted = sum(owner["workspace_bytes"] + owner["output_bytes"] for owner in owners)
print(json.dumps({"baseline": baseline, "owners": owners, "observer": observed}, sort_keys=True), flush=True)
print(f"obsolete_expected={old_expected} observed={observed['live_bytes']} omitted_idle_pools={omitted}", flush=True)
assert observed["live_bytes"] == old_expected, "OBSOLETE_RELATION_ONLY_EQUATION_FAILED"
