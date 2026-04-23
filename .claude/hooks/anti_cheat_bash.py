#!/usr/bin/env python3
"""PreToolUse hook for Bash.

Blocks commands known to sidestep verification: commit/push without hooks, running
ignored tests to "prove" they pass, single-threaded PG comparisons, etc. Exit 2 =
block; stderr is shown to Claude.
"""
from __future__ import annotations

import json
import re
import sys

BANNED = [
    (
        re.compile(r"(^|[\s;&|])git\s+(?:commit|push|rebase|cherry-pick|merge)[^\n]*--no-verify\b"),
        "--no-verify bypasses pre-commit hooks. Fix the failure, don't skip the check. "
        "Rule .claude/rules/anti-cheat.md #8.",
    ),
    (
        re.compile(r"(^|[\s;&|])git\s+(?:commit|push)[^\n]*--no-gpg-sign\b"),
        "--no-gpg-sign disables commit signing. Don't bypass the build. "
        "Rule .claude/rules/anti-cheat.md #8.",
    ),
    (
        re.compile(r"cargo\s+test[^\n]*\s--\s+--ignored\b"),
        "`cargo test -- --ignored` surfaces quarantined tests on the command line but "
        "leaves them ignored in source. If the test should run, un-ignore it in source. "
        "Otherwise this is a cheat. Rule .claude/rules/anti-cheat.md #2.",
    ),
    (
        re.compile(r"max_parallel_workers_per_gather\s*=\s*0", re.IGNORECASE),
        "max_parallel_workers_per_gather=0 compares against single-threaded PG. "
        "Banned per CLAUDE.md Benchmark Rule #11.",
    ),
    (
        re.compile(r"git\s+(?:push|commit)[^\n]*-f\s+origin\s+(?:main|master)\b"),
        "Force-pushing to main/master is destructive. Confirm with the user before "
        "running this — and never as a cheat to 'land' a broken state.",
    ),
]


def main() -> None:
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        sys.exit(0)

    if data.get("tool_name") != "Bash":
        sys.exit(0)

    cmd = ((data.get("tool_input") or {}).get("command") or "").strip()
    if not cmd:
        sys.exit(0)

    issues: list[str] = []
    for regex, msg in BANNED:
        if regex.search(cmd):
            issues.append(msg)

    if issues:
        print("Anti-cheat rail triggered on Bash command:", file=sys.stderr)
        for m in issues:
            print(f"  - {m}", file=sys.stderr)
        sys.exit(2)

    sys.exit(0)


if __name__ == "__main__":
    main()
