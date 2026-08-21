#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "usage: $0 <pg_config> <archive.tar.gz> [evidence-file]" >&2
    exit 2
}

[ "$#" -ge 2 ] && [ "$#" -le 3 ] || usage

pg_config="$1"
archive="$2"
evidence_file="${3:-}"

if [ ! -x "$pg_config" ]; then
    echo "error: pg_config is not executable: $pg_config" >&2
    exit 2
fi
if [ ! -f "$archive" ]; then
    echo "error: release archive does not exist: $archive" >&2
    exit 2
fi

archive_dir="$(cd "$(dirname "$archive")" && pwd -P)"
archive="$archive_dir/$(basename "$archive")"
archive_name="$(basename "$archive")"
outer_manifest="${archive_name}.sha256"

if [ ! -f "${archive}.sha256" ]; then
    echo "error: release archive checksum is missing: ${archive}.sha256" >&2
    exit 1
fi

if ! awk -v expected="$archive_name" '
    NR != 1 || NF != 2 || $2 != expected || $1 !~ /^[[:xdigit:]]{64}$/ {
        bad = 1
    }
    END { exit bad || NR != 1 ? 1 : 0 }
' "${archive}.sha256"; then
    echo "error: release archive checksum must contain exactly the target archive" >&2
    exit 1
fi

verify_manifest() {
    local directory="$1"
    local manifest="$2"
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$directory" && sha256sum -c "$manifest")
    elif command -v shasum >/dev/null 2>&1; then
        (cd "$directory" && shasum -a 256 -c "$manifest")
    else
        echo "error: neither sha256sum nor shasum is available" >&2
        return 1
    fi
}

hash_file() {
    local path="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$path" | awk '{print $1}'
    else
        shasum -a 256 "$path" | awk '{print $1}'
    fi
}

verify_manifest "$archive_dir" "$outer_manifest"

test_root="$(mktemp -d "${TMPDIR:-/tmp}/pgaccel-package-smoke.XXXXXX")"
pg_started=0
pg_bindir=""
server_log="$test_root/postgresql.log"

cleanup() {
    local status=$?
    set +e
    if [ "$pg_started" = 1 ] && [ -n "$pg_bindir" ]; then
        "$pg_bindir/pg_ctl" -D "$test_root/data" -m fast stop >/dev/null 2>&1
    fi
    if [ "$status" -ne 0 ] && [ -f "$server_log" ]; then
        echo "release package smoke server log:" >&2
        tail -80 "$server_log" | sed 's/^/  | /' >&2
    fi
    rm -rf -- "$test_root"
    return "$status"
}
trap cleanup EXIT

python3 - "$archive" <<'PY'
import posixpath
import re
import sys
import tarfile


def fail(message: str) -> None:
    raise SystemExit(f"error: unsafe release archive: {message}")


archive = sys.argv[1]
with tarfile.open(archive, "r:gz") as bundle:
    members = bundle.getmembers()

if not members:
    fail("archive is empty")

names: set[str] = set()
roots: set[str] = set()
member_by_name: dict[str, tarfile.TarInfo] = {}
for member in members:
    name = member.name
    if not name or "\0" in name or "\n" in name or "\r" in name:
        fail("member has an empty or unrepresentable name")
    if name.startswith("/") or posixpath.normpath(name).startswith("../"):
        fail(f"member escapes the extraction root: {name!r}")
    normalized = posixpath.normpath(name)
    if normalized in {"", ".", ".."} or normalized != name:
        fail(f"member name is not normalized: {name!r}")
    if name in names:
        fail(f"duplicate member: {name!r}")
    names.add(name)
    member_by_name[name] = member
    roots.add(name.split("/", 1)[0])

    if not (member.isdir() or member.isfile() or member.issym()):
        fail(f"unsupported member type: {name!r}")
    if member.issym():
        target = member.linkname
        if not target or target.startswith("/"):
            fail(f"symlink has an empty or absolute target: {name!r}")
        resolved = posixpath.normpath(posixpath.join(posixpath.dirname(name), target))
        if resolved.startswith("../") or resolved.split("/", 1)[0] != name.split("/", 1)[0]:
            fail(f"symlink escapes its package root: {name!r} -> {target!r}")

if len(roots) != 1:
    fail(f"expected one top-level package root, found {len(roots)}")
root = next(iter(roots))
if re.fullmatch(r"pg_accel-pg(?:18|19)", root) is None:
    fail(f"unexpected package root: {root!r}")
root_member = member_by_name.get(root)
if root_member is None or not root_member.isdir():
    fail("top-level package root is not a directory")
PY
tar -xzf "$archive" -C "$test_root"

package_root=""
package_count=0
for candidate in "$test_root"/pg_accel-pg*; do
    [ -d "$candidate" ] || continue
    package_root="$candidate"
    package_count=$((package_count + 1))
done
if [ "$package_count" -ne 1 ] || [ -z "$package_root" ]; then
    echo "error: expected exactly one pg_accel-pg* package root, found $package_count" >&2
    exit 1
fi
if [ ! -x "$package_root/install.py" ]; then
    echo "error: package installer is missing or not executable: $package_root/install.py" >&2
    exit 1
fi
if [ ! -f "$package_root/SHA256SUMS" ]; then
    echo "error: package payload checksum manifest is missing" >&2
    exit 1
fi
python3 - "$package_root" <<'PY'
import pathlib
import re
import stat
import sys


def fail(message: str) -> None:
    raise SystemExit(f"error: invalid package checksum manifest: {message}")


root = pathlib.Path(sys.argv[1]).resolve()
manifest = root / "SHA256SUMS"
if manifest.is_symlink() or not manifest.is_file():
    fail("SHA256SUMS must be a regular file")

listed: list[str] = []
for line in manifest.read_text(encoding="utf-8").splitlines():
    match = re.fullmatch(r"[0-9a-f]{64}  (.+)", line)
    if match is None:
        fail("entry is not canonical")
    listed.append(match.group(1))
if not listed or listed != sorted(set(listed)):
    fail("entries must be nonempty, unique, and sorted")

actual: list[str] = []
for path in root.rglob("*"):
    mode = path.lstat().st_mode
    if stat.S_ISDIR(mode):
        continue
    if not (stat.S_ISREG(mode) or stat.S_ISLNK(mode)):
        fail(f"payload contains a special file: {path.relative_to(root)}")
    if stat.S_ISLNK(mode):
        resolved = path.resolve()
        if not resolved.is_relative_to(root) or not resolved.is_file():
            fail(f"payload contains an unsafe symlink: {path.relative_to(root)}")
    if path != manifest:
        actual.append(path.relative_to(root).as_posix())
actual.sort()
if listed != actual:
    fail("entries do not exactly cover the extracted payload")
PY
verify_manifest "$package_root" SHA256SUMS

stage="$test_root/stage"
"$package_root/install.py" \
    --package-root "$package_root" \
    --pg-config "$pg_config" \
    --destdir "$stage"

pg_bindir="$("$pg_config" --bindir)"
pkglibdir="$stage$("$pg_config" --pkglibdir)"
sharedir="$stage$("$pg_config" --sharedir)"
socket_dir="$test_root/socket"
mkdir -p "$socket_dir"

for value in "$pkglibdir" "$sharedir" "$socket_dir"; do
    case "$value" in
        *"'"* | *$'\n'*)
            echo "error: generated PostgreSQL path cannot be represented safely" >&2
            exit 1
            ;;
    esac
done

port="${PGACCEL_PACKAGE_SMOKE_PORT:-55432}"
case "$port" in
    *[!0-9]* | "")
        echo "error: PGACCEL_PACKAGE_SMOKE_PORT must be an integer" >&2
        exit 2
        ;;
esac
if [ "$port" -lt 1024 ] || [ "$port" -gt 65535 ]; then
    echo "error: PGACCEL_PACKAGE_SMOKE_PORT must be in [1024, 65535]" >&2
    exit 2
fi

"$pg_bindir/initdb" -D "$test_root/data" --no-locale --encoding=UTF8 >/dev/null
{
    printf "dynamic_library_path = '%s'\n" "$pkglibdir"
    printf "extension_control_path = '\$system:%s'\n" "$sharedir"
    printf "shared_preload_libraries = 'pg_accel'\n"
    printf "listen_addresses = ''\n"
    printf "unix_socket_directories = '%s'\n" "$socket_dir"
    printf "port = %s\n" "$port"
} >> "$test_root/data/postgresql.conf"

"$pg_bindir/pg_ctl" \
    -D "$test_root/data" \
    -l "$server_log" \
    -w start
pg_started=1

connection=("$pg_bindir/psql" -h "$socket_dir" -p "$port" -d postgres -v ON_ERROR_STOP=1)
"${connection[@]}" -q -c "CREATE EXTENSION pg_accel;"
observed_version="$("${connection[@]}" -Atc "SELECT pg_accel_version();")"
stats_rows="$("${connection[@]}" -Atc "SELECT count(*) FROM pg_accel_stats();")"
expected_version="$(awk -F"'" '/^[[:space:]]*default_version[[:space:]]*=/{print $2; exit}' \
    "$package_root/share/extension/pg_accel.control")"

if [ -z "$expected_version" ] || [ "$observed_version" != "$expected_version" ]; then
    echo "error: loaded extension version mismatch: expected=$expected_version observed=$observed_version" >&2
    exit 1
fi
if [ "$stats_rows" != "1" ]; then
    echo "error: pg_accel_stats() returned an unexpected row count: $stats_rows" >&2
    exit 1
fi

"$pg_bindir/pg_ctl" -D "$test_root/data" -m fast -w stop
pg_started=0

if grep -Eiq '(FATAL|PANIC):|could not load library|undefined symbol' "$server_log"; then
    echo "error: isolated package smoke log contains a fatal load/runtime error" >&2
    exit 1
fi

archive_sha256="$(hash_file "$archive")"
manifest_sha256="$(hash_file "$package_root/SHA256SUMS")"
server_log_sha256="$(hash_file "$server_log")"
git_head="$(git rev-parse --verify HEAD 2>/dev/null || printf 'unknown')"

if [ -n "$evidence_file" ]; then
    evidence_dir="$(dirname "$evidence_file")"
    mkdir -p "$evidence_dir"
    evidence_tmp="$(mktemp "$evidence_dir/.package-smoke.XXXXXX")"
    {
        echo "schema_version=1"
        echo "status=PASS"
        echo "git_head=$git_head"
        echo "archive_name=$archive_name"
        echo "archive_sha256=$archive_sha256"
        echo "package_manifest_sha256=$manifest_sha256"
        echo "pg_version=$("$pg_config" --version)"
        echo "extension_version=$observed_version"
        echo "stats_rows=$stats_rows"
        echo "platform=$(uname -s)"
        echo "architecture=$(uname -m)"
        echo "server_log_sha256=$server_log_sha256"
        echo "server_log_audit=clean"
    } > "$evidence_tmp"
    mv "$evidence_tmp" "$evidence_file"
fi

echo "release package smoke: PASS archive=$archive_name pg=$observed_version stats_rows=$stats_rows"
