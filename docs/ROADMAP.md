# Roadmap

Open work only; remove an item when it lands. The completed Rust/Iced migration plan, its checkpoints and the fab0d03 review live in Git history (`7b10352` is the last full migration roadmap).

## Artwork lookup

- `echo-first`, the Describe/Schedule row previews and the Calendar still find artwork by first matching basename (`images::index_by_basename`), not the hash-verified `artwork::resolve`. `echo-first` feeds the manual SKILL fallback upload, so it can hand over a different file than the recorded hash. Behavior change: confirm before switching.
- Describe rebuilds the whole data-dir basename index once per shown row (`src/ui/describe.rs`); build it once per load.

## Native UI

- Long candidate file names overflow Pick cards.
- A tall Calendar day grows alone, leaving blank space under its week neighbours.

## DeviantArt worker and images

- Tiers use blind `_pick_combo`; galleries first check `_combo_labels` (`python/publicator_da/browser.py`).
- `identify -regard-warnings` (`src/images.rs`) refuses any image with a decoder warning, even a harmless one.

## Code health

- `publish_journal::apply_delivery` duplicates the state transition in `workflow::record_delivery`. Consider one shared pure transition, without nesting Store mutations or weakening atomicity.

## Testing

- Native replay (`nix run .#native-acceptance`) runs on demand only, not in `nix flake check`, and drives xdotool on Xvfb rather than a VNC bridge.
- Watch: one historical `publish_options_ui_persists_plural_allow_lists_zero_and_clear` failure was never explained; no current defect was found. Investigate if it recurs.

## Data directory (owner actions, outside this repo)

- Delete `.thumbs/` once: full-size entries written before the thumbnail fix are never evicted.
- Add `.publicator/` to the data repository's ignore rules before the first publishing receipt lands there.

## Backlog

- **Platform-independent scheduling.** Trigger `nix run .#da-publish -- --all --headless` from an external scheduler (e.g. a GitHub Actions cron) instead of paying for DeviantArt's premium scheduling. Not started; the runner needs the human-made PerimeterX login profile. Does not authorize live scheduling.
