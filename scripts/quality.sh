#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
# Tests create their own repositories; a caller's explicit Git paths must not leak.
unset $(git rev-parse --local-env-vars)

status=0
check() {
    printf '\n== %s ==\n' "$1"
    shift
    "$@" || status=1
}

check 'Python tests (gates, worker, browser)' env PYTHONPATH=python pytest tests/python -q
check 'Rust formatting' cargo fmt --all --check
check 'Clippy (warnings are errors)' cargo clippy --locked --all-targets --features desktop -- -D warnings
check 'Rust tests (including desktop)' cargo test --locked --all-targets --features desktop
# Its own target dir: headless tests must never see executables left by desktop builds.
check 'Rust tests (headless)' env CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}/headless" \
    cargo test --locked --all-targets --no-default-features
check 'Rust complexity and duplication' bash scripts/check-metrics.sh

if (( status != 0 )); then
    echo 'Quality checks failed. Fix the findings above; no historical baseline is exempt.' >&2
fi
exit "$status"
