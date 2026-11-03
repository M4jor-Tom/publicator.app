# Application and AI metadata

Contract: `docs/features/ai-metadata.md`. Use injected loopback endpoints, dummy keys and synthetic
images; no real model, installation or provider call.

## META-02 — P2 · Reproduced · Malformed-response test passes on a fixture failure

**Evidence:** `tests/local_metadata.rs:130–159` scripts tags then chat, but direct `local_generate`
sends chat (`src/metadata/local.rs:150–179`). The fixture panics at `tests/common/http.rs:56`;
Drop ignores join failure (`96–100`), and generic `is_err()` accepts the transport error.

**Observed:** The focused test exited 0 with one pass while two fixture threads panicked:
actual `/api/chat`, expected `/api/tags`. Intended malformed/empty replies never arrived.
Exact command and witness are recorded in [validation](validation.md).

**Existing coverage:** Successful chat (`tests/local_metadata.rs:87`) and shared parsers
(`tests/metadata.rs`) provide partial protection, not proof of this provider-specific test.

**Regression / close when:** Script only chat, assert its request body/path, account for response
delivery, require the specific parse/validation failure, and finish the fixture explicitly.
Separately inject an unexpected request and require a visible test failure. Cleanup must remain
bounded and avoid double panic during unwinding. Coordinate fixture shutdown with VER-05.

## META-C02 — P2 · Gap · Composed provider budgets and local error replies need proof

**Evidence:** `src/metadata/local.rs:94–105,190–222` and
`src/metadata/openrouter.rs:57–95,189–225` implement deadlines. Existing tests cover one OpenRouter
body timeout, schema retry/fallback order and HTTP-500 pull failure, not the following sequences.
This is not a claim that the currently timed network paths exceed their budgets.

**Regression / close when:** Gate requests instead of relying solely on sleeps:
- OpenRouter selected failure → catalogue → multiple failures uses one budget; no post-expiry call.
- Local fit → installed tags → chat headers with withheld body ends boundedly after request capture.
- Consented pull returning HTTP-200 `{"status":"failed"}` never advances to chat.
- Tags returning HTTP-200 without `models` fail as protocol errors and leave publication bytes intact.
- Fake llmfit signals entry, blocks, then is terminated/reaped by the deadline.
Preserve explicit installation consent and loopback-only dependencies.

## META-C04 — P2 · Gap · Wire-level media and catalogue filtering need composition tests

**Evidence:** Provider filtering validates every advertised price component and image capability
(`src/metadata/openrouter.rs:127–225,241–270`), retaining the attempted ID rather than a response
alias. `tests/openrouter.rs:50,69,113,149,228,261` covers individual choices; image tests cover
encoding separately (`tests/metadata.rs:80`, `tests/images.rs:215,324,354`).

**Regression / close when:** Drive the application with a real hashed image, capture its request,
decode the transmitted media and check MIME, dimensions and representative pixels. Traverse a
mixed catalogue containing free, paid, nonvision, duplicate and failing candidates; assert exact
attempt order, no paid/nonvision call, no duplicate retry and correct stored model provenance.
Combine with the shared deadline witness, not another fake all-success provider.
AI acceptance of WebP is distinct from DeviantArt's prohibition on uploading WebP.
