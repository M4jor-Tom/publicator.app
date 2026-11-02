# Worker boundary and operator safety

These items concern the optional DOM prober, human recovery contract and cross-process behavior.
Do not weaken unknown-outcome protection to make a retry convenient. Use private temporary paths
and synthetic sessions; never test through the real prober IPC paths or browser profiles.

## PUB-R3 — P3 · Risk · Abrupt abortion differs from cooperative cleanup

**Evidence:** `src/publishing.rs:205–225,710–719` awaits cleanup after normal batch return; worker
spawn has no drop-cleanup policy. Diagnostic draining after group shutdown lacks its own deadline.
No abrupt-abort or detached-pipe failure was reproduced.

**Conditional impact:** Dropping/aborting the future or terminating the CLI can skip cooperative
ownership. A detached descendant retaining stderr can prevent the drain from completing.

**Existing coverage:** `tests/publishing.rs:461,976` and
`tests/ui_publish_calendar.rs:422,613` cover cooperative cancellation and same-group descendants.
Do not describe those normal paths as absent or broken.

**Regression / close when:** Use gated synthetic processes with PID witnesses. Abort an in-flight
task and separately interrupt its CLI; inspect children, publisher lock and durable pending intent.
Test a deliberately detached stderr-holder in a watchdog-owned process. Require bounded teardown
and explicit fixture cleanup without losing retry-blocking unknown state. Test-only outer watchdogs
must kill/reap their own descendants even when the current implementation fails the assertion.
