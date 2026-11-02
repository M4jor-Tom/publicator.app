"""Playwright daemon. Reads Python command chunks from a private per-run FIFO, execs
with `page`, `context`, `p` in scope. Writes screenshot + log after each command.

Chromium persistent context in the publication database dir (--data-dir, default
CWD) `.deviantart-session/`. It is NOT logged in: publish_batch refills that dir with
a Firefox profile Chromium cannot read, and PerimeterX blocks Playwright browsers at
the login page. A DOM-probing tool, not a publishing path.

# ponytail: single-page, single-context daemon. Add tab tracking if the
# publication flow starts spawning multiple tabs we care about.
"""
import argparse
import io
import os
import sys
import tempfile
import traceback
from contextlib import redirect_stdout, redirect_stderr
from pathlib import Path

from playwright.sync_api import sync_playwright


def private_endpoints() -> tuple[Path, Path, Path]:
    """Create this run's command FIFO, log and screenshot paths in a fresh 0700 dir.

    mkfifo and O_EXCL|O_NOFOLLOW fail on ANY pre-existing entry (regular file,
    symlink, FIFO, other owner), so a planted endpoint is refused before the
    browser starts and before anything is read or executed.
    """
    # ponytail: the run dir is left behind for post-mortem; tmp cleaners reap it.
    run = Path(tempfile.mkdtemp(prefix="pw-"))
    cmd, log_path, shot = run / "pw.cmd", run / "pw.log", run / "pw.png"
    if os.path.lexists(shot):
        raise FileExistsError(f"refusing pre-existing screenshot endpoint {shot}")
    os.mkfifo(cmd, 0o600)
    os.close(os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600))
    return cmd, log_path, shot


def log(path: Path, msg: str) -> None:
    with open(path, "a") as f:
        f.write(str(msg).rstrip() + "\n")


def main(argv: list[str] | None = None) -> None:
    _ap = argparse.ArgumentParser(description="Playwright daemon for manual DA publishing.")
    _ap.add_argument("--data-dir", default=None,
                     help="publication database dir holding .deviantart-session/; default: CWD")
    _dd = _ap.parse_args(argv).data_dir
    SESSION = (Path(_dd).resolve() if _dd else Path.cwd()) / ".deviantart-session"

    fifo, log_path, shot = private_endpoints()
    print(f"pw-daemon FIFO: {fifo}\npw-daemon log: {log_path}\npw-daemon screenshot: {shot}", flush=True)
    SESSION.mkdir(parents=True, exist_ok=True)

    with sync_playwright() as p:
        context = p.chromium.launch_persistent_context(str(SESSION), headless=False)
        page = context.pages[0] if context.pages else context.new_page()
        log(log_path, f"READY pid={os.getpid()}")
        seq = 0
        scope = {"p": p, "context": context, "page": page}
        while True:
            with open(fifo, "r") as f:
                cmd = f.read()
            if not cmd.strip():
                continue
            seq += 1
            log(log_path, f"--- CMD {seq} ---")
            log(log_path, cmd)
            buf = io.StringIO()
            try:
                with redirect_stdout(buf), redirect_stderr(buf):
                    exec(cmd, scope)
                out = buf.getvalue()
                if out:
                    log(log_path, out)
                log(log_path, f"--- OK {seq} ---")
            except Exception:
                out = buf.getvalue()
                if out:
                    log(log_path, out)
                log(log_path, traceback.format_exc())
                log(log_path, f"--- ERR {seq} ---")
            page = scope.get("page", page)
            try:
                page.screenshot(path=shot, full_page=False)
            except Exception as e:
                log(log_path, f"screenshot err: {e}")


if __name__ == "__main__":
    sys.exit(main())
