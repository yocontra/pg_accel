"""Opt-in real compiler regressions for the isolated SSCP replay patch.

Set ACPP_HCF_REPLAY_PREFIX, ACPP_HCF_REPLAY_ORIGINAL_PREFIX, ACPP_HCF_REPLAY_LLVM
and ACPP_HCF_REPLAY_ARTIFACTS (a new directory) to run. This is compiler/artifact
qualification, not a projection proof or runtime test. No objects are linked or
loaded. Identical IDs are permitted only for separately retained replay files.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import platform
import re
import struct
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "scripts/tests/fixtures/acpp_hcf_replay.cpp"
OPTION = "-acpp-sscp-hcf-object-id="
APPENDIX = b"__acpp_hcf_binary_appendix"


def hcf(data: bytes) -> tuple[int, bytes, int]:
    """Extract the single native embedded HCF, with bounded appendix slicing."""
    matches = list(re.finditer(rb"object-id=([0-9]+)\n", data))
    if len(matches) != 1:
        raise ValueError("expected exactly one embedded HCF object ID")
    match = matches[0]
    marker = data.index(APPENDIX, match.end())
    sizes = re.findall(rb"\nsize=([0-9]+)\n", data[match.start():marker])
    if len(sizes) != 1:
        raise ValueError("expected one device image")
    start = marker + len(APPENDIX)
    end = start + int(sizes[0])
    if end > len(data):
        raise ValueError("invalid or truncated LLVM bitcode appendix")
    payload = data[start:end]
    bitcode = payload
    if payload.startswith(b"\xde\xc0\x17\x0b"):
        if len(payload) < 20:
            raise ValueError("truncated LLVM bitcode wrapper")
        _, version, offset, length, _ = struct.unpack_from("<5I", payload)
        if version != 0 or offset < 20 or offset + length > len(payload):
            raise ValueError("invalid LLVM bitcode wrapper bounds")
        bitcode = payload[offset:offset + length]
    if not bitcode.startswith(b"BC\xc0\xde"):
        raise ValueError("invalid LLVM bitcode magic")
    return int(match[1]), payload, end - match.start()


def immediate(text: str, register: int) -> int:
    """Decode a narrow straight-line AArch64 window, rejecting unknown effects."""
    value = None
    pattern = rf"(mov|movk)\s+([wx]){register},\s+#(-?0x[0-9a-f]+)(?:,\s*lsl #(\d+))?"
    for line in text.splitlines():
        if not line.strip():
            continue
        instruction = re.fullmatch(r"\s*(?:[0-9a-f]+:\s*)?([a-z][a-z0-9.]*)\s+(.+?)\s*", line)
        if instruction is None:
            raise ValueError("unsupported disassembly line")
        opcode, operands = instruction.groups()
        match = re.fullmatch(pattern, opcode + " " + operands)
        if match is None:
            # These are the only unrelated operations in the qualified native
            # windows. A store's writeback may update SP, never the decoded GPR.
            if opcode == "stp" and re.fullmatch(r"x\d+,\s*x\d+,\s*\[sp(?:,\s*#-?0x[0-9a-f]+)?\]!?", operands):
                continue
            other = re.fullmatch(r"([wx]\d+|sp),\s*(.+)", operands)
            if (opcode in {"mov", "movk", "adrp", "add", "sub"} and other
                    and other[1] not in {f"w{register}", f"x{register}"}):
                continue
            raise ValueError("unsupported register write or control flow")
        op, width, literal, shift = match.groups()
        bits = 64 if width == "x" else 32
        shift = int(shift or 0)
        literal = int(literal, 16)
        if op == "mov":
            if shift:
                raise ValueError("unsupported shifted MOV")
            value = literal & ((1 << bits) - 1)
        elif value is None:
            raise ValueError("MOVK without preceding literal MOV")
        else:
            if shift >= bits or shift % 16 or not 0 <= literal <= 65535:
                raise ValueError("invalid MOVK immediate")
            value = (value & ~(65535 << shift)) | (literal << shift)
            value &= (1 << bits) - 1
    if value is None:
        raise ValueError("no supported literal sequence")
    return value


class ReplayInspectionTests(unittest.TestCase):
    def test_immediate_preserves_high_bits_and_unsigned_max(self):
        self.assertEqual(immediate("mov x0, #0x1\nmovk x0, #0x8000, lsl #48", 0), (1 << 63) + 1)
        self.assertEqual(immediate("mov x0, #-0x1", 0), (1 << 64) - 1)
        with self.assertRaises(ValueError):
            immediate("movk x0, #0x1, lsl #16", 0)

    def test_immediate_rejects_unmodeled_writes_and_control_flow(self):
        for suffix in ("add x0, x0, #0x1", "ldr x0, [x1]", "mov w0, w1",
                       "ldp x1, x0, [sp]", "stp x1, x2, [x0, #0x10]!",
                       "bl _helper", "b 0x20", "b.eq 0x20", "cbz x1, 0x20",
                       "ret", ".long 0x12345678", "unknown x1, x2"):
            with self.subTest(suffix=suffix), self.assertRaises(ValueError):
                immediate("mov x0, #0x1\n" + suffix, 0)

    def test_hcf_rejects_missing_duplicate_and_truncated_payload(self):
        payload = b"object-id=1\nsize=4\n" + APPENDIX + b"BC\xc0\xde"
        self.assertEqual(hcf(payload), (1, b"BC\xc0\xde", len(payload)))
        for bad in (b"", payload + payload, payload[:-1]):
            with self.assertRaises(ValueError):
                hcf(bad)

    def test_hcf_accepts_bounded_wrapper_without_removing_bytes(self):
        bitcode = b"BC\xc0\xde"
        wrapper = struct.pack("<5I", 0x0B17C0DE, 0, 20, len(bitcode), 0xFFFFFFFF) + bitcode
        prefix = b"object-id=1\nsize=24\n" + APPENDIX
        self.assertEqual(hcf(prefix + wrapper)[1], wrapper)
        for version, offset, length in ((1, 20, 4), (0, 19, 4), (0, 21, 4), (0, 20, 5), (0, 20, 3)):
            invalid = struct.pack("<5I", 0x0B17C0DE, version, offset, length, 0) + bitcode
            with self.subTest(version=version, offset=offset, length=length), self.assertRaises(ValueError):
                hcf(prefix + invalid)


@unittest.skipUnless(os.environ.get("ACPP_HCF_REPLAY_PREFIX"), "requires explicit isolated patched compiler prefix")
class AdaptiveCppHcfReplayTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if platform.system() != "Darwin" or platform.machine() != "arm64":
            raise RuntimeError("native registration inspection currently qualifies Darwin arm64 only")
        cls.prefix = Path(os.environ["ACPP_HCF_REPLAY_PREFIX"]).resolve(strict=True)
        cls.original = Path(os.environ["ACPP_HCF_REPLAY_ORIGINAL_PREFIX"]).resolve(strict=True)
        cls.llvm = Path(os.environ["ACPP_HCF_REPLAY_LLVM"]).resolve(strict=True)
        cls.artifacts = Path(os.environ["ACPP_HCF_REPLAY_ARTIFACTS"])
        if not cls.artifacts.is_absolute() or cls.prefix == cls.original:
            raise RuntimeError("use an absolute new artifact directory and distinct owned prefix")
        cls.artifacts.mkdir(parents=True, exist_ok=False)
        cls.environment = dict(os.environ, ACPP_APPDB_DIR=str(cls.artifacts / "unused-runtime-base"))
        cls.sdk = os.environ["SDKROOT"]

    def compile(self, name, *, source=FIXTURE, ids=(), opt="O3", shared=False,
                original=False, width_only=False):
        directory = self.artifacts / name
        directory.mkdir(exist_ok=True)
        prefix = self.original if original else self.prefix
        args = [str(prefix / "lib/cmake/AdaptiveCpp/syclcc-launcher"),
                "--launcher-cxx-compiler=/usr/bin/c++",
                f"--launcher-syclcc={prefix / 'bin/acpp'}", "/usr/bin/c++",
                f"-I{ROOT / 'pgaccel-kernels/include'}", "-isystem", str(prefix / "include"),
                "-isystem", str(prefix / "include/AdaptiveCpp"), f"-{opt}", "-std=gnu++17",
                "-fPIC", "-ffast-math"]
        if opt == "O3":
            args.append("-DNDEBUG")
        if shared:
            args += ["-DPGACCEL_TEST_HOOKS", "-Dpgaccel_kernels_shared_EXPORTS"]
        if width_only:
            args += ["--target=i386-unknown-linux-gnu", "-DPGACCEL_REPLAY_WIDTH_ONLY"]
        else:
            args += ["-arch", "arm64", "-isysroot", self.sdk]
        for value in ids:
            args += ["-mllvm", OPTION + value]
        args += ["-MD", "-MT", "object.o", "-MF", str(directory / "object.d"),
                 "-o", str(directory / "object.o"), "-c", str(source)]
        result = subprocess.run(args, cwd=directory, env=self.environment, capture_output=True)
        index = len(list(directory.glob("command-*.json")))
        stem = directory / f"command-{index}"
        stem.with_suffix(".log").write_bytes(result.stdout + result.stderr)
        record = {"argv": args, "cwd": str(directory), "exit": result.returncode}
        artifact = directory / "object.o"
        if result.returncode == 0:
            record["sha256"] = hashlib.sha256(artifact.read_bytes()).hexdigest()
            record["size"] = artifact.stat().st_size
        stem.with_suffix(".json").write_text(json.dumps(record, indent=2) + "\n")
        return result, artifact

    def success(self, *args, **kwargs):
        result, artifact = self.compile(*args, **kwargs)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace")[-8000:])
        return artifact

    def inspect(self, artifact):
        data = artifact.read_bytes()
        object_id, payload, size = hcf(data)
        bitcode = artifact.with_suffix(".device.bc")
        bitcode.write_bytes(payload)
        subprocess.run([str(self.llvm / "bin/llvm-dis"), str(bitcode), "-o", str(bitcode.with_suffix(".ll"))], check=True, capture_output=True)
        result = subprocess.run([str(self.llvm / "bin/llvm-objdump"), "--macho", "--disassemble",
                                 "--no-show-raw-insn", "--section=__StaticInit", str(artifact)],
                                check=True, capture_output=True, text=True)
        artifact.with_suffix(".disassembly.txt").write_text(result.stdout)
        destructor = re.search(r"^__ZN7hipsycl4glue4sscp23static_hcf_registrationD1Ev:\n(.*?)^\s*[0-9a-f]+:\s*(?:bl|b)\s+___acpp_unregister_hcf", result.stdout, re.M | re.S)
        self.assertIsNotNone(destructor, "missing native unregister call")
        self.assertEqual(immediate(destructor[1], 0), object_id)
        registration = re.search(r"(adrp\s+x0, ___acpp_local_sscp_hcf_content.initialized@PAGE\n.*?)^\s*[0-9a-f]+:\s*(?:bl|b)\s+___acpp_register_hcf", result.stdout, re.M | re.S)
        self.assertIsNotNone(registration, "missing native HCF buffer registration")
        self.assertIn("___acpp_local_sscp_hcf_content.initialized@PAGEOFF", registration[1])
        self.assertEqual(immediate(registration[1], 1), size)
        return object_id, payload

    def test_01_option_rejections_and_unpatched_fail_closed(self):
        invalid = ("", "0", "00", "01", "+1", "-1", " 1", "1 ", "1\t", "1\n", "0x1", "1x",
                   "18446744073709551616", "99999999999999999999999999999999999", "١")
        for index, value in enumerate(invalid):
            with self.subTest(value=value):
                result, artifact = self.compile(f"invalid-{index}", ids=(value,))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(b"canonical decimal ID", result.stderr)
                self.assertFalse(artifact.exists())
        for ids in (("1", "1"), ("1", "2")):
            with self.subTest(duplicate=ids):
                result, artifact = self.compile("duplicate-" + ids[1], ids=ids)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(b"may only occur once", result.stderr)
                self.assertFalse(artifact.exists())
        result, artifact = self.compile("unpatched-option", ids=("1",), original=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"Unknown command line argument", result.stderr)
        self.assertFalse(artifact.exists())

    def test_02_fixture_replay_names_and_id_payload_coherence(self):
        payloads = []
        for index, value in enumerate(("1", "18446744073709551615", "9223372036854775808")):
            artifact = self.success(f"fixture-id-{index}", ids=(value,))
            first = artifact.read_bytes()
            artifact.with_name("actual.retained.o").write_bytes(first)
            self.success(f"fixture-id-{index}", ids=(value,))
            self.assertEqual(artifact.read_bytes(), first, "complete object replay differs")
            found_id, payload = self.inspect(artifact)
            self.assertEqual(found_id, int(value))
            payloads.append(payload)
            for number in range(20):
                self.assertEqual(first.count(f"replay_kernel_{number:02d}".encode() + b"\0"), 1)
            symbols = subprocess.check_output([str(self.llvm / "bin/llvm-nm"), str(artifact)], text=True)
            for number in range(20):
                self.assertEqual(symbols.count(f"_replay_name_{number:02d}.initialized\n"), 1)
            self.assertNotIn("__acpp_sscp_extract_kernel_name_", symbols)
        self.assertTrue(all(payload == payloads[0] for payload in payloads))
        ordinary_ids = []
        for original in (False, True):
            for number in range(2):
                artifact = self.success(f"default-{original}-{number}", original=original)
                object_id, payload = self.inspect(artifact)
                ordinary_ids.append(object_id)
                self.assertEqual(payload, payloads[0], "patch changed device computation")
        self.assertEqual(len(set(ordinary_ids)), 4, "ordinary random ID generation unexpectedly repeats")

    def test_03_actual_join_four_cell_exact_replay(self):
        ids = iter((5100000000000000001, 6100000000000000001, 7100000000000000001, 8100000000000000001))
        for shared in (True, False):
            for opt in ("O0", "O3"):
                with self.subTest(shared=shared, opt=opt):
                    value = str(next(ids))
                    name = f"join-{shared}-{opt}"
                    kwargs = dict(source=ROOT / "pgaccel-kernels/src/join_rows.cpp", ids=(value,), shared=shared, opt=opt)
                    artifact = self.success(name, **kwargs)
                    first = artifact.read_bytes()
                    artifact.with_name("actual.retained.o").write_bytes(first)
                    self.success(name, **kwargs)
                    self.assertEqual(artifact.read_bytes(), first, "complete object replay differs")
                    self.assertEqual(hcf(first)[0], int(value))
                    if opt == "O3":
                        self.inspect(artifact)

    def test_04_32bit_target_default_compiles_explicit_id_rejects(self):
        self.success("target32-default", width_only=True)
        result, artifact = self.compile("target32-explicit", width_only=True, ids=("1",))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"explicit HCF IDs require a known 64-bit host-target registration ABI", result.stderr)
        self.assertFalse(artifact.exists())


if __name__ == "__main__":
    unittest.main()
