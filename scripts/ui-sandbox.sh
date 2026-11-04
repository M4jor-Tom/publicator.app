#!/usr/bin/env bash
set -euo pipefail

if (( $# > 1 )); then
  echo 'usage: ui-sandbox [SCENARIO]' >&2
  exit 2
fi

cd "$(dirname "${BASH_SOURCE[0]}")/.."
# nix develop removes its TMPDIR on exit; keep fixtures outside it for inspection.
fixture=$(mktemp -d /tmp/publicator-ui.XXXXXX)
printf 'Sandbox fixture (preserved): %s\n' "$fixture"
exec cargo run --locked --release --example ui_acceptance --features desktop -- \
  --fixture-dir "$fixture" --scenario "${1:-mixed}"
