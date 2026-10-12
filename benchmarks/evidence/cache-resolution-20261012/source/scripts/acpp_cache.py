"""Pinned Metal cache paths and deliberately narrow, fd-relative cache clearing.

Observing a path never initializes AdaptiveCpp. The Python implicit contract is
Darwin-only: other platforms must identify the actual target process's cache.
"""

from __future__ import annotations

import argparse
import contextlib
import os
import pwd
import re
import stat
import sys
from pathlib import Path
from typing import Mapping


class CachePathError(ValueError):
    pass


def passwd_home() -> str | None:
    try:
        return pwd.getpwuid(os.getuid()).pw_dir
    except KeyError:
        return None


def _explicit(value: str) -> Path:
    if not value or not value.strip() or any(ord(char) < 32 for char in value):
        raise CachePathError(
            "explicit cache directory is empty or contains control characters"
        )
    path = Path(value)
    if not path.is_absolute() or ".." in path.parts:
        raise CachePathError("explicit cache directory must be absolute without '..'")
    return path


def _runtime_base(env: Mapping[str, str]) -> Path:
    # Match std::stringstream's classic whitespace token, not expanduser() or
    # shell interpretation. A defined-empty modern variable hides the legacy.
    configured = env.get("ACPP_APPDB_DIR", env.get("HIPSYCL_APPDB_DIR", ""))
    token = re.match(r"[ \t\n\v\f\r]*([^ \t\n\v\f\r]+)", configured)
    if token:
        base = Path(token[1])
    elif "XDG_DATA_HOME" in env:
        base = Path(env["XDG_DATA_HOME"]) / "acpp"
    elif "HOME" in env:
        base = Path(env["HOME"]) / ".acpp"
    else:
        home = passwd_home()
        base = (Path(home) if home is not None else Path.cwd()) / ".acpp"
    return base


def resolve_cache_dir(
    explicit: str | None = None,
    *,
    environ: Mapping[str, str] | None = None,
    platform: str | None = None,
) -> Path:
    env = os.environ if environ is None else environ
    if explicit is not None:
        return _explicit(explicit)
    if "PGACCEL_ACPP_CACHE_DIR" in env:
        return _explicit(env["PGACCEL_ACPP_CACHE_DIR"])
    if (sys.platform if platform is None else platform) != "darwin":
        raise CachePathError(
            "non-Darwin cache discovery requires --cache-dir or PGACCEL_ACPP_CACHE_DIR for the target process"
        )
    # The pinned Darwin runtime cannot discover /proc/self/exe: it selects
    # global and does not discover executable-adjacent configuration files.
    return _runtime_base(env) / "apps" / "global" / "jit-cache"


def clear_cache(path: Path, *, environ: Mapping[str, str] | None = None) -> int:
    """Unlink only regular files in one validated cache; never recurse/follow links.

    Directory descriptors anchor the operation against target/ancestor swaps.
    Unsafe entries are rejected in a complete preflight before any unlink.
    Detected changes abort, but checks are not atomic: an exclusive quiet window
    is required. Already completed unlinks cannot be rolled back.
    """
    env = os.environ if environ is None else environ
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts or len(path.parts) < 5:
        raise CachePathError(
            "clear requires a specific absolute cache path without '..'"
        )
    if path.name != "jit-cache" or path.parent.parent.name != "apps":
        raise CachePathError(
            "clear requires the exact apps/<application>/jit-cache directory"
        )
    protected_roots = [_runtime_base(env)]
    home = env["HOME"] if "HOME" in env else passwd_home()
    if home is not None:
        protected_roots.append(Path(home))
    if "XDG_DATA_HOME" in env:
        protected_roots.append(Path(env["XDG_DATA_HOME"]))
    for protected in protected_roots:
        if os.path.abspath(protected) == str(path):
            raise CachePathError("refusing to clear a home or runtime base directory")
    try:
        with contextlib.ExitStack() as stack:
            flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
            descriptor = os.open(path.anchor, flags)
            stack.callback(os.close, descriptor)
            parent = descriptor
            for component in path.parts[1:]:
                parent = descriptor
                try:
                    descriptor = os.open(component, flags, dir_fd=parent)
                except FileNotFoundError:
                    return 0
                stack.callback(os.close, descriptor)
            target = os.fstat(descriptor)
            # Follow aliases only for protection comparisons, never for target
            # traversal. Compare against the opened target before any unlink.
            for protected in protected_roots:
                try:
                    identity = protected.stat()
                except FileNotFoundError:
                    continue
                if (identity.st_dev, identity.st_ino) == (target.st_dev, target.st_ino):
                    raise CachePathError(
                        "refusing to clear a home or runtime base directory"
                    )
            if target.st_uid != os.getuid() or target.st_mode & 0o022:
                raise CachePathError(
                    "cache directory must be owned and not group/world writable"
                )

            def unchanged_target():
                current = os.stat(path.name, dir_fd=parent, follow_symlinks=False)
                if (current.st_dev, current.st_ino) != (target.st_dev, target.st_ino):
                    raise CachePathError("cache target changed during clearing")

            unchanged_target()
            entries = []
            for name in os.listdir(descriptor):
                info = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                if not stat.S_ISREG(info.st_mode):
                    raise CachePathError(
                        "cache contains a symlink or non-regular entry; nothing cleared"
                    )
                entries.append((name, info.st_dev, info.st_ino))
            for name, device, inode in entries:
                unchanged_target()
                current = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                if not stat.S_ISREG(current.st_mode) or (
                    current.st_dev,
                    current.st_ino,
                ) != (device, inode):
                    raise CachePathError("cache entry changed during clearing")
                os.unlink(name, dir_fd=descriptor)
            unchanged_target()
            if os.listdir(descriptor):
                raise CachePathError(
                    "cache changed during clearing; new entries were not deleted"
                )
            return len(entries)
    except OSError as error:
        raise CachePathError(f"cannot safely clear {path}: {error}") from error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("resolve", "clear"))
    parser.add_argument("--cache-dir")
    args = parser.parse_args()
    try:
        path = resolve_cache_dir(args.cache_dir)
        if args.command == "resolve":
            print(path)
        else:
            # The client cwd cannot identify a server's relative runtime base.
            if not path.is_absolute():
                raise CachePathError(
                    "relative runtime cache requires an explicit absolute --cache-dir or PGACCEL_ACPP_CACHE_DIR for clearing"
                )
            count = clear_cache(path)
            print(f"cleared {path} ({count} regular files; directory retained)")
        return 0
    except (CachePathError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
