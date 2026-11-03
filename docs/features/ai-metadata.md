# AI metadata

- **Does:** Generates a validated title/description pair through OpenRouter or local Ollama; only a complete pair advances a publication to DESCRIBED.
- **Run:** `nix run .#generate-meta -- --help`; or use the native `ui` Describe phase.
- **Code:** `src/metadata/`, `src/application.rs`, `src/artwork.rs`, `src/images.rs`, `src/bin/publicator.rs`, `src/bin/publicator-ui.rs`.
- **Tests:** `tests/metadata*.rs`, `tests/application_ai.rs`, `tests/cli.rs`, `tests/ui_describe.rs`.
- **Config:** Provider/model selection is explicit; local fit is assessed with llmfit. CLI-only `PUBLICATOR_OLLAMA_URL` overrides the Ollama endpoint.
- **Data:** `publications.json` stores title/description; `.thumbs/` holds AI input previews. Runtime credentials are read only when the selected provider needs them.
- **Decisions:** Local Ollama and OpenRouter only. No Claude CLI, Python `llm` bridge, automatic cross-provider fallback, or production fake mode.
- **Verify:** `nix develop -c cargo test --test metadata --test application_ai --test cli`

## How it works

All calls request both title and description and validate the response as a complete usable pair. UI acceptance saves the exact TODO marker in both fields on generation failure; the publication remains SCHEDULED and retryable. Generated results merge against fresh Store state so a newer manual edit is not overwritten: single generation snapshots the title/description it admitted, and its completion (generated pair or failure markers) replaces only fields still holding the admitted value, approving only when both still did. A draft saved before admission is therefore replaced; one saved by any writer during generation survives, unapproved. Batch admission validates the complete source document before any provider call; each successful record is persisted before processing the next.

OpenRouter requests use Rust HTTP clients and loopback fixtures in ordinary tests. Local generation uses an already-running Ollama service; Publicator does not start or install the service/model. `vision-models` reports local host fit. Install-on-use requires explicit consent in the GUI or CLI.

## Invariants

- A missing, blank or marker-bearing field cannot be approved or published.
- Provider work occurs outside Store locks; batch save merges only fields still carrying the exact marker, single save only fields still equal to their admitted value.
- Invalid source data/config fails before provider admission and leaves source bytes unchanged. Single and batch generation both validate and reserve their rows before the local preflight, so invalid source data runs no llmfit/Ollama check; a batch whose expected UUIDs differ, even only in order, is rejected the same way.
- The request timeout bounds the whole operation, not provider work alone: single generation's local preflight, AI image preparation and every provider request share one deadline; a batch gives each record its own deadline for preparation and provider work, and its one local preflight its own. Preparation runs off the async runtime from the bytes matching the resolved SHA-512, and its `identify`/`magick` children are killed with their process group and reaped at the deadline.
- AI input is the publication's recorded artwork: `files[0]` resolves to the visible file whose bytes match the stored SHA-512, whatever directory holds a same-named file. Changed bytes require reconciliation; no provider call or metadata is made from them.
- Ollama is unavailable when `/api/tags` is refused, times out or answers an HTTP error; a reply that arrives but is not tags JSON is a protocol error, reported as such and never as unavailability.
- Without an OpenRouter key (configured, else a nonempty `OPENROUTER_KEY`), OpenRouter generation fails with `OPENROUTER_KEY is not set` before any request; the model catalogue is never fetched unauthenticated.
- The `PUBLICATOR_OLLAMA_URL` override is CLI-only and selects a dependency endpoint; it is not a hidden test/fake switch.
- Tests use deterministic local/loopback fixtures and never require live provider credentials. Provider oracles (`tests/local_metadata.rs`, `tests/openrouter.rs`) hold a reply with the fixture gate instead of sleeping, assert the exact requests sent, and finish the fixture so an unconsumed reply or wrong path fails the test.

## Gotchas

- A complete source snapshot must validate before a batch starts; invalid rows are not reported as ordinary per-record provider failures.
- Local service/model installation has external effects and is outside routine validation.
- AI input may be WebP (sent as `image/webp`); DeviantArt's no-WebP rule applies only to uploads.
