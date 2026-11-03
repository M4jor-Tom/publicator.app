# Test gates, packaging and fixture safety

Recorded execution is in [validation](validation.md). Passing suites do not discharge the gaps
below. META-02 owns false-green provider assertions; UI-C01 owns discarded lifecycle actions.

## VER-03 — P2 · Gap / risk · Headless and real packaged runtime are not gated

Also incorporates **UI-R03**; no separate package issue needs to be maintained.

**Evidence:** `scripts/quality.sh:18–20` always enables desktop; Cargo defaults are empty
(`Cargo.toml:6–9`). `flake.nix:33–85,126–145,178–199` has no checks output and both packages set
doCheck=false. `tests/packaging.rs:6–66` executes Cargo binaries with copied/injected assets and
checkout PYTHONPATH, not the Nix wrappers. A dev shell can mask missing runtime dependencies.
Desktop URL opening calls xdg-open (`src/ui/calendar.rs:426–439`), not explicitly supplied by
rustRuntime (`flake.nix:48,91–95`); this is unverified packaging risk, not a proven missing binary.

**Regression / close when:** Independently gate no-default-features with a fresh target directory;
desktop-only CLI tests cannot rely on stale UI executables. Build CLI/desktop packages, then from
an unrelated temporary CWD with controlled PATH and no checkout PUBLICATOR_WORKER/PUBLICATOR_SKILL/
PYTHONPATH, exercise embedded-schema validation, packaged check-steps and UI help. Capture a harmless
URL-opener request without opening a real browser. A deliberately missing runtime asset must fail.
Keep GPU feature validation separate. Do not count an empty flake checks set or successful Cargo
asset injection as proof of the installed closure. No package build ran during this audit.
