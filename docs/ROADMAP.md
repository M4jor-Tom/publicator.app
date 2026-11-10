# Roadmap

Open work only; remove an item when it lands. The completed Rust/Iced migration plan, its checkpoints and the fab0d03 review live in Git history (`7b10352` is the last full migration roadmap).

## DeviantArt worker and images

- The manual fallback (`echo-first` → SKILL upload) hands over the original file, which can be `.webp`; DeviantArt refuses `.webp`, while automation converts via `images::prepare_upload`.
- The worker's `_pick_combo` (`python/publicator_da/browser.py`) only ensures the requested tiers and galleries are selected: it never deselects ones DeviantArt pre-selects that the job did not request, and leaves the tier box untouched when the job has no tiers, so a piece approved free could go out subscriber-only if DeviantArt pre-fills tiers. The new tier check is also unproven on the live page: selected chips must show the exact option text, otherwise every tiered publish fails safely before commit. Needs live-page knowledge: verify on the next supervised tiered publish, then make the selection exact.

## Testing

- The offline browser DOM tests (`tests/python/test_browser_dom.py`) skip silently when the host's `LD_LIBRARY_PATH` leaks into `nix develop` through the shellHook and Firefox fails to start, so `nix run .#quality` can pass without them; fail or clear the variable instead.
- The native replay's `previews()` probe (`scripts/native-ui-replay.py`) can accept a frame where a visible card still shows "Loading preview…"; `schedule_custom_preset_refresh_and_persistence` failed once on that race and passed 3/3 in isolation.
- Watch: one historical `publish_options_ui_persists_plural_allow_lists_zero_and_clear` failure was never explained; no current defect was found. Investigate if it recurs.

## Data directory (owner actions, outside this repo)

- Delete `.thumbs/` once: full-size entries written before the thumbnail fix are never evicted.
- Add `.publicator/` to the data repository's ignore rules before the first publishing receipt lands there.

## Backlog

- **Platform-independent scheduling.** Trigger `nix run .#da-publish -- --all --headless` from an external scheduler (e.g. a GitHub Actions cron) instead of paying for DeviantArt's premium scheduling. Not started; the runner needs the human-made PerimeterX login profile. Does not authorize live scheduling.
