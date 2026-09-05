import pathlib
import re
import unittest


REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
BARE_EXTERN_C_BLOCK = re.compile(r'^\s*(?:pub\s+)?extern\s+"C"\s*\{', re.MULTILINE)
SIGNED_RELKIND_CAST = re.compile(r"pg_sys::RELKIND_[A-Z0-9_]+\s+as\s+i8")


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

    def test_postgres_relkind_comparisons_are_char_signedness_neutral(self) -> None:
        violations: list[str] = []
        for source_path in sorted((REPO_ROOT / "pg_accel").glob("**/*.rs")):
            source = source_path.read_text(encoding="utf-8")
            for match in SIGNED_RELKIND_CAST.finditer(source):
                line = source.count("\n", 0, match.start()) + 1
                violations.append(f"{source_path.relative_to(REPO_ROOT)}:{line}")

        self.assertEqual(
            violations,
            [],
            "PostgreSQL `char` is u8 on targets such as Linux arm64; compare "
            "relkind values through u8 instead of forcing constants to i8",
        )


if __name__ == "__main__":
    unittest.main()
