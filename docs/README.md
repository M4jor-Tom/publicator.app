# docs/

Feature maintenance docs live in `features/`. Each describes Does / Run / Code / Tests / Config / Data / Decisions / Verify, then flow, invariants, and operational gotchas. Read the owning feature before changing code.

## Features

- [Configuration](features/config.md) — data-directory configuration and validation.
- [Publications store](features/publications-store.md) — JSON data contract, locks, atomic writes and recovery state.
- [Images and thumbnails](features/images-thumbnails.md) — candidate discovery, cache and upload preparation.
- [Gallery UI](features/gallery-ui.md) — native Iced Pick / Schedule / Describe / Publish workflow.
- [AI metadata](features/ai-metadata.md) — OpenRouter and local Ollama generation and consent.
- [Scheduling](features/scheduling.md) — timezone-aware dates and capacity.
- [Calendar](features/calendar.md) — read-only native history and apparition navigation.
- [Prompt mapping](features/prompt-mapping.md) — content-addressed Git archive mapping.
- [DeviantArt publishing](features/deviantart-publish.md) — Rust orchestration and retained Python Playwright boundary.

## Architecture decisions

- [0009 Separate interfaces, application operations and Store](adr/0009-separate-interfaces-application-operations-and-store.md) — historical Python/HTTP decision, superseded for the application interface by:
- [0010 Rust core and native Iced interface](adr/0010-rust-core-and-native-iced-interface.md).

Feature-specific behavior belongs with its owner. Preserve historical ADR context; do not treat it as current runtime guidance. [ROADMAP](ROADMAP.md) lists open work only.

## Rust quality checks

Run `nix run .#quality` from this checkout. The same command runs on every GitHub
push and pull request. No Sonar account, server, container, or analysis history is
needed. Nix pins Rustfmt, Clippy and rust-code-analysis through `flake.lock`, and
jscpd through a versioned, hash-verified upstream static binary in `flake.nix`.

The command runs every pytest test under `tests/python` (gate regression, worker
and browser tests; no live browser or service), formatting, Clippy with warnings as
errors, the Rust test suite with the `desktop` feature, the headless
(`--no-default-features`) suite in its own `CARGO_TARGET_DIR` subdirectory `headless`,
and complexity/duplication analysis. It runs every check even if an earlier check fails.
Rust files under `src/`, `tests/`, and `examples/` are scanned, including inline tests;
build output and external publication data are outside these roots.

Pre-push and CI run three gates: `nix run .#quality`, `nix run .#native-acceptance`
(see [testing](testing.md)) and `nix flake check -L`. The hook runs all three even after a
failure and fails with the first failing status; CI runs each as its own job.

`nix flake check -L` builds the CLI and desktop packages and runs
`checks.x86_64-linux.package-smoke`. From an unrelated temporary directory with an empty
environment, that check runs the wrapped `publicator validate` and `check-steps` and
`publicator-ui --help`. It also has the desktop wrapper's own `xdg-open` hand a URL to a fake
`$BROWSER`, and requires the unwrapped binary's `check-steps` to fail because its assets are missing.

- Cognitive complexity: maximum **15 per function/closure**.
- Cyclomatic complexity: maximum **10 per function/closure**.
- Duplication: **no repeated blocks of at least 100 tokens and 10 lines**;
  comments and whitespace are ignored, identifiers/literals are not normalized.
- Existing violations fail too. There are no baseline exemptions or blanket
  suppressions. These tools do not implement every Sonar rule or prove absence
  of defects. Rustfmt/Clippy/tests cover compilation and behavior; the metrics
  analyzer works on syntax, not macro-expanded code.

Activate the checked-in hook once per clone (first check whether you already use
another hooks directory):

```sh
git config --get core.hooksPath
git config --local core.hooksPath .githooks
```

The pre-push hook requires a clean working tree and checks pushes of the current
HEAD only, so uncommitted fixes cannot disguise failures in the commit being
pushed. Check out other branches before pushing them. Deletion-only pushes skip
checks; pushes of other objects, including annotated tag objects, are rejected.
Hook setup is repository-local, never global. CI does not depend on hook setup.

To inspect only duplication and complexity: `nix develop -c bash scripts/check-metrics.sh`.
To test the gates: `nix develop -c python -m unittest discover -s tests/python -p test_quality.py -v`.
Failures print findings in the terminal/Actions log; temporary analyzer JSON is
removed on exit. Fix existing findings rather than raising thresholds to make
an initial run pass.

[Behaviour-to-gate map](testing.md) names the check (automated or supervised) behind each
publishing safety transition and the paths no check measures.

## Maintaining

- Document each symbol in the feature that owns its implementation; other files link there.
- `src/` and `assets/publicationsSchema.json` are Rust application/code assets. `python/publicator_da/` is browser-only. The publishing skill remains a static manual/recovery contract.
- Update feature docs, tests and code together. `tests/docs.rs` checks concrete paths in maintained docs; glob/placeholders, ROADMAP and historical ADR paths are intentionally excluded.
- Add architecture decisions with the next ADR number; mark superseded decisions without erasing history.
- Plans, execution reports, screenshots and temporary fixtures belong in `/tmp`, not Git.
