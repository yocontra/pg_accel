import pathlib
import re
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
BARE_EXTERN_C_BLOCK = re.compile(r'^\s*(?:pub\s+)?extern\s+"C"\s*\{', re.MULTILINE)


class TargetSpecificRustTests(unittest.TestCase):
    def test_rust_2024_ffi_blocks_are_explicitly_unsafe(self) -> None:
        violations: list[str] = []
        for crate in ("pg_accel", "pg_accel_bench"):
            for source_path in sorted((REPO_ROOT / crate).glob("**/*.rs")):
                source = source_path.read_text(encoding="utf-8")
                for match in BARE_EXTERN_C_BLOCK.finditer(source):
                    line = source.count("\n", 0, match.start()) + 1
                    violations.append(f"{source_path.relative_to(REPO_ROOT)}:{line}")

        self.assertEqual(
            violations,
            [],
            "Rust 2024 requires foreign blocks to use `unsafe extern`; "
            "bare blocks may be hidden locally by target-specific cfgs",
        )


if __name__ == "__main__":
    unittest.main()
