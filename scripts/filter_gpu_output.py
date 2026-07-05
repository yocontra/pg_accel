#!/usr/bin/env python3
"""Run a GPU command with known warning noise folded into a short summary."""

from __future__ import annotations

import argparse
import collections
import datetime
import os
import re
import subprocess
import sys
from typing import Counter


ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
METAL_UNUSED_RE = re.compile(
    r".*/\.acpp/apps/global/jit-cache/.*\.metal:\d+:\d+: warning: unused variable "
)
SDK_VERSION_RE = re.compile(
    r"warning: linking module flags 'SDK Version': IDs have conflicting values"
)
COMPILER_WARNING_SUMMARY_RE = re.compile(r"^\d+ warnings generated\.$")
IMPORTANT_RE = re.compile(
    r"(Results:|RESULT:|FAIL|FAILED|FATAL|ERROR|Error|error:|panic|assert|"
    r"xpc_compiler_service_hits|pipeline_state_failures|archive_build_failures)"
)
LOG_LABEL_SAFE_RE = re.compile(r"[^A-Za-z0-9_.-]+")


def plain(line: str) -> str:
    return ANSI_RE.sub("", line).rstrip("\n")


def classify_noise(line: str, state: dict[str, bool]) -> str | None:
    text = plain(line)
    stripped = text.strip()

    if state.get("after_metal_unused"):
        if stripped.startswith("constexpr constant ") or stripped == "^":
            if stripped == "^":
                state["after_metal_unused"] = False
            return "metal-unused-detail"
        state["after_metal_unused"] = False

    if METAL_UNUSED_RE.search(text):
        state["after_metal_unused"] = True
        return "metal-unused"
    if SDK_VERSION_RE.search(text):
        return "sdk-version"
    if COMPILER_WARNING_SUMMARY_RE.match(stripped):
        return "compiler-warning-summary"
    if "[AdaptiveCpp Warning] kernel_cache:" in text:
        return "kernel-cache"
    if "metal_hardware_manager: MTLCopyAllDevices returned no devices" in text:
        return "metal-no-devices"
    if text == "pgaccel: FATAL: no SYCL GPU device found":
        return "no-sycl-device"
    if text.startswith("acpp-metal-archive-build: wrote "):
        return "archive-write"
    if "Context leak detected, CoreAnalytics returned false" in text:
        return "coreanalytics"
    return None


def emit_suppression_summary(label: str, log_path: str, counts: Counter[str]) -> None:
    if not counts:
        return
    parts = [f"{name}={counts[name]}" for name in sorted(counts)]
    print(
        f"gpu-noise[{label}]: suppressed {', '.join(parts)}; raw log: {log_path}",
        flush=True,
    )


def emit_failure_summary(label: str, log_path: str, returncode: int) -> None:
    print(f"gpu-test[{label}]: failed with exit code {returncode}", flush=True)
    print(f"--- failure summary from {log_path} ---", flush=True)

    matches: collections.OrderedDict[str, int] = collections.OrderedDict()
    try:
        with open(log_path, "r", encoding="utf-8", errors="replace") as log_file:
            for raw_line in log_file:
                text = plain(raw_line)
                if IMPORTANT_RE.search(text):
                    matches[text] = matches.get(text, 0) + 1
    except OSError as exc:
        print(f"could not read log: {exc}", flush=True)
        return

    if not matches:
        print("(no failure summary lines found)", flush=True)
        return

    for line, count in list(matches.items())[-80:]:
        if count > 1:
            print(f"{line} (repeated {count}x)", flush=True)
        else:
            print(line, flush=True)


def default_log_path(label: str, log_dir: str) -> str:
    safe_label = LOG_LABEL_SAFE_RE.sub("_", label).strip("._-") or "gpu-command"
    timestamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    return os.path.join(log_dir, f"{safe_label}-{timestamp}-{os.getpid()}.log")


def run(args: argparse.Namespace) -> int:
    log_path = args.log or default_log_path(args.label, args.log_dir)
    os.makedirs(os.path.dirname(log_path) or ".", exist_ok=True)
    state: dict[str, bool] = {}
    counts: Counter[str] = collections.Counter()

    with open(log_path, "w", encoding="utf-8", errors="replace") as log_file:
        process = subprocess.Popen(
            args.command,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
        )
        assert process.stdout is not None
        for line in process.stdout:
            log_file.write(line)
            category = classify_noise(line, state)
            if category:
                counts[category] += 1
                continue
            print(line, end="", flush=True)

        returncode = process.wait()

    emit_suppression_summary(args.label, log_path, counts)
    if returncode != 0:
        emit_failure_summary(args.label, log_path, returncode)
    return returncode


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--label", required=True, help="short command label for summaries")
    log_target = parser.add_mutually_exclusive_group(required=True)
    log_target.add_argument("--log", help="raw combined stdout/stderr log path")
    log_target.add_argument(
        "--log-dir",
        help="directory for a generated timestamped raw combined stdout/stderr log",
    )
    parser.add_argument("command", nargs=argparse.REMAINDER, help="command after --")
    args = parser.parse_args()
    if args.command and args.command[0] == "--":
        args.command = args.command[1:]
    if not args.command:
        parser.error("missing command after --")
    return args


if __name__ == "__main__":
    raise SystemExit(run(parse_args()))
