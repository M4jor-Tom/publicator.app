# Prompt mapping

- **Does:** Resolves an image identity against every archived prompt Git blob as Exact, Nearest or Unknown; supports prompt search and audit.
- **Run:** `nix run .#prompt-audit -- --data-dir DIR`; use prompt search in the native UI.
- **Code:** `src/prompts.rs`, `src/config.rs`, `src/ui/`, `src/bin/publicator.rs`.
- **Tests:** `tests/prompts.rs`, `tests/ui_workflow.rs`, `tests/docs.rs`.
- **Config:** Data-dir `[prompts]` declares repository, regex with required `version` and optional `lineage` captures, and digest algorithm.
- **Data:** Prompt Git repo and image files live in the data directory; archive is read-only to Publicator.
- **Decisions:** Digest all Git blobs, including historical/unreachable content; Nearest renders candidate prompt text as non-authoritative, never as Exact. No basename-at-HEAD lookup.
- **Verify:** `nix develop -c cargo test --test prompts --test ui_workflow`
- **Verify:** `nix run .#prompt-audit -- --data-dir DIR`

## How it works

The configured filename grammar maps image names to version digest and optional lineage. Exact digest hits identify immutable text. If the digest is absent, lineage can rank archived candidates as Nearest, explicitly not authoritative. Search is exact-text-only; missing optional archive disables the feature without guessing a grammar.

The search limit bounds *available* results returned (Exact first, then lineage Maybe hits), not archive matches scanned: matches already referenced by a publication, unreadable files and names outside the grammar never consume it. Hashing stops one available result past the limit; "truncated" means at least that one more available result exists.

Freshness: the in-memory index is rebuilt automatically only when HEAD changes (probed at most every 2 seconds). New unreachable blobs, commits on other refs and pruned objects appear after `PromptArchive::invalidate`, which makes the next resolve rebuild unconditionally; the UI Refresh action calls it. There is no CLI flag: each CLI run builds a fresh index. Staleness can only mean "not found yet" (or a pruned blob still listed with its true text), never a wrong Exact, because digests are content-addressed.

The data repository's prompt archive is a separate concern: the image producer attempts to commit prompt content before minting its identity. Publicator does not edit that repository.

## Invariants

- `Nearest` candidates are rendered text but never authoritative: shown under a "not archived / not authoritative" warning, visually separated from the Exact style, with every candidate line labelled by its lineage path. Candidate text never becomes Exact provenance.
- Digest is computed from prompt blob content using the configured algorithm, not Git object ID.
- Lineage paths come from NUL-delimited Git history and round-trip verbatim (quotes, tabs, newlines, backslashes); archive Git commands ignore inherited `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE`.
- Filename shape and digest choice are configuration; no `sha1` or delimiter convention is hardcoded in Rust.
- Archive failures are visible diagnostics; an optional archive outage does not turn into false exact matches.
- A rebuild replaces the index in one assignment: a failed or unavailable rebuild leaves the previous index whole, resolves report `Unknown("prompt archive unavailable: …")`, and the next resolve retries.

## Gotchas

- Grammar drift appears as falling parsed counts; fix the data-dir configuration, not a code fallback.
- Missing archive and malformed archive/configuration are distinct conditions.
- Archive size ceiling: a build buffers the whole `git cat-file --batch-all-objects` stream and copies blob bodies; a rebuild also keeps the old index alive until the swap. Measured with `prompt-audit` (first build, debug binary, 1 MiB text blobs): 0 / 25 / 50 / 100 MiB of blobs peak at 57 / 108 / 158 / 258 MiB RSS, about 2x object bytes over the baseline. The accepted ceiling is about 100 MB of object content; beyond it, stream cat-file instead of raising the ceiling.
