#!/usr/bin/env python3
"""PreToolUse hook for Edit / Write / MultiEdit.

Blocks the most load-bearing anti-cheat patterns from .claude/rules/anti-cheat.md
at the moment they would land in source files. Exit 2 = block; stderr is shown to
Claude.

Bypass: add `// anti-cheat-allow: <specific reason>` on the same or immediately
preceding line. The reason must cite an issue, upstream bug, or concrete blocker.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

# (regex, scope suffixes or "*", human message)
# Scope limits which file suffixes the pattern applies to. Use "*" for any path.
BANNED = [
    (
        re.compile(r"^\s*#\[ignore(?:\s*=\s*\"[^\"]*\")?\]", re.MULTILINE),
        (".rs",),
        "#[ignore] disables a test. If the test catches a bug, fix the bug. "
        "If platform-quarantine is genuinely required, add "
        "`// anti-cheat-allow: <issue/blocker>` above it. "
        "Rule .claude/rules/anti-cheat.md #2.",
    ),
    (
        re.compile(r"\b(todo|unimplemented)!\(\)"),
        (".rs",),
        "todo!() / unimplemented!() ships as a panic. If you can't implement it, "
        "stop and say so — don't land a stub. "
        "Rule .claude/rules/anti-cheat.md #7.",
    ),
    (
        re.compile(r"\.unwrap_or\(\s*(?:Vec::new\(\)|vec!\[\s*\])\s*\)"),
        (".rs",),
        "unwrap_or(Vec::new()) on a Result silently swallows errors. "
        "Propagate via `?`, log via `tracing::error!`, or panic. "
        "An empty result from a failed GPU dispatch corrupts queries silently. "
        "Rule .claude/rules/anti-cheat.md #4.",
    ),
    (
        re.compile(r"max_parallel_workers_per_gather\s*=\s*0", re.IGNORECASE),
        ("*",),
        "max_parallel_workers_per_gather=0 compares against single-threaded PG. "
        "Banned per CLAUDE.md Benchmark Rule #11 and anti-cheat rule #3.",
    ),
    (
        re.compile(r"--no-verify\b"),
        ("*",),
        "--no-verify bypasses pre-commit hooks. Fix the underlying issue. "
        "Rule .claude/rules/anti-cheat.md #8.",
    ),
]

BYPASS_RE = re.compile(r"anti-cheat-allow:\s*\S")

# Always skip these paths — they legitimately contain banned patterns as documentation
# or metadata, and scanning them would produce infinite false positives.
SKIP_PREFIXES = (".claude/", "target/", ".git/")
SKIP_SUFFIXES = (".md", ".lock", ".json", ".toml", ".yaml", ".yml")


def path_is_skipped(path: Path) -> bool:
    s = str(path)
    # Absolute paths: strip a leading CWD segment match if obvious
    if any(seg in s for seg in (".claude/rules/", ".claude/hooks/")):
        return True
    for pref in SKIP_PREFIXES:
        if pref in s:
            return True
    return path.suffix in SKIP_SUFFIXES


def scope_matches(path: Path, scope: tuple[str, ...]) -> bool:
    if "*" in scope:
        return True
    return path.suffix in scope


def has_bypass(new_content: str, match_start: int, match_end: int) -> bool:
    # Inspect the flagged line and the line immediately above for an allow-comment.
    line_start = new_content.rfind("\n", 0, match_start) + 1
    line_end = new_content.find("\n", match_end)
    if line_end == -1:
        line_end = len(new_content)
    # Include up to ~2 preceding lines for an above-comment case.
    above_start = line_start
    for _ in range(2):
        prev = new_content.rfind("\n", 0, above_start - 1)
        if prev == -1:
            above_start = 0
            break
        above_start = prev + 1
    return bool(BYPASS_RE.search(new_content[above_start:line_end]))


def check_content(path: Path, new_content: str) -> list[str]:
    if not new_content or path_is_skipped(path):
        return []
    hits: list[str] = []
    for regex, scope, msg in BANNED:
        if not scope_matches(path, scope):
            continue
        for m in regex.finditer(new_content):
            if has_bypass(new_content, m.start(), m.end()):
                continue
            hits.append(msg)
            break  # one report per pattern per file is enough
    return hits


def main() -> None:
    try:
        data = json.load(sys.stdin)
    except json.JSONDecodeError:
        sys.exit(0)

    tool_name = data.get("tool_name", "")
    tool_input = data.get("tool_input") or {}
    targets: list[tuple[str, str]] = []

    if tool_name == "Write":
        targets.append((tool_input.get("file_path", ""), tool_input.get("content", "") or ""))
    elif tool_name == "Edit":
        targets.append((tool_input.get("file_path", ""), tool_input.get("new_string", "") or ""))
    elif tool_name == "MultiEdit":
        fp = tool_input.get("file_path", "")
        for e in tool_input.get("edits", []) or []:
            targets.append((fp, e.get("new_string", "") or ""))
    else:
        sys.exit(0)

    issues: list[str] = []
    for path_str, content in targets:
        if not path_str:
            continue
        issues.extend(check_content(Path(path_str), content))

    if issues:
        # Preserve first-seen order, dedupe.
        seen: dict[str, None] = {}
        for m in issues:
            seen.setdefault(m, None)
        print("Anti-cheat rail triggered:", file=sys.stderr)
        for m in seen:
            print(f"  - {m}", file=sys.stderr)
        print(
            "\nTo bypass with justification, add "
            "`// anti-cheat-allow: <specific reason>` on the same or preceding line. "
            "Reason must cite an issue, upstream bug, or concrete blocker.",
            file=sys.stderr,
        )
        sys.exit(2)

    sys.exit(0)


if __name__ == "__main__":
    main()
