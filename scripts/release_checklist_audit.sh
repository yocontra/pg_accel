#!/usr/bin/env bash
set -euo pipefail

checklist="${RELEASE_CHECKLIST_EVIDENCE_PATH:-docs/release-checklist-1.0.md}"

if [ -z "$checklist" ] || [ ! -f "$checklist" ] || [ ! -r "$checklist" ]; then
    echo "release checklist audit: evidence path must name a readable regular file: $checklist" >&2
    exit 1
fi

required_patterns=(
    "CUDA, NVIDIA, and PG-Strom are owner-deferred"
    "PostgreSQL native comparison passes"
    "Coverage reaches at least 90%"
    "Metal stress gate passes"
    "Required hosted CI ship-bar jobs pass"
    "Release verification matrix passes"
    "Release checklist synchronization is complete"
)

missing=0
for pattern in "${required_patterns[@]}"; do
    if ! grep -q -F -- "$pattern" "$checklist"; then
        # External evidence ledgers may retain the original row title. Both
        # spellings name the same mandatory CI gate; neither may be omitted.
        if [ "$pattern" = "Required hosted CI ship-bar jobs pass" ] && \
            grep -q -F -- "Required CI ship-bar jobs pass" "$checklist"; then
            continue
        fi
        echo "missing checklist item matching: $pattern" >&2
        missing=1
    fi
done

placeholder_matches="$(grep -n -E -- '<(sha-or-url|url|sha|name)>|<release-url>' "$checklist" || true)"
unchecked_matches="$(grep -n -- '^- \[ \]' "$checklist" || true)"
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

echo "release checklist audit: PASS ($checklist)"
