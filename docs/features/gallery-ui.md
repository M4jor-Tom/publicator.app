# Gallery UI

- **Does:** Native Iced human-review workflow for Pick, Schedule, Describe and Publish, with a read-only Calendar.
- **Run:** `nix run .#ui -- --data-dir DIR`; for a disposable dummy data dir, `nix develop -c scripts/dummy-data.sh [DIR]` prints one to pass as `DIR`.
- **Code:** `src/ui/`, `src/application.rs`, `src/workflow.rs`, `src/store.rs`, `src/calendar.rs`.
- **Tests:** `tests/ui*.rs`, `tests/calendar.rs`, `tests/application*.rs`, `tests/docs.rs`.
- **Config:** Search uses a runtime subset of configured `publicable` roots; schedule and prompt options come from the data-dir config. `--openrouter-model NAME` (with or without the `openrouter/` prefix) selects the initial supported vision model; an unsupported name fails at startup.
- **Data:** `publications.json` is authoritative on refresh/reopen; `.thumbs/` is persistent preview cache. Deletion applies only to unpicked candidate files.
- **Decisions:** Native Iced controls replace the retired HTTP/HTML UI and keep its look (`src/ui/style.rs`: dark page, #111 header with tab counts, card grid, browser-grey buttons, bordered Publish table, month grids). App operations own state changes; no RAM-only queue or browser clock.
- **Verify:** `nix develop -c cargo test --features desktop --test ui_workflow --test ui_publish_calendar --test calendar`

## Development sandbox

Run `nix develop -c ui-sandbox [SCENARIO]`, or `ui-sandbox [SCENARIO]` inside
`nix develop`. The default scenario is `mixed`; alternatives include `gallery`,
`calendar`, `empty`, `history`, `publish`, `prompt-search`,
`prompt-search-missing-archive` and `reopen`.

This dev-shell-only command builds/runs `examples/ui_acceptance.rs` in release
mode with fake AI and publishing services. Each launch prints a fresh `/tmp`
fixture directory, preserved after exit for inspection. It is not an exported
Nix app/package or a dependency of production builds. By contrast, dummy data
from `scripts/dummy-data.sh` does **not** replace real provider/publishing services.

## How it works

Pick scans configured roots and supports a runtime subset. Candidate limits are Empty/0, 25, 50, 75, or 100; startup defaults to 25 and all configured `publicable` paths selected. A candidate's file name, and the path in its delete confirmation, wraps between glyphs where it has no word break (a digest), so the whole name stays inside its card or column. A candidate's prompt panel shows only its heading (Archived prompt, or the NOT archived candidate warning) until its Show button opens the prompt text; Hide closes it, per card. Calendar prompts are unaffected: the event chip already collapses them. The four workflow phases have distinct actions. UI actions call Rust application operations, which persist validated state. Refresh and restart reconstruct the workflow and history from disk.

Thumbnails load lazily: each pending placeholder reports when it comes within one thumbnail of the visible list (an Iced sensor on the rendered layout, so any row height, column count, resize or scroll jump loads exactly the cards on screen), at most four at a time, and loading keeps filling as previews finish; calendar events are compact chips whose artwork loads when expanded.

Publish selection is separate from submission. Only registered platform cells are executable. Publish and its tab count list only publications whose DeviantArt delivery is still pending; a completed send leaves the list (Calendar still shows it), except when opened from Calendar or while an unresolved attempt awaits reconciliation, where it shows locked. Retries skip persisted successes. Calendar is read-only and navigates to the relevant workflow phase.

## Invariants

- UI and CLI share Rust application operations and Store; the Python worker is not an application service.
- Draft edits are revision-safe and flush at phase changes; failed saves remain visible and retryable. A draft has at most one save in flight; its completion saves any newer revision, so the newest lands last. Reloads show stored text for every field without unsaved or in-flight input.
- Closing never interrupts publishing. Every close path (window close, Discard drafts and close, a save or status refresh completing a pending close) shows Wait/Cancel and exits only after the durable outcome and worker cleanup. Refresh is refused while publishing, because receipt loading needs the publisher lock.
- Replies apply only to the request that is still current. Reload replies carry the reload generation that requested them, calendar preparations and thumbnails the generation of the rows they read; older ones are dropped. Publish is refused while a run is active, and a publish completion from another or an already finished run changes nothing.
- Refresh invalidates the prompt archive inside its reload task (off the UI thread), resolves prompt labels again and discards unsaved custom date input. Any date save discards that row's custom input, and Approve date is refused while the row shows unsaved custom input.
- Dates and approvals require explicit user action. Only valid complete metadata advances to DESCRIBED.
- Publish results are durably recorded per target. Unknown remote outcomes require reconciliation; never infer failure and resend.
- Native selection controls remain keyboard-operable and visibly indicate focus/context.
- Actual stored apparition URLs are the only external URL actions; no URL-slug guessing.

## Gotchas

- The native UI is a local trusted-human application, not a network service.
- The GUI binary and its CLI smoke test require `--features desktop`; default headless tests must not launch that optional binary.
- Keep test fixtures disposable and publishers/providers stubbed. Never use the UI acceptance harness to submit real art.
