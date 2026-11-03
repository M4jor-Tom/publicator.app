# DeviantArt publishing

- **Does:** Rust validates and journals each approved pending target, then drives the retained Python Playwright worker through the DeviantArt submit flow.
- **Run:** `nix run .#da-publish -- --data-dir DIR [--uuid ID | --all] [--headless]`; `nix run .#check-steps`; `nix run .#login`; `nix run .#pw-daemon` for explicit DOM probing.
- **Development launch:** without Nix wrappers, CLI and GUI (`Publisher::from_env` in `src/publishing.rs`) default to the checkout-relative `python/publicator-da-worker` launcher (adds its directory to `PYTHONPATH`, runs `python3 -m publicator_da.worker`) and `.claude/skills/publish-deviantart/SKILL.md`; run them from the checkout, e.g. `nix develop -c cargo run --bin publicator -- check-steps`. `PUBLICATOR_WORKER`/`PUBLICATOR_SKILL` override both; the Nix wrappers set them to packaged assets.
- **Code:** `src/publishing.rs`, `src/publish_journal.rs`, `src/publish_protocol.rs`, `src/artwork.rs`, `python/publicator_da/`, `.claude/skills/publish-deviantart/SKILL.md`, `src/bin/`.
- **Tests:** `tests/publishing.rs`, `tests/publish_protocol.rs`, `tests/python/test_worker.py`, `tests/python/test_browser.py`, `tests/python/test_pw_daemon.py`, `tests/worker_integration.rs`, `tests/python/test_browser_dom.py` (offline headless Firefox on the routed local `submit_form.html`; no live selectors); `tests/cli.rs` runs the no-override checkout `check-steps` default.
- **Config:** Data-dir tags and DeviantArt allow-lists are validated by Rust before worker startup.
- **Data:** `.deviantart-login/` is the human-owned original profile; `.deviantart-session/` is a disposable working copy. `.publicator/` holds persistent receipts/intents/staging; `.publicator/publish.lock` excludes concurrent publishers.
- **Decisions:** Rust owns database/config/validation/recovery. Python receives one bounded job at a time and never reads/writes publication data. Keep these data paths ignored in the separate data repository.
- **Verify:** `nix develop -c cargo test --test publishing --test publish_protocol --test worker_integration`
- **Verify:** `nix develop -c env PYTHONPATH=python pytest tests/python -q`
- **Verify:** `nix run .#check-steps`

## How it works

Rust validates the complete selected batch and current approved record before dispatch, writes a durable intent, and sends one artwork at a time to the versioned NDJSON worker. A result is saved before the next send. Success receipts prevent duplicate submissions on restart. If the remote outcome is unknown, preserve the receipt and stop; human reconciliation is required before retry.

The Playwright worker uses the existing ordered browser steps and static skill. Rust binds the Store data directory at process startup with `--data-root`; the worker accepts only the `.deviantart-login` and `.deviantart-session` children of that trusted root, while protocol v1 carries no root authority. The login profile is never opened for publishing: Rust/worker use a copy. DeviantArt blocks automated login behind PerimeterX, so `login` remains a human-operated real Firefox action.

## Worker trust boundary

The worker is an isolated child that trusts its startup `--data-root` and the Rust parent writing its stdin; it is not a second validator of publication data, so Python gets no copies of Rust-owned checks.

- **Rust owns** every publication-level property before dispatch: Store/schema validation (including non-empty title and description), price validation (`save_options` rejects non-finite and negative prices; a serde_json value cannot hold NaN/Infinity and would be written as `null`, which the worker rejects), artwork resolution and preparation (a regular non-symlink file found in the data dir with a verified hash and sniffed format; every job uploads a hash-verified per-attempt copy staged under `.publicator/`, WebP converted to PNG, so the worker never opens the mutable original), and timezone and wall-time generation (session and schedule zone come from one config value; wall times derive from the epoch and DST-gap times are rejected).
- **Python owns** what keeps the process safe on its own: data-root and profile binding (only the `.deviantart-login`/`.deviantart-session` children of the trusted root, checked before session acquisition), the immutable original login profile (publishing uses a copy), NDJSON protocol framing (1 MiB lines, UTF-8, exact fields, version 1, UUID syntax, epoch recomputed through `zoneinfo`), WebP suffix refusal and nonzero-minute refusal, all before any session mutation. Its price check is NaN-proof (`0 <= price < inf`) because `json.loads` accepts `NaN`/`Infinity` tokens.
- `tests/worker_integration.rs` drives the real worker (`tests/python/recording_worker.py` injects a recording session into `serve`; there is no fake mode) and pins the split: a valid job is scheduled from a staged copy whose SHA-512 matches the artwork; Rust-owned faults never reach the worker; a shared frame corpus records which side refuses each frame (a Python refusal is a `fatal` reply with no session submit); and a lost result (exit after submit, partial frame then EOF, mismatched `attempt_id`, `fatal` after submit, explicit `unknown`) stays `unknown` with its durable intent and is not resent until reconciled.

## Recovery

Classify a nonzero `da-publish` exit before any manual action. When the summary shows `unknown: 1` or more, or a later run refuses with `reconciliation required`, the CLI lists each blocked attempt as `UNRESOLVED <attempt> (<uuid>): <reason>` with its two reconcile commands. The submit may have reached DeviantArt: inspect DeviantArt first, then run `da-publish --reconcile-attempt <attempt> --outcome scheduled [--url URL]` if it is there or `--outcome not-submitted` if it is absent. Retry or manual upload is allowed only after `not-submitted`. `unknown: 0` with no `UNRESOLVED` line (a `failed` result or an error before dispatch) is a safe failure: check for a leftover draft, then the static skill's manual steps apply. Unresolved receipts are never deleted. `tests/cli.rs` checks that the CLI guidance and the skill's recovery prose agree.

## Invariants

- The complete source document, target, options, tags, schedule and artwork identity validate before sending.
- Approval and upload come from one fresh read: `files[0]` resolves to the visible file whose bytes match the recorded SHA-512 (same-named files are told apart by hash; changed bytes require reconciliation). The worker opens only a hash-verified per-attempt copy under `.publicator/`, never the mutable original.
- A target sends only while its DeviantArt delivery is still `unpublished`; a delivery recorded since approval stops the batch. Recovery still replays a receipt whose delivery is already applied.
- Python receives no publication database/config path and cannot mutate the Store.
- Per-target receipt and Store state survive restart; do not delete `.publicator/` to clear a failure.
- An ambiguous external result is reconciled explicitly, never automatically resubmitted.
- `scheduled` needs positive evidence: after the final Schedule click the page must navigate within a bounded wait to another path on the form's host that is not a login page. A timeout or any error after the click reports `unknown`; any error before it reports `failed`.
- `.webp` is converted before upload; STEPS ordering remains synchronized with the static skill.
- The original browser profile is read-only; publish uses a separate working profile.

## Gotchas

- A browser flow may have created a remote draft before failing. Check DeviantArt before any manual retry.
- `pw-daemon` is a debugging DOM prober, not the production worker or login flow. It execs whatever reaches its command FIFO, so that FIFO, its log and screenshot live in a fresh private 0700 `pw-*` temp directory whose paths it prints; it refuses any pre-existing endpoint before starting the browser.
- Never run the real publisher for validation or change a real profile/data directory.
