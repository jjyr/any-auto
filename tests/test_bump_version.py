"""Exercise release preparation in disposable Git repositories."""

from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent


class BumpVersionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        (self.repo / "scripts").mkdir()
        (self.repo / "src").mkdir()
        (self.repo / "src/main.rs").write_text("fn main() {}\n")
        for filename in ["Cargo.toml", "Cargo.lock", "scripts/bump-version.py"]:
            shutil.copyfile(ROOT / filename, self.repo / filename)
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Release Test")
        self.git("config", "user.email", "release@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "tag.gpgsign", "false")
        self.git("config", "core.hooksPath", "/dev/null")
        self.git("add", ".")
        self.git("commit", "-m", "Initial")
        self.initial = self.git("rev-parse", "HEAD")
        self.git("checkout", "-b", "release-prep")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, text=True,
                                       stderr=subprocess.DEVNULL).strip()

    def bump(self, *args):
        return subprocess.run([sys.executable, str(self.repo / "scripts/bump-version.py"), *args],
                              cwd=self.temp.name, text=True, capture_output=True)

    def assert_version(self, expected):
        for filename in ["Cargo.toml", "Cargo.lock"]:
            text = (self.repo / filename).read_text()
            version = re.search(r'name = "any-auto"\nversion = "([^"]+)"', text).group(1)
            self.assertEqual(version, expected)
        self.assertEqual(self.git("tag", "--list"), "")

    def test_bump_commit_without_tag(self):
        before_manifest = (self.repo / "Cargo.toml").read_text()
        current = re.search(r'\nversion = "([^"]+)"', before_manifest).group(1)
        major, minor, _ = map(int, current.split("."))
        expected = f"{major}.{minor + 1}.0"
        result = self.bump("minor")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(expected)
        self.assertEqual(self.git("branch", "--show-current"), "release-prep")
        self.assertEqual(self.git("rev-parse", "main"), self.initial)
        self.assertEqual(self.git("rev-parse", "HEAD^"), self.initial)
        self.assertEqual(self.git("status", "--porcelain"), "")
        self.assertEqual(self.git("diff", "--name-only", "HEAD^", "HEAD").splitlines(),
                         ["Cargo.lock", "Cargo.toml"])
        for filename in ["Cargo.toml", "Cargo.lock"]:
            before = self.git("show", f"HEAD^:{filename}")
            after = (self.repo / filename).read_text().strip()
            old = re.search(r'name = "any-auto"\nversion = "([^"]+)"', before).group(1)
            self.assertEqual(after, before.replace(f'version = "{old}"',
                                                   f'version = "{expected}"', 1))
        self.assertIn("open a PR into main", result.stdout)
        self.assertIn("After the PR is merged and CI passes", result.stdout)

    def test_explicit_version(self):
        result = self.bump("v99.0.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version("99.0.0")

    def test_patch_and_major(self):
        self.assertEqual(self.bump("99.1.2").returncode, 0)
        for mode, expected in [("patch", "99.1.3"), ("major", "100.0.0")]:
            result = self.bump(mode)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assert_version(expected)

    def test_no_argument_defaults_to_patch(self):
        self.assertEqual(self.bump("99.1.2").returncode, 0)
        result = self.bump()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version("99.1.3")

    def test_invalid_or_lower_versions_leave_tree_unchanged(self):
        for version in ["0.0.0", "01.2.3", "1.2.3-beta", "nonsense"]:
            with self.subTest(version=version):
                self.assertNotEqual(self.bump(version).returncode, 0)
                self.assertEqual(self.git("status", "--porcelain"), "")
                self.assertEqual(self.git("rev-parse", "HEAD"), self.initial)

    def test_existing_tag_leaves_tree_unchanged(self):
        self.git("tag", "v99.0.0")
        self.assertNotEqual(self.bump("99.0.0").returncode, 0)
        self.assertEqual(self.git("status", "--porcelain"), "")

    def test_dirty_tree_and_detached_head_are_rejected(self):
        extra = self.repo / "untracked.txt"
        extra.write_text("keep me")
        self.assertNotEqual(self.bump("patch").returncode, 0)
        self.assertEqual(extra.read_text(), "keep me")
        extra.unlink()
        self.git("checkout", "--detach")
        self.assertNotEqual(self.bump("patch").returncode, 0)
        self.assertEqual(self.git("status", "--porcelain"), "")
