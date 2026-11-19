#!/usr/bin/env bash
# The repo's checks. Exit 0 only when the branch is fit to review.
#
# The gate runs this script from the main checkout with the job's worktree as
# the working directory. These are the same three gates as CI and the local
# hooks (AGENTS.md); Nix provides every dependency, so a fresh worktree needs
# no setup. Keep the list in step with .github/workflows/quality.yml and
# .githooks/quality-gate. Runs all three even after a failure, failing with the first status.
set -euo pipefail
# A caller's Git paths would point the worktree's commands at another checkout.
# shellcheck disable=SC2046 # one variable name per line
unset $(git rev-parse --local-env-vars)

status=0
for cmd in "nix flake check -L" "nix run .#quality" "nix run .#native-acceptance"; do
    printf '\n=== %s ===\n' "$cmd"
    $cmd || { code=$?; echo "checks.sh: $cmd failed ($code)"; ((status)) || status=$code; }
done
exit "$status"
