#!/usr/bin/env bash
set -euo pipefail

checklist="docs/release-checklist-1.0.md"

required_patterns=(
    "CUDA, NVIDIA, and PG-Strom are owner-deferred"
    "PostgreSQL native comparison passes"
    "Coverage reaches at least 90%"
    "Metal stress gate passes"
    "Required CI ship-bar jobs pass"
    "Release verification matrix passes"
    "Release checklist synchronization is complete"
)

missing=0
for pattern in "${required_patterns[@]}"; do
    if ! rg -q -F "$pattern" "$checklist"; then
        echo "missing checklist item matching: $pattern" >&2
        missing=1
    fi
done

placeholder_matches="$(rg -n '<(sha-or-url|url|sha)>|<release-url>' "$checklist" || true)"
unchecked_matches="$(rg -n '^- \[ \]' "$checklist" || true)"
if [ -n "$placeholder_matches" ]; then
    printf '%s\n' "$placeholder_matches" >&2
fi
if [ -n "$unchecked_matches" ]; then
    printf '%s\n' "$unchecked_matches" >&2
fi
placeholder_count="$(printf '%s\n' "$placeholder_matches" | sed '/^$/d' | wc -l | tr -d ' ')"
unchecked_count="$(printf '%s\n' "$unchecked_matches" | sed '/^$/d' | wc -l | tr -d ' ')"

if [ "$missing" -ne 0 ]; then
    exit 1
fi
if [ "$placeholder_count" -ne 0 ] || [ "$unchecked_count" -ne 0 ]; then
    echo "release checklist audit: FAIL (${unchecked_count} unchecked item(s), ${placeholder_count} placeholder evidence token(s))" >&2
    exit 1
fi

echo "release checklist audit: PASS"
