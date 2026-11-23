# docs/

Read the owning feature before changing code. Each Gherkin spec describes behavior. [The feature index](features/index.toml) lists its code, tests, and run/verify commands under the `.feature` filename without its extension. Use `verify` for offline validation; `run` can operate on real data or services.

## Features

- [Configuration](features/config.feature) — data-directory configuration and validation.
- [Publications store](features/publications-store.feature) — JSON data contract, locks, atomic writes and recovery state.
- [Images and thumbnails](features/images-thumbnails.feature) — candidate discovery, cache and upload preparation.
- [Gallery UI](features/gallery-ui.feature) — native Iced Pick / Schedule / Describe / Publish workflow.
- [AI metadata](features/ai-metadata.feature) — OpenRouter and local Ollama generation and consent.
- [Scheduling](features/scheduling.feature) — timezone-aware dates and capacity.
- [Calendar](features/calendar.feature) — read-only native history and apparition navigation.
- [Prompt mapping](features/prompt-mapping.feature) — content-addressed Git archive mapping.
- [DeviantArt publishing](features/deviantart-publish.feature) — Rust orchestration and retained Python Playwright boundary.
- [Factory model routing](features/factory-model-routing.feature) — shared tiers and Pi model selection.

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

- `pre-commit` runs `nix flake check -L` on the checkout.
- `post-commit` starts `.githooks/quality-gate` in the background: `quality` and
  `native-acceptance` on the new commit, in a private worktree
  (`.git/quality-gate/worktree`), so later edits cannot disguise failures in the
  commit. A new commit cancels the previous commit's gate. The verdict
  (`running`, `ok`, `failed N`, `cancelled`) and log are
  `.git/quality-gate/<sha>[.log]`.
- `pre-push` runs no checks: it accepts only commits whose gate passed, so the
  remote connection never waits on them. Deletion-only pushes pass; tags are
  checked through the commit they point at. To gate a commit made without the
  hook (a fast-forward merge, for example), run `.githooks/post-commit [COMMIT]`.
- All three skip `factory/*` branches: the software factory gates them with
  `factory/checks.sh`, which runs the same checks (see AGENTS.md).

Hook setup is repository-local, never global. CI does not depend on hook setup
and always runs every check.

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
- Update feature docs, tests and code together. `tests/docs.rs` checks concrete paths in maintained Markdown, validates the feature index, and parses every Gherkin spec. Scenarios document behavior; the indexed tests exercise it. Markdown glob/placeholders, ROADMAP and historical ADR paths are intentionally excluded.
- Add architecture decisions with the next ADR number; mark superseded decisions without erasing history.
- Plans, execution reports, screenshots and temporary fixtures belong in `/tmp`, not Git.
