"""Bounded compiler-closure diagnostics; never an export acceptance overlay.

No imported receipt is executable authority. Provenance qualification and
effects integration are deliberately absent from this initial checkpoint.
"""

from __future__ import annotations

import dataclasses
import collections
import hashlib
import json
import math
import os
import pathlib
import re
import signal
import subprocess
import sys
import tempfile
import time


_IN_WATCHED_WORKER = False


class ExtractionError(ValueError):
    pass


@dataclasses.dataclass(frozen=True)
class Limits:
    seconds: float = 600
    output_bytes: int = 128 * 1024 * 1024
    rss_kib: int = 8 * 1024 * 1024
    json_depth: int = 256


def digest(path: pathlib.Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def snapshot_copy(
    source: pathlib.Path, target: pathlib.Path, maximum: int, deadline: float
) -> int:
    """Bound actual copied bytes, including a source growing during copying."""
    count = 0
    with source.open("rb") as incoming, target.open("xb") as outgoing:
        while True:
            if time.monotonic() >= deadline:
                raise ExtractionError("snapshot deadline")
            chunk = incoming.read(min(1024 * 1024, maximum - count + 1))
            if not chunk:
                return count
            count += len(chunk)
            if count > maximum:
                raise ExtractionError("snapshot byte limit")
            outgoing.write(chunk)


def decode_bounded(path: pathlib.Path, limits: Limits) -> dict:
    """Check structural nesting before Python's recursive JSON decoder runs."""
    if path.stat().st_size > limits.output_bytes:
        raise ExtractionError("JSON byte limit")
    with path.open("rb") as stream:
        data = stream.read(limits.output_bytes + 1)
    if len(data) > limits.output_bytes:
        raise ExtractionError("JSON changed beyond byte limit")
    depth = 0
    quoted = escaped = False
    for byte in data:
        if quoted:
            if escaped:
                escaped = False
            elif byte == 92:
                escaped = True
            elif byte == 34:
                quoted = False
        elif byte == 34:
            quoted = True
        elif byte in (91, 123):
            depth += 1
            if depth > limits.json_depth:
                raise ExtractionError("JSON nesting limit")
        elif byte in (93, 125):
            depth -= 1
    try:
        result = json.loads(data)
    except (ValueError, RecursionError) as error:
        raise ExtractionError("invalid JSON") from error
    if not isinstance(result, dict):
        raise ExtractionError("expected JSON object")
    return result


def run_bounded(
    argv: list[str],
    cwd: pathlib.Path,
    output: pathlib.Path,
    errors: pathlib.Path,
    limits: Limits = Limits(),
    artifacts: tuple[pathlib.Path, ...] = (),
    environment: dict[str, str] | None = None,
) -> dict:
    """Run a caller-selected diagnostic, with no shell or imported commands.

    Monitor the dedicated process group, including descendants. Output is
    streamed to files and bounded while running, not captured in Python RAM.
    A short polling interval is a watchdog, not a hard OS memory reservation.
    """
    if not isinstance(limits.seconds, (int, float)) or not math.isfinite(
        limits.seconds
    ):
        raise ExtractionError("invalid finite process deadline")
    if not 0 < limits.seconds <= 600:
        raise ExtractionError("invalid process limits")
    if any(
        type(value) is not int or value <= 0
        for value in (limits.output_bytes, limits.rss_kib, limits.json_depth)
    ):
        raise ExtractionError("invalid integer process limits")
    if limits.json_depth > 256:
        raise ExtractionError("JSON depth exceeds approved envelope")
    if not argv or limits.output_bytes > 1024**3 or limits.rss_kib > 8 * 1024**2:
        raise ExtractionError("process limits exceed approved envelope")
    if not all(hasattr(os, name) for name in ("waitid", "WNOWAIT", "P_PID")):
        raise ExtractionError("non-reaping child observation unavailable")
    start = time.monotonic()
    peak = 0
    failure = None

    def check_sizes() -> None:
        for artifact in (output, errors, *artifacts):
            if artifact.exists() and artifact.stat().st_size > limits.output_bytes:
                raise ExtractionError("process output limit")

    with output.open("xb") as out, errors.open("xb") as err:
        process = subprocess.Popen(
            [
                sys.executable,
                str(pathlib.Path(__file__).resolve()),
                "--limited-exec",
                str(limits.output_bytes),
                *argv,
            ],
            cwd=cwd,
            stdout=out,
            stderr=err,
            start_new_session=not _IN_WATCHED_WORKER,
            env=environment,
        )
        leader_owned = True
        observed = None
        try:
            while True:
                if time.monotonic() - start > limits.seconds:
                    raise ExtractionError("process deadline")
                try:
                    observed = os.waitid(
                        os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT
                    )
                except InterruptedError:
                    continue
                except ChildProcessError as error:
                    # An unexpected external reaper invalidates the PID/PGID
                    # ownership token. Never signal that numeric ID afterward.
                    leader_owned = False
                    raise ExtractionError(
                        "owned child was unexpectedly reaped"
                    ) from error
                if observed is not None:
                    break
                check_sizes()
                # ps output is machine inventory, never compiler output. The
                # watchdog itself has a timeout and only reads numeric fields.
                snapshot = subprocess.run(
                    ["ps", "-axo", "pgid=,rss="],
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=2,
                    check=True,
                )
                resident = sum(
                    int(fields[1])
                    for line in snapshot.stdout.splitlines()
                    if len(fields := line.split()) == 2
                    and int(fields[0])
                    == (os.getpgrp() if _IN_WATCHED_WORKER else process.pid)
                )
                peak = max(peak, resident)
                if resident > limits.rss_kib:
                    raise ExtractionError("process-group RSS limit")
                time.sleep(0.05)
        except BaseException as error:
            failure = error
        finally:
            # WNOWAIT retains the leader (including a zombie) until AFTER the
            # final group signal, preventing its PID/PGID from being recycled.
            # Popen.poll/kill/send_signal can reap, so none belongs above here.
            try:
                if not leader_owned:
                    pass
                elif _IN_WATCHED_WORKER:
                    # The outer watchdog owns the entire group. Killing it
                    # here would kill this worker before it records results.
                    if observed is None:
                        os.kill(process.pid, signal.SIGKILL)
                else:
                    os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except PermissionError:
                # Darwin may return EPERM for a group consisting solely of
                # zombies. Verify while the unreaped leader STILL pins PGID;
                # never suppress this error if a living member remains.
                if observed is None or _IN_WATCHED_WORKER:
                    raise
                members = subprocess.run(
                    ["ps", "-axo", "pgid=,stat="],
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    timeout=2,
                    check=True,
                )
                if any(
                    int(fields[0]) == process.pid and not fields[1].startswith("Z")
                    for line in members.stdout.splitlines()
                    if len(fields := line.split()) == 2
                ):
                    raise
            finally:
                if leader_owned:
                    process.wait(timeout=5)
        if failure:
            raise failure
        check_sizes()
    return {
        "argv": argv,
        "cwd": str(cwd.resolve()),
        "returncode": process.returncode,
        "elapsed_seconds": time.monotonic() - start,
        "observed_peak_process_group_rss_kib": peak,
        "limits": dataclasses.asdict(limits),
        "output_sha256": digest(output),
        "stderr_sha256": digest(errors),
        "effects_proven": False,
        "provenance_verified": False,
    }


@dataclasses.dataclass(frozen=True)
class GroupedRecipe:
    """Explicit local inputs, never populated from a saved proof receipt."""

    compiler: pathlib.Path
    clang_library: pathlib.Path
    llvm_library: pathlib.Path
    plugin: pathlib.Path
    extractor: pathlib.Path
    resource_directory: pathlib.Path
    sdk: pathlib.Path
    acpp_include: pathlib.Path
    kernel_include: pathlib.Path
    source: pathlib.Path


def resolve_contained(
    path: pathlib.Path, anchor: pathlib.Path, *, max_hops: int = 64
) -> pathlib.Path:
    """Resolve every symlink hop without ever leaving the reviewed SDK."""
    if not 0 < max_hops <= 64:
        raise ExtractionError("invalid symlink hop limit")
    supplied_anchor = anchor.absolute()
    anchor = anchor.resolve(strict=True)
    path = path.absolute()
    if path.is_relative_to(supplied_anchor):
        path = anchor / path.relative_to(supplied_anchor)
    if not path.is_relative_to(anchor):
        raise ExtractionError("symlink outside SDK anchor")
    pending = collections.deque(path.relative_to(anchor).parts)
    current = anchor
    hops = 0
    while pending:
        part = pending.popleft()
        if part == "..":
            if current == anchor:
                raise ExtractionError("symlink escapes SDK anchor")
            current = current.parent
            continue
        candidate = current / part
        if candidate.is_symlink():
            hops += 1
            if hops > max_hops:
                raise ExtractionError("symlink cycle or hop limit")
            target = pathlib.Path(os.readlink(candidate))
            if target.is_absolute():
                if target.is_relative_to(supplied_anchor):
                    target = anchor / target.relative_to(supplied_anchor)
                if not target.is_relative_to(anchor):
                    raise ExtractionError("symlink escapes SDK anchor")
                current = anchor
                target = target.relative_to(anchor)
            pending.extendleft(reversed(target.parts))
        else:
            candidate.stat()  # Reject dangling paths instead of inventing absence.
            current = candidate
    return current


def include_symlink_aliases(
    paths: set[pathlib.Path],
    roots: list[pathlib.Path],
    *,
    observed: set[pathlib.Path] | None = None,
    deadline: float | None = None,
    sdk_anchor: pathlib.Path | None = None,
) -> set[pathlib.Path]:
    """Preserve bounded real include aliases lost by canonical line markers.

    For example the macOS SDK's sched.h is a symlink to pthread/sched.h.
    Only aliases resolving to an already observed input are frozen; replay
    still rejects any genuinely new include. This is file discovery, not a
    declaration/reference identity join.
    """
    supplied_sdk_anchor = sdk_anchor.absolute() if sdk_anchor else None
    sdk_anchor = sdk_anchor.resolve(strict=True) if sdk_anchor else None

    def resolve(path):
        if sdk_anchor and (
            path.absolute().is_relative_to(sdk_anchor)
            or path.absolute().is_relative_to(supplied_sdk_anchor)
        ):
            return resolve_contained(path, supplied_sdk_anchor)
        try:
            return path.resolve(strict=True)
        except (OSError, RuntimeError) as error:
            raise ExtractionError("invalid include alias") from error

    canonical = {resolve(path) for path in paths}
    allowed_roots = [root.resolve(strict=True) for root in roots]
    explicit = {path.resolve(strict=True) for path in (observed or set())}
    aliases = set(paths)
    visited = 0
    for root in roots:
        for directory, directories, files in os.walk(root, followlinks=False):
            for name in (*directories, *files):
                if deadline is not None and time.monotonic() >= deadline:
                    raise ExtractionError("snapshot deadline")
                visited += 1
                if visited > 100000:
                    raise ExtractionError("include alias discovery limit")
                path = pathlib.Path(directory) / name
                if not path.is_symlink():
                    continue
                target = resolve(path)
                if not any(target.is_relative_to(root) for root in allowed_roots):
                    if target not in explicit and not (
                        sdk_anchor and target.is_relative_to(sdk_anchor)
                    ):
                        raise ExtractionError(
                            f"include symlink escapes declared roots: {path} -> {target}"
                        )
                if target in canonical:
                    aliases.add(path)
                elif path.is_dir():
                    for item in canonical:
                        if item.is_relative_to(target):
                            aliases.add(path / item.relative_to(target))
                if len(aliases) > 20000:
                    raise ExtractionError("include alias count limit")
    return aliases


def compile_grouped_snapshot(recipe: GroupedRecipe, directory: pathlib.Path) -> dict:
    """One watched worker owns copying, compiler children and JSON decoding.

    The request is generated from this live API invocation, not a proof receipt.
    Neither the request nor the result grants a trusted compiler capability.
    All nested compiler children inherit the outer owned process group.
    """
    directory = directory.absolute()
    control = pathlib.Path(
        tempfile.mkdtemp(prefix="metadata-worker-", dir=directory.parent)
    )
    request = control / "request.json"
    request.write_text(
        json.dumps(
            {
                field.name: str(getattr(recipe, field.name).absolute())
                for field in dataclasses.fields(recipe)
            }
        )
    )
    receipt = run_bounded(
        [
            sys.executable,
            str(pathlib.Path(__file__).resolve()),
            "--grouped-worker",
            str(request),
            str(directory),
        ],
        directory.parent,
        control / "result.json",
        control / "worker.err",
        limits=Limits(output_bytes=1024**3),
    )
    (control / "watchdog.json").write_text(json.dumps(receipt, indent=2))
    if receipt["returncode"]:
        raise ExtractionError(f"grouped worker failed; retained diagnostics: {control}")
    result = decode_bounded(control / "result.json", Limits())
    result["whole_task_watchdog"] = receipt
    result["worker_control_directory"] = str(control)
    return result


def _compile_grouped_snapshot(recipe: GroupedRecipe, directory: pathlib.Path) -> dict:
    """Fresh closed-input checkpoint; does NOT construct proof capability yet.

    Executables/non-platform libraries are private read-only copies. Fresh
    dyld traces are retained so loaded snapshot paths can be independently
    checked before this checkpoint becomes trusted compiler evidence.
    """
    directory = directory.resolve()
    deadline = time.monotonic() + 600
    directory.mkdir(exist_ok=False)
    tools_dir = directory / "tools"
    tools_dir.mkdir()
    copied = {}
    for name, original in {
        "clang++": recipe.compiler,
        "libclang-cpp.dylib": recipe.clang_library,
        "libLLVM.dylib": recipe.llvm_library,
        "libacpp-clang.so": recipe.plugin,
        "metadata_ast_dump": recipe.extractor,
    }.items():
        original = original.resolve(strict=True)
        target = tools_dir / name
        if original.stat().st_size > 256 * 1024**2:
            raise ExtractionError("tool snapshot size limit")
        snapshot_copy(original, target, 256 * 1024**2, deadline)
        target.chmod(0o500 if name in {"clang++", "metadata_ast_dump"} else 0o400)
        if digest(target) != digest(original):
            raise ExtractionError("tool changed during snapshot")
        copied[name] = {
            "source": str(original),
            "snapshot": str(target),
            "sha256": digest(target),
        }
    environment = {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
        "LC_ALL": "C",
        "TMPDIR": str(directory),
        "DYLD_LIBRARY_PATH": str(tools_dir),
        "DYLD_PRINT_LIBRARIES": "1",
    }
    profile = [
        str(tools_dir / "clang++"),
        "-std=gnu++17",
        "-O3",
        "-DNDEBUG",
        "-arch",
        "arm64",
        "-ffast-math",
        "-fno-fast-math",
        "-fhonor-nans",
        "-fhonor-infinities",
        "-isysroot",
        str(recipe.sdk.resolve(strict=True)),
        "-resource-dir",
        str(recipe.resource_directory.resolve(strict=True)),
        "-I",
        str(recipe.kernel_include.resolve(strict=True)),
        "-isystem",
        str(recipe.acpp_include.resolve(strict=True)),
        "-isystem",
        str(recipe.acpp_include.resolve(strict=True).parent),
        "-D__OPENSYCL__",
        "-D__HIPSYCL__",
        "-D__ADAPTIVECPP__",
        "-D__ACPP__",
        "-D__ACPP_ENABLE_LLVM_SSCP_TARGET__",
        "-include",
        str(
            recipe.acpp_include.resolve()
            / "hipSYCL/glue/llvm-sscp/hcf_registration.hpp"
        ),
        "-Xclang",
        "-disable-O0-optnone",
        "-mllvm",
        "-acpp-sscp",
        "-mllvm",
        "-acpp-sscp-kernel-opts=fast-math",
        "-fplugin=" + str(tools_dir / "libacpp-clang.so"),
        "-fpass-plugin=" + str(tools_dir / "libacpp-clang.so"),
        "-Werror=return-type",
        "-Werror=vla",
        "-Wsystem-headers",
    ]
    source = recipe.source.resolve(strict=True)
    receipts = []

    def stage(name: str, command: list[str], artifacts=()) -> pathlib.Path:
        output = directory / (name + ".out")
        receipt = run_bounded(
            command,
            directory,
            output,
            directory / (name + ".err"),
            artifacts=artifacts,
            environment=environment,
            limits=Limits(seconds=max(0, deadline - time.monotonic())),
        )
        receipts.append(receipt)
        if receipt["returncode"]:
            raise ExtractionError(
                f"{name} failed; inspect retained bounded diagnostics"
            )
        return output

    preprocessed = stage("preprocess", [*profile, "-E", str(source)])
    if preprocessed.stat().st_size > 128 * 1024**2:
        raise ExtractionError("preprocessed input too large")
    text = preprocessed.read_text()
    paths = {source}
    for match in re.finditer(r'^# \d+ ("(?:[^"\\]|\\.)*")', text, re.M):
        name = json.loads(match[1])
        if not name.startswith("<"):
            paths.add(pathlib.Path(name).absolute())
    search_roots = [
        recipe.sdk.resolve() / "usr/include",
        recipe.resource_directory.resolve() / "include",
        recipe.acpp_include.resolve().parent,
        recipe.kernel_include.resolve(),
    ]
    # Existence probes such as libc++'s __has_include(<unistd.h>) affect
    # preprocessing even when the probed file is never included. Snapshot the
    # finite declared search roots, not only line-marker inputs, so absence is
    # represented without any live filesystem fallthrough.
    observed = set(paths)
    for root in search_roots:
        for directory_name, _, filenames in os.walk(root, followlinks=False):
            if time.monotonic() >= deadline:
                raise ExtractionError("snapshot deadline")
            for name in filenames:
                candidate = pathlib.Path(directory_name) / name
                if candidate.is_file():
                    paths.add(candidate)
                if len(paths) > 20000:
                    raise ExtractionError("header search snapshot count limit")
    paths = include_symlink_aliases(
        paths,
        search_roots,
        observed=observed,
        deadline=deadline,
        sdk_anchor=recipe.sdk,
    )
    if len(paths) > 20000:
        raise ExtractionError("include count limit")
    inputs_dir = directory / "inputs"
    inputs_dir.mkdir()
    roots, inputs = [], {}
    canonical_snapshots = {}
    total = 0
    for index, path in enumerate(sorted(paths)):
        canonical = path.resolve(strict=True)
        snapshot = canonical_snapshots.get(canonical)
        if snapshot is None:
            snapshot = inputs_dir / str(index)
            total += snapshot_copy(path, snapshot, 256 * 1024**2 - total, deadline)
            snapshot.chmod(0o400)
            canonical_snapshots[canonical] = snapshot
        inputs[str(path)] = digest(snapshot)
        if digest(path) != inputs[str(path)]:
            raise ExtractionError("include changed during snapshot")
        roots.append(
            {
                "type": "file",
                "name": str(path),
                "external-contents": str(snapshot),
                "use-external-name": False,
            }
        )
    overlay = directory / "inputs.json"
    overlay.write_text(json.dumps({"version": 0, "fallthrough": False, "roots": roots}))
    frozen = [*profile, "-ivfsoverlay", str(overlay)]
    replay = stage("preprocess-frozen", [*frozen, "-E", str(source)])
    if digest(replay) != digest(preprocessed):
        raise ExtractionError("closed preprocessing differs from discovered input")
    ast = directory / "grouped.ast"
    stage(
        "serialize",
        [
            *frozen,
            "-fpch-instantiate-templates",
            "-Xclang",
            "-fvalidate-ast-input-files-content",
            "-emit-ast",
            str(source),
            "-o",
            str(ast),
        ],
        (ast,),
    )
    roots.append(
        {
            "type": "file",
            "name": str(ast),
            "external-contents": str(ast),
            "use-external-name": False,
        }
    )
    # Clang serializes the generation VFS option and reopens that exact overlay
    # while loading. Make only this known immutable recipe input available.
    roots.append(
        {
            "type": "file",
            "name": str(overlay),
            "external-contents": str(overlay),
            "use-external-name": False,
        }
    )
    # ASTUnit reapplies the serialized generation overlay on top of the load
    # VFS. Its external-content names must resolve to the same frozen buffers,
    # not fall through to arbitrary live inputs.
    for snapshot in canonical_snapshots.values():
        roots.append(
            {
                "type": "file",
                "name": str(snapshot),
                "external-contents": str(snapshot),
                "use-external-name": False,
            }
        )
    load_overlay = directory / "load-inputs.json"
    load_overlay.write_text(
        json.dumps({"version": 0, "fallthrough": False, "roots": roots})
    )
    closure = directory / "closure.json"
    result = run_bounded(
        [
            str(tools_dir / "metadata_ast_dump"),
            str(ast),
            "pgaccel_hash_count_partition_workspace_requirements_v1",
            str(load_overlay),
        ],
        directory,
        closure,
        directory / "closure.err",
        environment=environment,
        limits=Limits(seconds=max(0, deadline - time.monotonic())),
    )
    receipts.append(result)
    if result["returncode"] not in (0, 1):
        raise ExtractionError("closed AST extraction failed")
    # Decode the large compiler output inside the same watched process group,
    # not in the parent process after its resource deadline has ended.
    decoded = decode_bounded(closure, Limits())
    for item in copied.values():
        if digest(pathlib.Path(item["snapshot"])) != item["sha256"]:
            raise ExtractionError("private tool snapshot changed")
    for path, expected in inputs.items():
        if digest(pathlib.Path(path)) != expected:
            raise ExtractionError("live source drifted during closed compilation")
    return {
        "tools": copied,
        "inputs": inputs,
        "declared_search_roots": [str(root) for root in search_roots],
        "sdk_containment_anchor": str(recipe.sdk.resolve(strict=True)),
        "input_alias_targets": {
            str(path): str(path.resolve(strict=True))
            for path in sorted(paths)
            if path != path.resolve(strict=True)
        },
        "profile": profile,
        "environment": environment,
        "receipts": receipts,
        "ast_sha256": digest(ast),
        "closure_sha256": digest(closure),
        "unresolved": decoded.get("unresolved"),
        "provenance_verified": False,
        "effects_proven": False,
        "pending": [
            "verify loaded tool/library snapshot identities",
            "bounded closure decode and complete semantic edge validation",
        ],
    }


def _limited_exec(arguments: list[str]) -> None:
    """Apply a real per-file bound before exec, without threaded preexec_fn.

    RLIMIT_FSIZE is supported on macOS, unlike RLIMIT_AS in this environment.
    The parent additionally watches all registered artifacts and process-group
    RSS. The wrapper never reads executable arguments from a receipt.
    """
    import resource

    if len(arguments) < 2:
        raise ExtractionError("limited exec requires size and command")
    maximum = int(arguments[0])
    if not 0 < maximum <= 1024**3:
        raise ExtractionError("invalid file size limit")
    resource.setrlimit(resource.RLIMIT_FSIZE, (maximum, maximum))
    os.execvp(arguments[1], arguments[1:])


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--limited-exec":
        _limited_exec(sys.argv[2:])
    elif len(sys.argv) == 4 and sys.argv[1] == "--grouped-worker":
        _IN_WATCHED_WORKER = True
        request = decode_bounded(
            pathlib.Path(sys.argv[2]), Limits(output_bytes=64 * 1024)
        )
        names = {field.name for field in dataclasses.fields(GroupedRecipe)}
        if set(request) != names or any(
            not isinstance(v, str) for v in request.values()
        ):
            raise ExtractionError("invalid live compiler request")
        recipe = GroupedRecipe(
            **{name: pathlib.Path(value) for name, value in request.items()}
        )
        print(json.dumps(_compile_grouped_snapshot(recipe, pathlib.Path(sys.argv[3]))))
    else:
        raise SystemExit("internal bounded-exec helper; no imported receipt commands")
