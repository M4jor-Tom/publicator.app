# Outstanding synthetic-data requirements

This is a removable fixture checklist, not a request for a new fixture framework. Reuse
`tests/common::Fixture`, existing row/image helpers, fake executables, loopback servers and
`examples/ui_acceptance.rs`. Each block can disappear once its relevant behavior tests exist;
creating files alone does not close it. Detailed fault oracles remain with their subsystem items.

## FIX-09 — Real native UI datasets

**Required:** Empty/history/mixed/publish/calendar/prompt-search/missing-archive/reopen scenarios;
at least 30 distinguishable thumbnails beyond one viewport; long Unicode/newline/literal-markup
metadata, long prompt panels and plural options. Include dirty, saving, provider-pending and
publish-pending states, failed saves, retry and unresolved receipt cases.

**Done when:** Actual native replay establishes focus, keyboard input, visible image identity,
scroll/resize/jump behavior, persisted edits and safe close. Reuse ui_acceptance dependency injection
and retain fake-call/state/PID evidence outside Git. Message-only tests remain useful but separate.
