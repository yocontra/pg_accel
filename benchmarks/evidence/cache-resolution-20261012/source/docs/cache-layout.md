# AdaptiveCpp cache observation and clearing

The pinned AdaptiveCpp runtime treats `ACPP_APPDB_DIR` as its **base** directory.
On Metal/macOS its cache is `<base>/apps/global/jit-cache`, not
`<base>/global/jit-cache`. An unset modern override permits
`HIPSYCL_APPDB_DIR`; a defined empty modern override suppresses the legacy value.
Configured values use the runtime's whitespace-delimited first token; literal
`~` is not expanded. XDG/HOME paths, unlike configured override tokens, retain
spaces.

Without a configured base the precedence is defined `XDG_DATA_HOME` plus
`acpp`, defined `HOME` plus `.acpp`, passwd home plus `.acpp`, then cwd plus
`.acpp`. Defined empty XDG/HOME values therefore produce relative paths.
The native resolver uses AdaptiveCpp's read-only settings accessor, including
its configuration-file rules. It never calls `persistent_storage::get()`,
creates directories, opens an application database or initializes a device.
Read-only settings may be lazily cached.

The native API resolves its own executable using the pinned runtime helper.
On Linux the application directory is the executable basename plus the runtime
hash of its full path; `global` is used when no executable path is available.
The pinned Darwin helper reads `/proc/self/exe`, which is unavailable there,
and consequently also does not discover executable-adjacent config files.
Native path and hash fixtures run on macOS are not live Linux qualification.
Environment/configuration must match the target runtime; changing it after
AdaptiveCpp has initialized does not relocate that runtime's existing storage.

## Tool paths

`scripts/acpp_cache.py` is the shared resolver for artifact collection,
diagnostics and `just clear-jit`. Its implicit discovery supports the pinned
Darwin behavior. On other platforms specify the actual target process's cache:

```sh
python3 scripts/acpp_cache.py resolve --cache-dir /owned/base/apps/global/jit-cache
```

`--cache-dir` takes precedence over the tool-only `PGACCEL_ACPP_CACHE_DIR`
environment variable. Invalid explicit values fail rather than falling back.
This tool variable does **not** change AdaptiveCpp's runtime storage. In
particular, an explicit observation path must describe the server's environment,
not merely the environment of a client connecting to it.

`PGACCEL_ACPP_CACHE_DIR` passes unchanged through `clear-jit`, `gpu-test-cold`,
`gpu-test-cold-all` and `gpu-stress-archive`. The resident-concurrency benchmark
retains its separate recursive fingerprint of the entire override tree; this
fix does not narrow that evidence scope.

## Clearing requires an intentional, exclusive quiet window

Only explicitly requested cold-cache workflows should clear a cache. Coordinate
with other runtime users first. Never clear a shared user cache to prepare an
isolated diagnostic: use an owned private AdaptiveCpp base for that runtime.

The canonical clearer accepts only an absolute `apps/<application>/jit-cache`
target. It rejects `..`, symlink traversal, broad roots, home/base roots,
non-owned or group/world-writable cache directories, and non-regular entries.
It preflights every entry before deletion and uses anchored directory
descriptors with no-follow traversal. It unlinks regular cache files only,
never recursively removes directories, and retains the cache directory.
Detected changes abort, but these checks are not atomic: an exclusive quiet
window is required. Earlier completed unlinks cannot be rolled back. An absent
cache is distinct from an unresolved or unreadable one. Protected home/base
directories are compared by existing filesystem identity as well as their path.
The runtime base uses the same token parsing and precedence as path discovery.

Relative runtime paths remain observable, but clearing never anchors them to a
client's working directory. Supply an explicit absolute tool target for clearing
when the runtime base is relative; determine that target in the server's context.

Use `python3 scripts/acpp_cache.py clear --cache-dir /exact/owned/apps/global/jit-cache`
or set the tool-only environment variable for `just clear-jit`. Clearing does
not remove the adjacent application database. No live user cache is cleared by
the regression suite: destructive checks use only owned temporary fixtures.

## Focused verification

```sh
python3 -m unittest scripts/tests/test_acpp_cache.py -v
```

The native `test_archive_cache_path` CMake target is host-only. It checks path
resolution, exact archive counts, nonexistent and unreadable directories,
truncation and no storage creation without calling `pgaccel_init`.
Set `PGACCEL_ARCHIVE_PATH_TEST` to its absolute executable path when running the
Python suite to exercise fresh-subprocess native/Python parity. Without that
executable the cross-language case is explicitly skipped, not counted as proven.
