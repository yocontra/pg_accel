"""Real filesystem regressions; no cache outside owned temporary trees is cleared."""

import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import acpp_cache  # noqa: E402


class CachePathTests(unittest.TestCase):
    def resolve(self, env, **kwargs):
        return acpp_cache.resolve_cache_dir(environ=env, platform="darwin", **kwargs)

    def test_runtime_environment_precedence_and_token_rules(self):
        cases = [
            ({"ACPP_APPDB_DIR": "/modern", "HIPSYCL_APPDB_DIR": "/legacy"}, "/modern"),
            ({"HIPSYCL_APPDB_DIR": "/legacy"}, "/legacy"),
            (
                {
                    "ACPP_APPDB_DIR": "",
                    "HIPSYCL_APPDB_DIR": "/ignored",
                    "HOME": "/home",
                },
                "/home/.acpp",
            ),
            ({"ACPP_APPDB_DIR": " \t\n", "HOME": "/home"}, "/home/.acpp"),
            ({"ACPP_APPDB_DIR": " \t/first second ", "HOME": "/home"}, "/first"),
            ({"XDG_DATA_HOME": "/xdg", "HOME": "/home"}, "/xdg/acpp"),
            ({"XDG_DATA_HOME": "", "HOME": "/home"}, "acpp"),
            ({"HOME": ""}, ".acpp"),
            ({"HOME": "/home with spaces"}, "/home with spaces/.acpp"),
            ({"ACPP_APPDB_DIR": "~/literal"}, "~/literal"),
            ({"ACPP_APPDB_DIR": "/non\u00a0breaking/path"}, "/non\u00a0breaking/path"),
        ]
        for env, base in cases:
            with self.subTest(env=env):
                self.assertEqual(
                    self.resolve(env), pathlib.Path(base) / "apps/global/jit-cache"
                )

    def test_passwd_and_cwd_fallback(self):
        with patch.object(acpp_cache, "passwd_home", return_value="/account"):
            self.assertEqual(
                self.resolve({}), pathlib.Path("/account/.acpp/apps/global/jit-cache")
            )
        with patch.object(acpp_cache, "passwd_home", return_value=None):
            self.assertEqual(
                self.resolve({}), pathlib.Path.cwd() / ".acpp/apps/global/jit-cache"
            )

    def test_explicit_tool_target_precedence_and_invalid_values(self):
        env = {"PGACCEL_ACPP_CACHE_DIR": "/tool/cache", "ACPP_APPDB_DIR": "/runtime"}
        self.assertEqual(self.resolve(env), pathlib.Path("/tool/cache"))
        self.assertEqual(
            self.resolve(env, explicit="/chosen/cache"), pathlib.Path("/chosen/cache")
        )
        self.assertEqual(
            acpp_cache.resolve_cache_dir("/linux/actual", platform="linux"),
            pathlib.Path("/linux/actual"),
        )
        for value in ("", " ", "relative", "/tmp/cache\nother", "/tmp/cache\x00bad"):
            with (
                self.subTest(value=value),
                self.assertRaises(acpp_cache.CachePathError),
            ):
                self.resolve(env, explicit=value)
            with (
                self.subTest(env_value=value),
                self.assertRaises(acpp_cache.CachePathError),
            ):
                self.resolve({"PGACCEL_ACPP_CACHE_DIR": value, "HOME": "/home"})
        with self.assertRaisesRegex(acpp_cache.CachePathError, "--cache-dir"):
            acpp_cache.resolve_cache_dir(environ={"HOME": "/home"}, platform="linux")

    def test_resolution_never_creates_files(self):
        with tempfile.TemporaryDirectory() as directory:
            base = pathlib.Path(directory) / "not-created"
            self.resolve({"ACPP_APPDB_DIR": str(base)})
            self.assertFalse(base.exists())

    def test_clear_protects_effective_tokenized_runtime_base(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            base = root / "apps/global/jit-cache"
            base.mkdir(parents=True)
            for variable in ("ACPP_APPDB_DIR", "HIPSYCL_APPDB_DIR"):
                sentinel = base / "keep"
                sentinel.write_text("runtime base")
                with self.subTest(variable=variable):
                    with self.assertRaisesRegex(acpp_cache.CachePathError, "base"):
                        acpp_cache.clear_cache(
                            base, environ={variable: f" \t{base} ignored"}
                        )
                    self.assertEqual(sentinel.read_text(), "runtime base")

    def test_clear_protects_home_directory_through_alias_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            home = root / "apps/global/jit-cache"
            home.mkdir(parents=True)
            sentinel = home / "keep"
            sentinel.write_text("home")
            alias = root / "home-alias"
            alias.symlink_to(home, target_is_directory=True)
            with self.assertRaisesRegex(acpp_cache.CachePathError, "home"):
                acpp_cache.clear_cache(home, environ={"HOME": str(alias)})
            self.assertEqual(sentinel.read_text(), "home")

    def test_clear_rejects_implicit_relative_base_in_distinct_client_cwd(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            client = root / "client"
            server = root / "server"
            for cwd in (client, server):
                cache = cwd / "relative/apps/global/jit-cache"
                cache.mkdir(parents=True)
                (cache / "keep").write_text(cwd.name)
            env = dict(os.environ, ACPP_APPDB_DIR="relative")
            env.pop("PGACCEL_ACPP_CACHE_DIR", None)
            # Exercise the Darwin CLI algorithm on Linux CI too, without a runtime.
            bootstrap = (
                "import runpy,sys; sys.platform='darwin'; "
                "sys.argv=[sys.argv[1], 'clear']; "
                "runpy.run_path(sys.argv[0], run_name='__main__')"
            )
            result = subprocess.run(
                [sys.executable, "-c", bootstrap, str(ROOT / "scripts/acpp_cache.py")],
                cwd=client,
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("explicit absolute", result.stderr)
            for cwd in (client, server):
                self.assertEqual(
                    (cwd / "relative/apps/global/jit-cache/keep").read_text(), cwd.name
                )

    def test_diagnostic_cache_section_uses_shared_resolver(self):
        # Execute the real cache section without unrelated runner/Git/GPU probes.
        source = (ROOT / "scripts/capture_metal_ci_diagnostics.sh").read_text()
        section = source.split('if ! cache_dir="$(python3 ', 1)[1]
        section = (
            'if ! cache_dir="$(python3 '
            + section.split("# Diagnostics are best-effort", 1)[0]
        )
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            cache = root / "base/apps/global/jit-cache"
            cache.mkdir(parents=True)
            (cache / "entry.metalar").write_text("keep")
            for value, label in ((str(cache), "valid"), ("", "invalid")):
                destination = root / label
                destination.mkdir()
                env = dict(
                    os.environ,
                    PGACCEL_ACPP_CACHE_DIR=value,
                    script_dir=str(ROOT / "scripts"),
                    diagnostic_dir=str(destination),
                )
                result = subprocess.run(
                    ["bash", "-c", section],
                    env=env,
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                if value:
                    self.assertIn(
                        "entry.metalar",
                        (destination / "jit-cache-files.txt").read_text(),
                    )
                else:
                    self.assertIn(
                        "not an empty-cache observation",
                        (destination / "jit-cache.missing.txt").read_text(),
                    )
                    self.assertIn(
                        "explicit cache directory",
                        (destination / "jit-cache-resolution-error.txt").read_text(),
                    )
                    self.assertFalse((destination / "jit-cache-files.txt").exists())
                self.assertEqual((cache / "entry.metalar").read_text(), "keep")

    def test_clear_preserves_decoys_and_never_traverses_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            cache = root / "base/apps/global/jit-cache"
            cache.mkdir(parents=True)
            victim = root / "sibling"
            victim.write_text("keep")
            (cache / "file.metalar").write_text("cache")
            (cache / "link").symlink_to(victim)
            with self.assertRaises(acpp_cache.CachePathError):
                acpp_cache.clear_cache(cache, environ={"HOME": str(root / "home")})
            self.assertTrue((cache / "file.metalar").exists())
            (cache / "link").unlink()
            nested = cache / "unexpected-directory"
            nested.mkdir()
            with self.assertRaises(acpp_cache.CachePathError):
                acpp_cache.clear_cache(cache, environ={})
            self.assertTrue((cache / "file.metalar").exists())
            nested.rmdir()
            self.assertEqual(acpp_cache.clear_cache(cache, environ={}), 1)
            self.assertEqual(victim.read_text(), "keep")
            self.assertTrue(cache.is_dir())
            self.assertEqual(
                acpp_cache.clear_cache(
                    root / "missing/apps/global/jit-cache", environ={}
                ),
                0,
            )

    def test_clear_refuses_unsafe_targets_before_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            sentinel = root / "keep"
            sentinel.write_text("keep")
            for target in (
                pathlib.Path("/"),
                root,
                root / "apps",
                root / "apps/global",
                pathlib.Path("relative/jit-cache"),
                root / "base/apps/global/../global/jit-cache",
            ):
                with (
                    self.subTest(target=target),
                    self.assertRaises(acpp_cache.CachePathError),
                ):
                    acpp_cache.clear_cache(target, environ={"HOME": str(root)})
                self.assertTrue(sentinel.exists())
            actual = root / "base/apps/global/jit-cache"
            actual.mkdir(parents=True)
            alias = root / "alias"
            alias.symlink_to(root / "base", target_is_directory=True)
            with self.assertRaises(acpp_cache.CachePathError):
                acpp_cache.clear_cache(alias / "apps/global/jit-cache", environ={})
            final_alias = root / "apps/global/jit-cache"
            final_alias.parent.mkdir(parents=True)
            final_alias.symlink_to(actual, target_is_directory=True)
            with self.assertRaises(acpp_cache.CachePathError):
                acpp_cache.clear_cache(final_alias, environ={})

    def test_target_swap_after_preflight_cannot_delete_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            cache = root / "apps/global/jit-cache"
            cache.mkdir(parents=True)
            (cache / "old.jit").write_text("old")
            saved = cache.with_name("saved-cache")
            original_listdir = os.listdir

            def swapped_listdir(descriptor):
                names = original_listdir(descriptor)
                cache.rename(saved)
                cache.mkdir()
                (cache / "replacement.jit").write_text("keep")
                return names

            with patch.object(acpp_cache.os, "listdir", side_effect=swapped_listdir):
                with self.assertRaisesRegex(
                    acpp_cache.CachePathError, "target changed"
                ):
                    acpp_cache.clear_cache(cache, environ={})
            self.assertEqual((saved / "old.jit").read_text(), "old")
            self.assertEqual((cache / "replacement.jit").read_text(), "keep")

    def test_explicit_target_reaches_all_nested_cold_recipes(self):
        # Invalid override must fail at canonical clear before any test/GPU command.
        with tempfile.TemporaryDirectory():
            env = dict(os.environ, PGACCEL_ACPP_CACHE_DIR="")
            for command in (
                ["gpu-test-cold", "not-a-real-test"],
                ["gpu-test-cold-all"],
                ["gpu-stress-archive", "1", "1"],
            ):
                with self.subTest(command=command):
                    result = subprocess.run(
                        ["just", *command],
                        cwd=ROOT,
                        env=env,
                        capture_output=True,
                        text=True,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("explicit cache directory", result.stderr)
                    self.assertNotIn("No such file", result.stderr)

    @unittest.skipUnless(
        os.environ.get("PGACCEL_ARCHIVE_PATH_TEST"),
        "set host-only native path probe for cross-language parity",
    )
    def test_native_fresh_process_parity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            cases = [
                {"ACPP_APPDB_DIR": str(root / "base")},
                {"HIPSYCL_APPDB_DIR": str(root / "legacy")},
                {
                    "ACPP_APPDB_DIR": "",
                    "HIPSYCL_APPDB_DIR": str(root / "ignored"),
                    "HOME": str(root / "home"),
                },
                {"ACPP_APPDB_DIR": "  \t", "HOME": str(root / "home")},
                {"ACPP_APPDB_DIR": str(root / "first") + " ignored"},
                {"XDG_DATA_HOME": str(root / "xdg"), "HOME": str(root / "home")},
                {"XDG_DATA_HOME": "", "HOME": str(root / "ignored")},
                {"HOME": ""},
                {"HOME": str(root / "home with spaces")},
                {"ACPP_APPDB_DIR": "~/literal"},
                {"ACPP_APPDB_DIR": str(root / "non\u00a0breaking/path")},
                {},
            ]
            probe = os.environ["PGACCEL_ARCHIVE_PATH_TEST"]
            for values in cases:
                env = {
                    key: value
                    for key, value in os.environ.items()
                    if key
                    not in {
                        "ACPP_APPDB_DIR",
                        "HIPSYCL_APPDB_DIR",
                        "PGACCEL_ACPP_CACHE_DIR",
                        "HOME",
                        "XDG_DATA_HOME",
                    }
                }
                env.update(values)
                with self.subTest(values=values):
                    before = list(root.iterdir())
                    result = subprocess.run(
                        [probe, "--path"],
                        cwd=root,
                        env=env,
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    native = pathlib.Path(result.stdout.rstrip("\n"))
                    expected = self.resolve(values)
                    if sys.platform == "darwin":
                        self.assertEqual(native, expected)
                    else:
                        # This checks native per-app structure, not a Python Linux resolver.
                        self.assertEqual(native.parent.parent, expected.parent.parent)
                        self.assertTrue(
                            native.parent.name.startswith(
                                pathlib.Path(probe).name + "-"
                            )
                        )
                    self.assertEqual(list(root.iterdir()), before)

    def test_cli_explicit_path_and_recipe_environment_propagation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory).resolve()
            cache = root / "apps/global/jit-cache"
            cache.mkdir(parents=True)
            (cache / "entry.jit").write_text("cache")
            env = dict(os.environ, PGACCEL_ACPP_CACHE_DIR=str(cache))
            result = subprocess.run(
                [sys.executable, str(ROOT / "scripts/acpp_cache.py"), "resolve"],
                env=env,
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), str(cache))
            # Exercise the canonical recipe, not a reconstruction of its body.
            result = subprocess.run(
                ["just", "clear-jit"], cwd=ROOT, env=env, capture_output=True, text=True
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((cache / "entry.jit").exists())
            (cache / "entry.jit").write_text("cache")
            env["PGACCEL_ACPP_CACHE_DIR"] = ""
            result = subprocess.run(
                ["just", "clear-jit"], cwd=ROOT, env=env, capture_output=True, text=True
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue((cache / "entry.jit").exists())


if __name__ == "__main__":
    unittest.main()
