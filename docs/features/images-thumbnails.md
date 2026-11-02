# Images and thumbnails

- **Does:** Finds unpublished candidates, deduplicates by content identity, caches resized previews and prepares DeviantArt-compatible uploads.
- **Run:** `nix run .#ui -- --data-dir DIR`; `nix run .#da-publish -- --data-dir DIR`.
- **Code:** `src/images.rs`, `src/ui/`, `src/publishing.rs`, `src/store.rs`.
- **Tests:** `tests/images.rs`, `tests/prompt_search.rs`, `tests/ui*.rs`, `tests/publishing.rs`.
- **Config:** `publicable` image roots are configured in the data-dir `publicator.toml`.
- **Data:** `.thumbs/` is a persistent content-addressed cache; `.publicator/` holds publish staging. Both are data-dir runtime state, not source assets.
- **Decisions:** Generate previews lazily; never upload WebP. Data repo should ignore `.thumbs/` and `.publicator/`.
- **Verify:** `nix develop -c cargo test --test images --test publishing`

## How it works

Pick scans only configured roots and excludes already-published file identities. Native previews are prepared as needed and reused from `.thumbs/`. Publishing prepares a supported upload in `.publicator/` before dispatching the Python worker; the worker receives one image job and does not access the database or config.

## Invariants

- Candidate selection is bounded to configured roots; it does not accept arbitrary paths.
- One reference policy (`images::References`) serves normal discovery, prompt search and deletion: a file is protected when its basename or its content hash (`sha512sum` or `fileSha512sum`, any case, in `files` or `previewFilesIfNotFree`) is recorded by any publication. A referenced basename stays protected after its bytes change.
- A search offers one candidate per content identity: byte-identical copies and files reached through overlapping roots collapse to a single candidate, matching Pick, which keys new rows by content hash.
- Cache entries are content-addressed and persist across runs; they are not temporary `/tmp` files.
- `ensure_thumb(path, cache, sha512)` creates an entry only from a private copy whose bytes hash to `sha512`; a source that changed since that digest was recorded is refused, so one identity's key never holds another identity's preview.
- Cache-entry policy: a decodable regular file is reused; corrupt bytes or a symlink are rebuilt and atomically replaced (a symlink's target is never written); a directory or other entry type at the name is an error and is never removed. Simultaneous creators each publish a complete entry by rename; no temporary file is left or treated as an entry.
- WebP is converted to PNG (alpha preserved) before DeviantArt upload. Conversion is chosen by detected content or a `.webp` suffix, so WebP bytes under another name are converted too; `validate_upload` rejects any upload whose content is still WebP.
- Deleting a candidate is identity-bound (Linux only; other platforms refuse): after the Store-locked reference check, the parent is reopened from the configured root without following symlinks and must be the authorized directory (device/inode); the entry is renamed to a private `.publicator-delete-<uuid>` name and unlinked only if it is still the authorized file. Anything else is renamed back without replacing a newer entry and the delete fails; if that restore fails, the error names the private path. A crash in that window leaves the file under the private name, which discovery ignores.
- Do not remove staging files or receipts needed by unresolved publication attempts.

## Gotchas

- A missing artwork file does not justify guessing a replacement from a similar basename.
- Cache/staging ignore rules belong in the separate data repository; this project does not edit Art.
