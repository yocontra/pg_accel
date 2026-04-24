#!/usr/bin/env bash
# Doc citation parity check.
#
# Extracts `<path>:<line>` citations from CLAUDE.md, ARCHITECTURE.md, TODO.md
# and validates:
#   1. File exists at that path (relative to repo root).
#   2. Line number is within the file's total line count.
#
# Exits non-zero if any citation is missing or out-of-range, so CI can gate on it.
# See .claude/rules/anti-cheat.md §10 ("cite file:line for code claims").

set -euo pipefail

usage() {
    cat <<EOF
Usage: $0 [--help] [--verbose]

Scans CLAUDE.md, ARCHITECTURE.md, and TODO.md for file:line citations and
verifies each points to a real file with a valid line number.

Exit codes:
  0  All citations valid
  1  One or more stale citations
  2  Script invocation error
EOF
}

VERBOSE=0
for arg in "$@"; do
    case "$arg" in
        -h|--help) usage; exit 0 ;;
        -v|--verbose) VERBOSE=1 ;;
        *) echo "unknown arg: $arg" >&2; usage >&2; exit 2 ;;
    esac
done

# Resolve repo root (script lives at scripts/doc_parity.sh).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

DOCS=("CLAUDE.md" "ARCHITECTURE.md" "TODO.md")

# File-extension allowlist for citation targets. This is the discriminator that
# keeps the regex from matching random `word:123` strings in prose.
#
#  - Source: .rs .cpp .h .hpp .c .cc .mm .metal .sql .toml .yaml .yml .json .sh
#  - Build : Justfile Makefile CMakeLists.txt Cargo.toml Cargo.lock build.rs
#  - Docs  : .md
#
# We also accept bare well-known filenames (Justfile, Makefile, CMakeLists.txt).
#
# Regex design:
#   - anchored on a word boundary or non-word char on the left
#   - path = [A-Za-z0-9_./-]+ ending in one of the allowlisted suffixes
#   - ':<digits>' with an optional '-<digits>' range tail (range only validates
#     the starting line, per spec)
#
# Excludes:
#   - URLs (http://, https://) — those have `//` which we reject via `[^/]`
#     lookbehind equivalent (bash regex has no lookbehind; we filter post-hoc)
#   - Pure numeric timestamps like `10:30` — path must contain `.` or be a known
#     bare filename

# Known bare filenames that count as paths even without an extension.
BARE_FILENAMES_RE='(Justfile|Makefile|CMakeLists\.txt|Cargo\.toml|Cargo\.lock|Dockerfile)'
# File-extension suffixes we accept as citation targets.
EXT_RE='\.(rs|cpp|cc|cxx|h|hpp|hxx|c|mm|metal|sql|toml|yaml|yml|json|sh|md|txt|py)'

# Path prefixes we try when a bare citation doesn't resolve from repo root.
# TODO.md / CLAUDE.md authors commonly write shorthand like
# `engine/ffi/planner_hooks/mod.rs:1770` meaning the file in the pg_accel
# crate. Trying these prefixes in order keeps citations compact without
# inviting drift — the line number still has to be in range of the *real*
# file we resolve to.
PATH_PREFIXES=(
    ""
    "pg_accel/src/"
    "pg_accel/"
    "pg_accel/src/engine/"
    "pg_accel/src/engine/ffi/"
    "pg_accel/src/engine/ffi/planner_hooks/"
    "pg_accel/src/engine/ffi/custom_scan/"
    "pg_accel/src/engine/executor/"
    "pg_accel/src/engine/executor/agg/"
    "pg_accel/src/engine/cost/"
    "pg_accel/src/gpu/"
    "pgaccel-kernels/src/"
    "pgaccel-kernels/include/"
    "pg_accel_bench/src/"
)

# External references we recognize but cannot validate locally. These are
# reported as warnings (not failures) so cited third-party file:line pairs
# don't rot silently in TODO.md without blocking CI.
# Any citation whose path starts with '/' or contains 'AdaptiveCpp/' or
# is a bare-filename PG internal (pathkeys.c etc.) lands here.
is_external_ref() {
    local path="$1"
    case "$path" in
        /*) return 0 ;;                      # absolute path (e.g. /Projects/AdaptiveCpp/...)
        *AdaptiveCpp/*|*LLVMToBackend.cpp) return 0 ;;
        pathkeys.c|relpath.c|allpaths.c|createplan.c|setrefs.c) return 0 ;;
        metal_queue.cpp) return 0 ;;         # AdaptiveCpp runtime file (not in-tree)
    esac
    return 1
}

# Try each prefix. If the cited line number is in-range for the resolved
# file, prefer that match — this disambiguates bare `mod.rs:1915` style
# citations that could resolve under multiple prefixes. Falls back to the
# first existing path if no prefix's line-count accommodates the line.
#
# This is NOT "accept off-by-N drift": each candidate still has to be a
# real file on disk, and the final selection is the one whose size proves
# the citation is valid. If no candidate has enough lines, we return the
# first match so the caller reports an honest OUT-OF-RANGE error against
# the best guess.
resolve_path() {
    local raw="$1"
    local lineno="$2"
    local first=""
    for p in "${PATH_PREFIXES[@]}"; do
        local cand="${p}${raw}"
        if [[ -f "$cand" ]]; then
            [[ -z "$first" ]] && first="$cand"
            local tl
            tl=$(wc -l < "$cand" | tr -d ' ')
            if (( lineno >= 1 && lineno <= tl + 1 )); then
                printf '%s' "$cand"
                return 0
            fi
        fi
    done
    if [[ -n "$first" ]]; then
        printf '%s' "$first"
        return 0
    fi
    return 1
}

total_found=0
total_ok=0
total_missing_file=0
total_out_of_range=0
total_external=0
declare -a FAIL_LINES=()
declare -a WARN_LINES=()
declare -a PER_DOC_COUNT=()

scan_doc() {
    local doc="$1"
    local found_in_doc=0

    if [[ ! -f "$doc" ]]; then
        echo "warn: doc not found: $doc" >&2
        PER_DOC_COUNT+=("$doc:0")
        return 0
    fi

    # Grep each line, then pick out citation tokens. We use a Perl-compatible
    # regex via `grep -oP` if available; otherwise fall back to awk.
    #
    # Token capture: every occurrence of path:line[-line] where path ends in
    # a known extension OR is a known bare filename.

    # Build one mega-regex for grep -oP.
    local re="(([A-Za-z0-9_./-]+${EXT_RE})|${BARE_FILENAMES_RE}):[0-9]+(-[0-9]+)?"

    # Read citations line-by-line to retain doc line number for reporting.
    local docline=0
    while IFS= read -r line; do
        docline=$((docline + 1))
        # Extract all candidate tokens on this line.
        # `grep -oE` is portable; we anchor-validate each match below.
        local tokens
        tokens=$(printf '%s\n' "$line" | grep -oE "$re" || true)
        [[ -z "$tokens" ]] && continue

        while IFS= read -r tok; do
            [[ -z "$tok" ]] && continue

            # Reject URL-ish tokens (shouldn't match the regex anyway, but
            # belt-and-suspenders for things like `http:11` substrings).
            if [[ "$tok" == *"//"* ]]; then continue; fi

            # Split path:lineno[-extra].
            local path="${tok%:*}"
            local tail="${tok##*:}"
            local lineno="${tail%%-*}"

            # Skip paths containing '://' style prefixes if any slipped through.
            case "$path" in
                *:*) continue ;;
            esac

            # Skip obviously-not-a-path tokens — single bare word that isn't an
            # allowlisted filename and has no slash or dot.
            if [[ "$path" != *"/"* && "$path" != *"."* ]]; then
                case "$path" in
                    Justfile|Makefile|Dockerfile) : ;;
                    *) continue ;;
                esac
            fi

            found_in_doc=$((found_in_doc + 1))
            total_found=$((total_found + 1))

            # Third-party / absolute paths: warn, don't fail.
            if is_external_ref "$path"; then
                total_external=$((total_external + 1))
                WARN_LINES+=("$doc:$docline  EXTERNAL (not validated)  $path:$lineno")
                continue
            fi

            # Try to resolve via known prefixes, preferring a candidate whose
            # line count can actually contain the cited line.
            local resolved
            if ! resolved=$(resolve_path "$path" "$lineno"); then
                total_missing_file=$((total_missing_file + 1))
                FAIL_LINES+=("$doc:$docline  MISSING FILE  $path:$lineno")
                continue
            fi

            # Count lines in the target file.
            local total_lines
            total_lines=$(wc -l < "$resolved" | tr -d ' ')
            # Allow citation = total_lines + 1 (pointing at EOF is a common idiom).
            if (( lineno < 1 || lineno > total_lines + 1 )); then
                total_out_of_range=$((total_out_of_range + 1))
                FAIL_LINES+=("$doc:$docline  OUT OF RANGE  $path:$lineno (resolved: $resolved; file has $total_lines lines)")
                continue
            fi

            total_ok=$((total_ok + 1))
            if (( VERBOSE )); then
                echo "ok  $doc:$docline  $path:$lineno -> $resolved"
            fi
        done <<< "$tokens"
    done < "$doc"

    PER_DOC_COUNT+=("$doc:$found_in_doc")
}

for doc in "${DOCS[@]}"; do
    scan_doc "$doc"
done

echo "=== doc_parity summary ==="
for entry in "${PER_DOC_COUNT[@]}"; do
    echo "  $entry citations"
done
echo "  total extracted: $total_found"
echo "  ok:              $total_ok"
echo "  external (warn): $total_external"
echo "  missing file:    $total_missing_file"
echo "  out of range:    $total_out_of_range"

if (( total_external > 0 )) && (( VERBOSE )); then
    echo
    echo "=== external references (not validated) ==="
    for warn in "${WARN_LINES[@]}"; do
        echo "  $warn"
    done
fi

if (( total_missing_file > 0 || total_out_of_range > 0 )); then
    echo
    echo "=== stale citations ==="
    for fail in "${FAIL_LINES[@]}"; do
        echo "  $fail"
    done
    echo
    echo "FAIL: $((total_missing_file + total_out_of_range)) stale citation(s)."
    echo "Fix by reading the current source and correcting the line number,"
    echo "or delete the citation if the referenced code is gone."
    exit 1
fi

echo "PASS"
exit 0
