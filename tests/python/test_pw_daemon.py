import os
import stat
import tempfile

import pytest

from publicator_da import pw_daemon


class BrowserAcquired(Exception):
    pass


@pytest.fixture
def acquired(monkeypatch):
    calls = []

    def fake_sync_playwright():
        calls.append("sync_playwright")
        raise BrowserAcquired

    monkeypatch.setattr(pw_daemon, "sync_playwright", fake_sync_playwright)
    return calls


def test_daemon_creates_private_endpoints_before_browser(tmp_path, monkeypatch, capsys, acquired):
    monkeypatch.setattr(tempfile, "tempdir", str(tmp_path))
    with pytest.raises(BrowserAcquired):
        pw_daemon.main(["--data-dir", str(tmp_path / "data")])
    assert acquired == ["sync_playwright"]
    (run,) = tmp_path.glob("pw-*")
    assert stat.S_IMODE(run.lstat().st_mode) == 0o700 and run.lstat().st_uid == os.getuid()
    assert stat.S_ISFIFO((run / "pw.cmd").lstat().st_mode)
    log = (run / "pw.log").lstat()
    assert stat.S_ISREG(log.st_mode) and stat.S_IMODE(log.st_mode) & 0o077 == 0
    assert not (run / "pw.png").exists()
    out = capsys.readouterr().out
    assert all(str(run / name) in out for name in ("pw.cmd", "pw.log", "pw.png"))


def plant_regular_cmd(run, payload, victim):
    (run / "pw.cmd").write_text(payload)


def plant_cmd_symlink(run, payload, victim):
    victim.write_text(payload)
    (run / "pw.cmd").symlink_to(victim)


def plant_cmd_fifo(run, payload, victim):
    os.mkfifo(run / "pw.cmd")


def plant_log_symlink(run, payload, victim):
    (run / "pw.log").symlink_to(victim)


def plant_png_symlink(run, payload, victim):
    (run / "pw.png").symlink_to(victim)


@pytest.mark.parametrize("plant", [plant_regular_cmd, plant_cmd_symlink, plant_cmd_fifo, plant_log_symlink, plant_png_symlink])
def test_daemon_refuses_planted_endpoints_before_browser_or_exec(tmp_path, monkeypatch, acquired, plant):
    run = tmp_path / "run"
    run.mkdir(mode=0o700)
    sentinel, victim = tmp_path / "executed", tmp_path / "victim"
    if plant is not plant_cmd_symlink:
        victim.write_text("keep")
    plant(run, f"open({str(sentinel)!r}, 'w').write('executed')", victim)
    before = victim.read_text()
    monkeypatch.setattr(pw_daemon.tempfile, "mkdtemp", lambda **_: str(run))
    with pytest.raises(FileExistsError):
        pw_daemon.main(["--data-dir", str(tmp_path / "data")])
    assert acquired == []
    assert not sentinel.exists()
    assert victim.read_text() == before
    assert not (tmp_path / "data").exists()
