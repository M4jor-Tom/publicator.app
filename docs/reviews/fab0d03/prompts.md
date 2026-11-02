# Prompt archive and rendering

Contract: `docs/features/prompt-mapping.md`. Existing tests cover historical/deleted/duplicate paths,
unreachable blobs present before construction, content-digest identity and concurrent readers.
All proposed repositories are disposable; never mutate the real prompt archive.

## ASSET-06 — P2 · Decision · Refresh invalidation does not cover the indexed universe

**Evidence:** `src/prompts.rs:197–207` skips rebuilding when HEAD is unchanged, but the index reads
all objects and refs (`222–241`). After warm-up, new unreachable blobs and non-HEAD branch changes
stay stale beyond the two-second probe TTL. Confidence: high in source behavior.

**Qualification:** HEAD-based refresh is itself documented historical behavior. This needs an
explicit freshness contract, not an invented guarantee that every new object is already promised
immediate visibility.

**Existing coverage:** `tests/prompts.rs:231–254` changes HEAD; `289–324` writes the unreachable blob
before constructing the archive. Neither covers post-warm-up object/ref changes.

**Regression / close when:** Warm a committed archive, add a blob with unchanged HEAD, age the probe
without a sleep where private test access permits, then resolve it. Repeat with another-ref update
and object removal. Decide whether automatic invalidation covers that universe or a documented
explicit refresh is required; implement/test the chosen guarantee consistently with the owner docs.
Do not remove all-object/unreachable lookup merely to make the mismatch disappear accidentally.

## ASSET-09 — P2 · Decision · Nearest text contradicts the maintained no-render contract

**Evidence:** `docs/features/prompt-mapping.md:9,21` says Nearest has no renderable prompt text.
`src/prompts.rs:342–354` supplies full versions and `src/ui/prompt.rs:42–49` renders their bodies.
The UI has a non-authoritative warning; it does **not** falsely label these Exact.

**Existing coverage:** `tests/prompts.rs:181–208` asserts Nearest candidate contents;
`src/ui/prompt.rs:59–87` checks warning/paths but not suppression of the bodies.

**Regression / close when:** Decide the intended product contract. If no-render remains the rule,
use a sentinel body and assert Nearest shows only warning/lineage while Exact still shows text.
If candidate bodies are intentionally supported, update maintained docs and assert unmistakable
non-authority plus separation from Exact. Use pure in-memory matches; do not silently turn candidate
text into authoritative provenance. No code change is authorized until the policy is resolved.

