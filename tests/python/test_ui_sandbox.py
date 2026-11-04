"""Keep the development launcher disposable and pointed at the fake-service UI."""

import os
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[2]
LAUNCHER = ROOT / "scripts/ui-sandbox.sh"


def test_launcher_uses_fresh_preserved_fixtures_and_propagates_cargo_exit(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    cargo = bin_dir / "cargo"
    cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$PWD" "$@" > "$CALLS"\nexit "$RESULT"\n')
    cargo.chmod(0o755)
    temp_root = tmp_path / "fixtures with spaces"
    temp_root.mkdir()
    calls = tmp_path / "calls"
    env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}",
               TMPDIR=str(temp_root), CALLS=str(calls))
    fixtures = []
    try:
        for arguments, scenario, status in (([], "mixed", 0), (["gallery"], "gallery", 7)):
            result = subprocess.run(
                ["bash", str(LAUNCHER), *arguments], cwd=tmp_path,
                env=dict(env, RESULT=str(status)), text=True, capture_output=True, timeout=10,
            )
            assert result.returncode == status, result.stdout + result.stderr
            cwd, *args = calls.read_text().splitlines()
            fixture = Path(args[args.index("--fixture-dir") + 1])
            fixtures.append(fixture)
            assert Path(cwd).resolve() == ROOT
            assert args == ["run", "--locked", "--release", "--example", "ui_acceptance",
                            "--features", "desktop", "--", "--fixture-dir", str(fixture),
                            "--scenario", scenario]
            assert fixture.parent == Path("/tmp"), "nix develop removes its own TMPDIR on exit"
            assert fixture.is_dir(), "fixture survives both successful and failed launches"
            assert str(fixture) in result.stdout
        assert fixtures[0] != fixtures[1]
    finally:
        for fixture in fixtures:
            fixture.rmdir()


def test_launcher_rejects_extra_arguments_before_creating_a_fixture(tmp_path):
    result = subprocess.run(
        ["bash", str(LAUNCHER), "mixed", "extra"],
        env=dict(os.environ, TMPDIR=str(tmp_path)),
        text=True, capture_output=True, timeout=10,
    )
    assert result.returncode == 2
    assert "usage:" in result.stderr
    assert list(tmp_path.iterdir()) == []
