---
name: publish-deviantart
description: Publish or schedule the next unpublished art piece to DeviantArt. Try the automated app first; if it exits non-zero, reconcile any UNRESOLVED attempt before anything else, and finish by hand in the #login Firefox only after a safe failure, using the pw-daemon Chromium only to probe the DOM.
---

Every command reads its **publication database** (`publications.json` + images +
the `.deviantart-session/` profile) from `--data-dir <dir>`, defaulting to the
current working directory. Run the commands from the Art data dir and omit the
flag, or run them from anywhere and pass `--data-dir <path-to-art-data-dir>`
(for nix apps, after `--`: `nix run ... -- --data-dir <dir>`).

## First: try the automated app

```sh
nix run .#da-publish -- --data-dir <path-to-art-data-dir>
```

Publishes ONE entry — the first `DESCRIBED` publication with an unpublished
DeviantArt apparition in `publications.json` (add `--uuid <id>` to target a
specific one). If it exits 0, the publication is done — stop here.

## If it exits non-zero: classify before any manual step

The summary line is `published: N, failed: N, unknown: N, unattempted: N`.

**Unknown outcome — reconcile, never redo.** stderr lists one
`UNRESOLVED <attempt> (<uuid>): <reason>` line per attempt (after `unknown: 1` or
more, or when a later run refuses with `reconciliation required`). The submit may
have reached DeviantArt, so do not resubmit or redo it by hand and do not rerun the
automation. Inspect DeviantArt first (scheduled deviations, submissions, drafts),
then record what you found with the command printed under that line:

```sh
# it is on DeviantArt:
nix run .#da-publish -- --data-dir <dir> --reconcile-attempt <attempt> --outcome scheduled [--url <deviation-url>]
# it is absent:
nix run .#da-publish -- --data-dir <dir> --reconcile-attempt <attempt> --outcome not-submitted
```

Retry the automation or follow the manual steps only after `--outcome not-submitted`.
Never delete `.publicator/` receipts to unblock a run.

**Busy.** `a publish batch or reconciliation is active` means another run holds the
publisher lock: wait for it; do not publish by hand meanwhile.

**Safe failure — manual fallback allowed.** `unknown: 0` and no `UNRESOLVED` line
(e.g. `failed: 1`, or an error before any submission): nothing was submitted. Check
DeviantArt for a leftover draft, finish the current publication with the manual
steps below, then update `python/publicator_da/browser.py` to cover whatever tripped
it (so the next worker run doesn't fall through).

## Values for the current publication

`<pub.path>` / `<pub.title>` / `<pub.schedule>` come from the first
metadata-approved publication with an unpublished DeviantArt apparition in
`publications.json` (stdlib-only, no app needed):

```sh
nix run <publicator.py>#echo-first -- --data-dir .
```

If `echo-first` refuses because the artwork `no longer matches its recorded content`,
the file changed since it was picked and the unresolved-attempt steps do not apply:
stop and ask the owner; never upload another file.

## Manual fallback (safe failures only): finish in the #login Firefox, probe with the daemon

```sh
nix run .#pw-daemon
```

The daemon's Chromium (persistent context in `.deviantart-session/`) is NOT logged
in: that dir holds the Firefox profile `publish_batch` copies from `.deviantart-login/`,
which Chromium cannot read, and PerimeterX blocks every Playwright browser at the
login page. Use it to probe the DOM (find the label a step now times out on). Do the
steps below by hand in the real, signed-in Firefox: `nix run .#login` opens it on
the `.deviantart-login/` profile.

On startup the daemon creates a fresh private `pw-XXXXXXXX` directory (mode 0700)
in the system temp dir and prints its command FIFO (`.../pw.cmd`), output log
(`.../pw.log`) and screenshot (`.../pw.png`) paths; it refuses to start if any of
them already exists. Scope has `page`, `context`, `p`. Send a command chunk to the
printed FIFO, e.g.:

```sh
printf '%s' 'page.goto("https://www.deviantart.com")' > <printed pw.cmd path>
```

### Steps

1. Connect to www.deviantart.com
2. Click on submit
3. Click on "upload your art" and pick file `<pub.path>`
4. Set as title `<pub.title>`
5. Set as description `<pub.description>`
6. Tick boxes "Mature" and "Created using AI tools"
7. Drop all the pre-filled tags in the "Tags" field
8. Copy the content of the tags file (`<data-dir>/<"tags" path>`, per the top-level "tags" key of publicator.toml) into the "Tags" field
9. If the piece has a price, tick "Sell as Premium Download" and set the price
10. Set the subscription tiers `<pub.tiers>`
11. Add to galleries `<pub.galleries>`
12. Schedule publication for the `<pub.schedule>`
