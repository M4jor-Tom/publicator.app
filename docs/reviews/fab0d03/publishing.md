# Browser publishing, approval and recovery

Contract: `docs/features/deviantart-publish.md` and the static browser/recovery skill.
Existing invariants are important: durable intent before send, receipt before Store mutation/next
send, conservative unknown outcomes, replay and cooperative cleanup. Preserve them.

## PUB-01 — P1 · Defect · Click-and-wait becomes durable success without confirmation

Also reported as **VER-01**; one canonical item owns both reports.

**Evidence:** `python/publicator_da/browser.py:68–75,169–171` returns scheduled after clicking and
waiting five seconds without observing the publishing effect. Rust records it
(`src/publishing.rs:310–316`) and excludes that target later (`637–665`). Confidence: high.

**Conditional impact:** Click succeeds but validation rejects, a challenge appears, or acceptance
is delayed/absent. Local state falsely records success. This behavior was preserved during
migration, not demonstrated as a newly introduced regression; no live failure was observed.

**Existing coverage:** `tests/python/test_browser.py:335–421` has permissive page doubles;
`tests/python/test_worker.py:17–27` manufactures terminal success. Rust's recovery tests correctly
trust the terminal event, but none establishes its truth at the browser boundary.

**Regression / close when:** Invoke actual `BrowserSession.submit` on fake page states and a routed
local DOM: acknowledged success, rejection/no confirmation, precommit exception, postcommit
exception/timeout. No scheduled event without positive bounded evidence; attempted but unconfirmed
commit stays unknown. No next send or automatic retry after unknown. Deny non-loopback network.
Local DOM contract tests do not prove current live-site selectors/authentication.

## PUB-03 — P2 · Defect · Gallery membership uses substring matching

**Evidence:** `python/publicator_da/browser.py:163–166` checks requested text inside aggregate
combo text. Selected Artwork can therefore suppress requested Art. Confidence: high.
`tests/python/test_browser.py:335,385` covers plural tiers/unset galleries, not selected membership.

**Regression / close when:** Stateful locators/local selected-label chips containing Artwork do
not suppress Art; an already selected exact Art is not toggled off. Verify plural/empty selections.
Compare exact selected identities, not aggregate text. Final success must retain requested options.

## PUB-04 — P2 · Defect · Failed tag clearing does not prevent submission

**Evidence:** `python/publicator_da/browser.py:147–155` ignores removal exceptions and continues
after sixty iterations without checking that old chips disappeared. Confidence: high.
Existing submission doubles always report zero chips (`tests/python/test_browser.py:335–421`).

**Trigger/impact:** Persistent/non-removable chips or more than sixty old tags survive; new tags
are appended and artwork submits with unintended tags.

**Regression / close when:** Model removal failure, no progress, excess chips and successful
clearing. Assert the old-tag-empty postcondition before new tags/final commitment; failures never
reach the commit callback. Count actual chips and commit attempts, not merely helper invocations.

## PUB-05 — P2 · Defect · Checkbox success on the last attempt still raises

**Evidence:** `python/publicator_da/browser.py:88–95` checks before clicks, never after the final
third click. Submission tests replace the helper with a no-op (`tests/python/test_browser.py:365,415`).
Confidence: high; valid preparation can be aborted after reaching the desired state.

**Regression / close when:** Directly test initially correct state, transitions on each allowed
click, and never-sticks behavior using a counter-backed page. Final-attempt success returns normally;
true failure remains bounded and prevents commitment. Verify the final observed state.

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

## PUB-C02 — P2 · Gap · Actual calendar/time controls need local-DOM readback

**Evidence:** `tests/publishing.rs:389`, `tests/publish_protocol.rs` and
`tests/python/test_browser.py:91–120` check Rust schedule vectors and helper behavior, not actual
DOM hour selection/readback or a changing calendar. No live selector failure is asserted.

**Regression / close when:** Route a local date/time page with changing month/year, exact hour
selection and readback, rejected/nonexistent times and declared fold policy. Feed shared fixed
vectors through Rust jobs and the real Python session logic. Assert the intended instant, timezone
and observed selected values. Keep live-site compatibility outside offline acceptance claims.
