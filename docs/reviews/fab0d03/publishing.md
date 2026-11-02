# Browser publishing, approval and recovery

Contract: `docs/features/deviantart-publish.md` and the static browser/recovery skill.
Existing invariants are important: durable intent before send, receipt before Store mutation/next
send, conservative unknown outcomes, replay and cooperative cleanup. Preserve them.

## PUB-06 — P2 · Defect · Interrupted intent creation can block all recovery

**Evidence:** Attempt directories precede receipt installation (`src/publish_journal.rs:272–284,914–935`).
Recovery reads every directory as complete (`406–412,509–544`); reconciliation also requires the
receipt (`447–456`). Existing injected parent-sync failure leaves the directory (`1043–1056`).
Confidence: high; no artwork is dispatched for that incomplete intent.

**Trigger/impact:** Crash/disk/sync failure between directory creation and receipt installation
leaves an empty directory. Every later publish fails during recovery and supported reconciliation
cannot resolve it. `tests/publish_journal.rs:409` correctly rejects empty receipts, but the fault
unit test stops before restart.

**Regression / close when:** Extend that fault through reopen/recovery/reconciliation; also create
a crash-like empty UUID directory. Require an explicit safe way to distinguish/recover incomplete
creation while preserving valid unknown/corrupt receipts for investigation. Never broadly ignore
malformed entries or delete potentially submitted intents to obtain green tests.

## PUB-07 — P2 · Defect · Step-check subprocess has no bounded lifecycle

**Evidence:** `src/publishing.rs:117–124` awaits status without a deadline, while publication owns
its lease/lock (`166–176`). This precedes bounded worker ownership (`196–225`); normal UI close
waits while publishing (`src/ui/mod.rs:1798–1806`). Confidence: high.

**Existing coverage:** `tests/publishing.rs:361` checks nonzero checker exit; ready/submit timeout
and worker termination tests (`src/publishing.rs:892–994`) do not cover a checker hang.

**Regression / close when:** Fake executable writes its PID and hangs only for check-steps.
Require bounded error, no session submission, reaped process tree, cancellation handling and lock
reacquisition. Use short test deadlines plus an external watchdog. Do not confuse this with VER-07's
separate development executable-path failure or META-03's image-tool timeout.

## PUB-C01 — P2 · Gap · Receipt-write and crash checkpoints need end-to-end witnesses

**Evidence:** `tests/publishing.rs:596,698,831,862,898` and
`tests/publish_journal.rs:88,211,228,336` cover success ordering, unknown blocking and replay.
Store failures are exercised at `src/publishing.rs:1055,1087`; equivalent receipt-write/crash
checkpoints remain an inspected coverage gap, distinct from PUB-06's incomplete directory.

**Regression / close when:** Inject failure before/after terminal receipt persistence and before/
after Store application; reopen the journal/process. Assert durable bytes, no next send on uncertain
persistence, correct replay and no resend after unknown. Include already-applied/not-submitted
outcomes and conflicting terminals. Reuse private fault hooks and scripted workers, not the basic
always-success `tests/support/fake-worker.sh` as proof of recovery.

