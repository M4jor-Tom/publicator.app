"""Exercise the quality gates with real analyzers and disposable Rust fixtures."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
METRICS = ROOT / "scripts/check-metrics.sh"
# Enough tokens to be scanned, but no branches or long repeated token sequences.
CLEAN = "pub fn values() -> [u32; 100] { [\n" + ",\n".join(map(str, range(100))) + "\n] }\n"


class QualityMetricsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "clean.rs").write_text(CLEAN)

    def scan(self, *paths, env=None):
        return subprocess.run(
            ["bash", str(METRICS), *map(str, paths or [self.root])],
            cwd=self.root,
            env=env,
            text=True,
            capture_output=True,
            timeout=30,
        )

    def test_clean_code_passes(self):
        result = self.scan()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_limits_are_per_function_not_file_totals(self):
        # Five nested ifs score 15 (1+2+3+4+5); nine sibling ifs score 10 cyclomatic.
        functions = "\n".join(
            f"fn nested_{n}(b: bool) {{ " + "if b { " * 5 + "}" * 6
            for n in range(2)
        )
        functions += "\nfn branches(b: bool) { " + "if b {} " * 9 + "}\n"
        (self.root / "limits.rs").write_text(functions)
        result = self.scan()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_excessive_cognitive_complexity_fails(self):
        (self.root / "complex.rs").write_text(
            "fn nested(b: bool) { " + "if b { " * 6 + "}" * 7
        )
        result = self.scan()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cognitive=21", result.stdout)
        self.assertIn("complex.rs:1", result.stdout)

    def test_excessive_cyclomatic_complexity_fails(self):
        (self.root / "branches.rs").write_text(
            "fn branches(b: bool) { " + "if b {} " * 10 + "}\n"
        )
        result = self.scan()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cyclomatic=11", result.stdout)

    def test_nested_closure_is_checked_without_aggregating_parent(self):
        (self.root / "closure.rs").write_text(
            "fn parent(b: bool) { let _f = || { " + "if b { " * 6 + "}" * 7 + "; }"
        )
        result = self.scan()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("closure.rs:1", result.stdout)
        self.assertNotIn("parent: cognitive=", result.stdout)

    def test_duplication_across_source_test_and_example_paths_fails(self):
        roots = [self.root / name for name in ("src", "tests", "examples")]
        duplicate = "pub fn repeated(mut x: u32) -> u32 {\n"
        duplicate += "".join(f"    x = x.wrapping_mul({n}).wrapping_add({n + 1});\n" for n in range(20))
        duplicate += "    x\n}\n"
        for folder in roots:
            folder.mkdir()
            (folder / "copy.rs").write_text(duplicate)
        result = self.scan(*roots)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("copy.rs", result.stdout)

    def test_duplication_requires_both_token_and_line_thresholds(self):
        # 5 header + 18*5 let + 4 return + 1 closing tokens = 100, on 10 lines.
        statements = [f"let x{n} = {n};" for n in range(18)]
        boundary = "fn boundary() {\n"
        boundary += "\n".join(" ".join(statements[n:n + 3]) for n in range(0, 18, 3))
        boundary += "\n\nreturn ();\n}\n"
        for source, fails in ((boundary, True),
                              (boundary.replace("return ();", "return;"), False),
                              (boundary.replace("\n\n", "\n"), False)):
            with self.subTest(source=source):
                for name in ("a.rs", "b.rs"):
                    (self.root / name).write_text(source)
                result = self.scan()
                self.assertEqual(result.returncode != 0, fails, result.stdout + result.stderr)

    def test_missing_or_empty_scan_fails(self):
        empty = self.root / "empty"
        empty.mkdir()
        for path in (empty, self.root / "missing"):
            with self.subTest(path=path):
                self.assertNotEqual(self.scan(path).returncode, 0)

    def test_failed_empty_or_malformed_analyzer_output_fails(self):
        # Only fake the external process to exercise failures it cannot reliably produce on demand.
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        analyzer = bin_dir / "rust-code-analysis-cli"
        env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}")
        for body in ("exit 7", "exit 0", "echo '{}'", "echo '{bad json'"):
            with self.subTest(body=body):
                analyzer.write_text("#!/bin/sh\n" + body + "\n")
                analyzer.chmod(0o755)
                self.assertNotEqual(self.scan(env=env).returncode, 0)


HOOKS = ROOT / ".githooks"


class GitHookTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.repo = root / "repo"
        self.repo.mkdir()
        local_vars = subprocess.check_output(["git", "rev-parse", "--local-env-vars"], text=True).splitlines()
        self.env = {key: value for key, value in os.environ.items() if key not in local_vars}
        self.git("init", "-q", "-b", "main")
        self.head = self.commit("committed")
        self.gates = self.repo / ".git/quality-gate"
        self.calls = root / "calls"
        self.slow = root / "slow"
        bin_dir = root / "bin"
        bin_dir.mkdir()
        # Records where it ran, the checked-out file, any leaked Git index and its arguments.
        nix = bin_dir / "nix"
        nix.write_text('#!/bin/sh\nwhile [ -e "$SLOW" ]; do sleep 0.1; done\n'
                       'printf "%s|%s|%s|%s\\n" "$PWD" "$(cat file)" "${GIT_INDEX_FILE:-}" "$*" >> "$CALLS"\n'
                       'exit "${RESULT:-0}"\n')
        nix.chmod(0o755)
        self.env.update(PATH=f"{bin_dir}:{self.env['PATH']}", CALLS=str(self.calls), SLOW=str(self.slow))

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, env=self.env, text=True)

    def commit(self, content):
        (self.repo / "file").write_text(content + "\n")
        self.git("add", "file")
        self.git("-c", "core.hooksPath=/dev/null", "-c", "user.name=Quality Test",
                 "-c", "user.email=quality@example.invalid", "commit", "-qm", content)
        return self.git("rev-parse", "HEAD").strip()

    def run_hook(self, name, *args, stdin="", env=None):
        return subprocess.run(["bash", str(HOOKS / name), *args], cwd=self.repo, env=env or self.env,
                              input=stdin, text=True, capture_output=True, timeout=10)

    def push(self, oid=None):
        return self.run_hook("pre-push", stdin=f"refs/heads/main {oid or self.head} refs/heads/main {'0' * 40}\n")

    def gate(self, sha, latest=None):
        self.gates.mkdir(exist_ok=True)
        (self.gates / "latest").write_text((latest or sha) + "\n")
        return self.run_hook("quality-gate", sha)

    def verdict(self, sha):
        path = self.gates / sha
        return path.read_text().strip() if path.exists() else None

    def wait_for(self, sha, wanted, timeout=10):
        deadline = time.monotonic() + timeout
        while self.verdict(sha) != wanted and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(self.verdict(sha), wanted)

    def calls_made(self):
        return self.calls.read_text().splitlines() if self.calls.exists() else []

    def test_pre_commit_flake_checks_the_checkout_without_hook_git_paths(self):
        env = dict(self.env, GIT_INDEX_FILE=str(self.repo / ".git/index"))
        result = self.run_hook("pre-commit", env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls_made(), [f"{self.repo}|committed||flake check -L"])
        self.env["RESULT"] = "7"
        self.assertEqual(self.run_hook("pre-commit").returncode, 7)

    def test_gate_checks_the_commit_not_later_edits_and_records_the_verdict(self):
        (self.repo / "file").write_text("uncommitted\n")
        worktree = self.gates / "worktree"
        every = [f"{worktree}|committed||run .#quality", f"{worktree}|committed||run .#native-acceptance"]
        self.assertEqual(self.gate(self.head).returncode, 0)
        self.assertEqual(self.verdict(self.head), "ok")
        self.assertEqual(self.calls_made(), every)
        self.calls.unlink()
        self.env["RESULT"] = "7"
        self.assertEqual(self.gate(self.head).returncode, 7)
        self.assertEqual(self.verdict(self.head), "failed 7")
        self.assertEqual(self.calls_made(), every, "a failure skipped a later check")
        self.assertIn("native-acceptance failed (7)", (self.gates / f"{self.head}.log").read_text())

    def test_superseded_gate_checks_nothing(self):
        self.assertEqual(self.gate(self.head, latest="f" * 40).returncode, 0)
        self.assertIsNone(self.verdict(self.head))
        self.assertEqual(self.calls_made(), [])

    def test_post_commit_gates_in_background_and_cancels_the_earlier_commit(self):
        self.slow.touch()
        result = self.run_hook("post-commit")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"{self.head}.log", result.stderr)
        self.wait_for(self.head, "running")
        second = self.commit("second")
        self.assertEqual(self.run_hook("post-commit").returncode, 0)
        self.wait_for(self.head, "cancelled")
        self.slow.unlink()
        self.wait_for(second, "ok")
        self.assertEqual([call.split("|")[1] for call in self.calls_made()], ["second", "second"])

    def test_pre_push_needs_a_passing_gate_for_every_pushed_commit(self):
        self.gates.mkdir()
        for verdict, message in ((None, "post-commit " + self.head), ("running", "still running"),
                                 ("failed 7", "failed 7"), ("cancelled", "cancelled")):
            with self.subTest(verdict=verdict):
                if verdict:
                    (self.gates / self.head).write_text(verdict + "\n")
                result = self.push()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
        (self.gates / self.head).write_text("ok\n")
        self.assertEqual(self.push().returncode, 0)
        self.assertNotEqual(self.push("1" * 40).returncode, 0, "an unknown object was pushed")
        self.assertEqual(self.calls_made(), [], "pre-push ran a check itself")

    def test_deletion_only_push_needs_no_checks(self):
        self.assertEqual(self.push("0" * 40).returncode, 0)
        self.assertFalse(self.calls.exists())

    def test_fixture_git_does_not_modify_an_inherited_repository(self):
        # Only the disposable outer repo can be touched even before the fix.
        with patch.dict(os.environ, GIT_DIR=str(self.repo / ".git"),
                        GIT_WORK_TREE=str(self.repo), GIT_INDEX_FILE=str(self.repo / ".git/index")):
            nested = GitHookTests("test_deletion_only_push_needs_no_checks")
            result = unittest.TestResult()
            nested.run(result)
        self.assertEqual(result.errors + result.failures, [])
        self.assertEqual(self.git("rev-parse", "HEAD").strip(), self.head)
        self.assertEqual(self.git("status", "--porcelain"), "")

    def run_quality(self, spies, timeout=60):
        """Runs quality.sh in the fixture repo with each `spies` tool (name: sh body) first on PATH."""
        real_bash = shutil.which("bash")
        bin_dir = Path(self.env["PATH"].split(os.pathsep)[0])
        for name, body in spies.items():
            (bin_dir / name).write_text(f"#!/bin/sh\n{body}\n")
            (bin_dir / name).chmod(0o755)
        return subprocess.run([real_bash, str(ROOT / "scripts/quality.sh")], cwd=self.repo,
                              env=self.env, text=True, capture_output=True, timeout=timeout)

    def test_quality_runner_clears_git_context_before_running_tools(self):
        self.env.update(GIT_DIR=str(self.repo / ".git"), GIT_WORK_TREE=str(self.repo),
                        GIT_INDEX_FILE=str(self.repo / ".git/index"))
        no_git = 'test -z "${GIT_DIR:-}${GIT_WORK_TREE:-}${GIT_INDEX_FILE:-}"'
        result = self.run_quality(dict.fromkeys(("pytest", "cargo", "bash"), no_git))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_quality_runner_tests_headless_in_a_separate_target_dir(self):
        self.env.pop("CARGO_TARGET_DIR", None)
        result = self.run_quality({"pytest": "exit 0", "bash": "exit 0",
                                   "cargo": 'printf "%s|%s\\n" "${CARGO_TARGET_DIR:-}" "$*" >> "$CALLS"'})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        tests = [line for line in self.calls.read_text().splitlines() if "|test " in line]
        self.assertEqual(tests, ["|test --locked --all-targets --features desktop",
                                 "target/headless|test --locked --all-targets --no-default-features"])

    def test_quality_runner_fails_when_a_python_test_fails(self):
        # Real pytest over a disposable tests/python; every other tool is a passing spy.
        tests = self.repo / "tests/python"
        tests.mkdir(parents=True)
        for body, fails in (("assert True", False), ("assert False", True)):
            with self.subTest(body=body):
                (tests / "test_fixture.py").write_text(f"def test_fixture():\n    {body}\n")
                result = self.run_quality({"cargo": "exit 0", "bash": "exit 0"}, timeout=120)
                self.assertEqual(result.returncode != 0, fails, result.stdout + result.stderr)
                self.assertIn("1 failed" if fails else "1 passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
