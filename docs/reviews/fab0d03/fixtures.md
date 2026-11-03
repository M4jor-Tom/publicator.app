# Outstanding synthetic-data requirements

This is a removable fixture checklist, not a request for a new fixture framework. Reuse
`tests/common::Fixture`, existing row/image helpers, fake executables, loopback servers and
`examples/ui_acceptance.rs`. Each block can disappear once its relevant behavior tests exist;
creating files alone does not close it. Detailed fault oracles remain with their subsystem items.

## FIX-05 — Prompt Git repositories

**Required:** Fresh empty repo, old/new/deleted paths, duplicate basenames, exact/nearest/unknown,
unreachable blobs before and after warm-up, non-HEAD ref changes, object removal, quoted/tab/newline/
backslash paths, missing/malformed archives and malformed framing. Include a small truncation corpus
whose early matches are referenced and later matches are available.

**Done when:** Isolated Git operations cannot inherit unrelated GIT_DIR/GIT_WORK_TREE/GIT_INDEX_FILE.
Probe aging is explicit where private access permits. Assertions distinguish digest identity,
lineage paths, refresh policy and rendered authority. No real prompt archive or giant blobs.

## FIX-08 — Persistence, recovery and process ownership

**Required:** Intent-only, scheduled-not-applied, applied, unknown, not-submitted, empty/orphan/invalid
receipt, conflicting terminal, pre-/post-rename failures and receipt-write interruption. Add gated
workers/checkers/image tools, stderr-filling workers and deliberately detached pipe holders only
inside watchdog-owned processes with PID witnesses.

**Done when:** Reopen/restart and inspect bytes, events, no-next-send, lock/lease release and retry
blocking. Tests kill/reap all owned processes even when assertions fail. Gates synchronize causal
windows; elapsed-time sleeps alone are not race proof. Preserve original login profiles; synthetic
cookies.sqlite/profile trees suffice for copy/containment checks.

## FIX-09 — Real native UI datasets

**Required:** Empty/history/mixed/publish/calendar/prompt-search/missing-archive/reopen scenarios;
at least 30 distinguishable thumbnails beyond one viewport; long Unicode/newline/literal-markup
metadata, long prompt panels and plural options. Include dirty, saving, provider-pending and
publish-pending states, failed saves, retry and unresolved receipt cases.

**Done when:** Actual native replay establishes focus, keyboard input, visible image identity,
scroll/resize/jump behavior, persisted edits and safe close. Reuse ui_acceptance dependency injection
and retain fake-call/state/PID evidence outside Git. Message-only tests remain useful but separate.
