"""Test-only worker entry: the real `serve` with an injected recording session.

Usage: recording_worker.py RECORD BEHAVIOUR (--check-steps --skill SKILL | --data-root DIR).
Each session call appends one JSON line to RECORD so `tests/worker_integration.rs` can see
which requests reached the browser. BEHAVIOUR scripts the session's reply to a submit.
Not collected by pytest (no `test_` prefix) and never shipped.
"""
import hashlib
import json
import os
import sys
from pathlib import Path

from publicator_da import worker

RECORD, BEHAVIOUR = sys.argv[1:3]
ARGS = sys.argv[3:]


def record(*entry):
    with open(RECORD, "a") as f:
        f.write(json.dumps(entry) + "\n")


class RecordingSession:
    def __init__(self, command, data_root):
        record("open", command, str(data_root), os.getpid())

    def submit(self, job):
        # The staged upload is deleted after the batch, so hash it while the worker holds it.
        image = Path(job["image_path"])
        digest = hashlib.sha512(image.read_bytes()).hexdigest() if image.is_file() else None
        record("submit", job, digest)
        reply = {"type": "scheduled", "attempt_id": job["attempt_id"], "uuid": job["uuid"]}
        if BEHAVIOUR == "failed":
            return dict(reply, type="failed", error="form refused before commit")
        if BEHAVIOUR == "unknown":
            return dict(reply, type="unknown", error="submit clicked, no confirmation")
        if BEHAVIOUR == "fatal":
            raise RuntimeError("browser crashed after submit")
        if BEHAVIOUR == "mismatch":
            other = "00000000-0000-4000-8000-00000000dead"
            sys.stdout.write(json.dumps(dict(reply, protocol=1, attempt_id=other)) + "\n")
        if BEHAVIOUR == "partial":
            sys.stdout.write('{"protocol":1,"type":"sched')
        sys.stdout.flush()
        if BEHAVIOUR in ("exit", "partial"):
            os._exit(1)
        return reply

    def close(self):
        record("close")


if ARGS[:1] == ["--check-steps"]:
    sys.argv[1:] = ARGS
    raise SystemExit(worker.main())
raise SystemExit(worker.serve(sys.stdin, sys.stdout, Path(ARGS[ARGS.index("--data-root") + 1]), RecordingSession))
