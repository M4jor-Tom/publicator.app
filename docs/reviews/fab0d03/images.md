# Artwork identity, destructive paths and codecs

Contract: `docs/features/images-thumbnails.md`. These findings concern current source behavior,
not observed real-artwork loss. Media and filesystem tests must use disposable roots only.

## ASSET-05 — P2 · Risk / decision · Multi-frame detection concatenates format tokens

**Evidence:** `src/images.rs:309–335` runs `identify -format %m` over the whole sequence and compares
the entire output to one token. Expected two-frame GIF output is `GIFGIF`; audit execution did not
confirm it. Single-frame format tests (`tests/images.rs:324–351`) cannot resolve the issue.

**Trigger/impact to probe:** Valid animated GIF/WebP or multipage TIFF may fail preview, AI or
upload admission accidentally rather than follow an explicit sequence policy.

**Regression / close when:** Create two colored 8×8 frames/pages and run detection, preparation and
upload validation. Record actual output; define accepted formats and first-frame/animation/rejection
behavior per consumer, then assert it. Do not claim a runtime codec failure until this probe runs.
Keep allocations tiny; an arbitrary invalid `.mp4` fixture is not a valid-media rejection test.
