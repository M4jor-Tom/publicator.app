#!/usr/bin/env bash
# Writes a disposable Publicator data dir (config, tags, prompt archive, publications, art) for UI testing.
# usage: scripts/dummy-data.sh [DIR]   (default: a fresh /tmp/publicator-dummy.XXXXXX)
# DIR must be absent or an empty directory; anything else is refused before any tool runs.
# PUBLICATOR_DUMMY_NOW (Unix seconds) anchors every timestamp; the fixed default keeps runs repeatable.
# Data only: `publicator-ui` on it still uses real provider endpoints and the real worker; for
# injected endpoints/workers use examples/ui_acceptance.rs. Never point it at real Art data.
set -euo pipefail

now=${PUBLICATOR_DUMMY_NOW:-1799150400} # 2027-01-05T12:00:00Z
[[ $now =~ ^[0-9]+$ ]] || { echo "PUBLICATOR_DUMMY_NOW must be Unix seconds, got: $now" >&2; exit 2; }
if [[ -n ${1:-} ]]; then
  dir=$1
  # Never reuse saved state: a symlink, file or nonempty directory may be real data.
  if [[ -L $dir || ( -e $dir && ( ! -d $dir || -n $(ls -A -- "$dir") ) ) ]]; then
    echo "Refusing to write into existing $dir; pass an absent path or an empty directory." >&2
    exit 1
  fi
else
  dir=$(mktemp -d /tmp/publicator-dummy.XXXXXX)
fi
mkdir -p "$dir/picked" "$dir/published"
day=86400

art() { # art FILE SEED WIDTH HEIGHT
  # Plasma at quarter size, scaled up: same look, ~16x less generation work.
  magick -seed "$2" -size "$(($3 / 4))x$(($4 / 4))" plasma:fractal -resize "$3x$4!" -blur 0x2 "$1"
}

# The prompt archive is its own repository: no caller Git context, config or hooks leak in.
while read -r name; do
  unset "$name"
done < <(git rev-parse --local-env-vars)
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_DATE="$now +0000" GIT_COMMITTER_DATE="$now +0000"
git init -q "$dir/prompts"
printf 'Plasma dunes at noon, soft blur' >"$dir/prompts/dunes.txt"
printf 'Plasma fog over a radio mast' >"$dir/prompts/fog.txt"
git -C "$dir/prompts" add -A
git -C "$dir/prompts" -c user.name=Dummy -c user.email=dummy@example.invalid commit -qm 'Dummy prompts'
identity() { # identity LINEAGE <TEXT -> basename matching the [prompts] grammar below
  printf '%s_%s.png' "$1" "$(sha256sum | cut -d' ' -f1)"
}

# Candidates 1-2 resolve Exact, 3 Nearest (archived lineage, unarchived text), the rest Unknown.
candidates=(
  "$(identity dunes.txt <"$dir/prompts/dunes.txt")"
  "$(identity fog.txt <"$dir/prompts/fog.txt")"
  "$(identity fog.txt <<<'An unarchived fog draft')"
)
for i in 4 5 6 7 8; do candidates+=("candidate-$i.png"); done
for i in "${!candidates[@]}"; do
  art "$dir/picked/${candidates[i]}" "$((i + 1))" $((480 + (i + 1) % 3 * 160)) $((640 - (i + 1) % 2 * 240))
done

printf 'abstract\nplasma\ndigital art\n' >"$dir/tags.txt"
cat >"$dir/publicator.toml" <<'EOF'
publicable = ["picked"]
tags = "tags.txt"

[deviantart]
tiers = ["Supporter", "Patron"]
galleries = ["Featured", "Landscapes", "Portraits"]

[prompts]
repo = "prompts"
filename = '(?P<lineage>[^_]+)_(?P<version>[a-f0-9]{64})\.png'
version_hash = "sha256"

[schedule]
timezone = "Europe/Paris"

[[schedule.profiles]]
name = "Tuesday evening"
day = "tuesday"
hour = 20
per_slot = 2

[[schedule.profiles]]
name = "Saturday morning"
day = "saturday"
hour = 10
per_slot = 1
EOF

marker='[[PUBLICATOR:TODO_GENERATE]]'
rows=()
row() { # row N STATE TITLE DESCRIPTION OFFSET_DAYS APPARITIONS
  local name="art-$1.png"
  art "$dir/published/$name" "$((100 + $1))" 600 $((400 + $1 % 3 * 200))
  rows+=("$(jq -n --arg uuid "$(printf '00000000-0000-4000-8000-%012d' "$1")" \
    --arg state "$2" --arg title "$3" --arg description "$4" --arg name "$name" \
    --arg sha "$(sha512sum "$dir/published/$name" | cut -d' ' -f1)" \
    --argjson submitted "$((now - 30 * day))" --argjson at "$((now + $5 * day))" \
    --argjson apparitions "$6" \
    '{uuid:$uuid, state:$state, title:$title, description:$description,
      submissionTimestamp:$submitted, apparitionTimestampIfDifferentThanSubmission:$at,
      files:[{basename:$name, sha512sum:$sha}], apparitions:$apparitions}')")
}
draft='[{"platformName":"deviantart","state":"unpublished","tiers":["Supporter"],"galleries":["Featured"]}]'
live() { printf '[{"platformName":"deviantart","state":"published_or_scheduled","url":"https://example.test/art/%s"}]' "$1"; }

row 1 APPEARED "Harbor at dusk" "Warm light over the old harbor." -24 "$(live 1)"
row 2 APPEARED "Glass forest" "Refraction study." -17 "$(live 2)"
row 3 APPEARED "Night market" "Neon and rain." -3 "$(live 3)"
row 4 DESCRIBED "Copper dunes" "Desert at noon." 4 "$draft"
row 5 DESCRIBED "Tidal bloom" "Algae under moonlight." 8 "$draft"
row 6 SCHEDULED "Signal tower" "Radio mast in fog." 11 '[]'
row 7 SCHEDULED "$marker" "$marker" 15 '[]'
row 8 SCHEDULED "Paper moons" "" 18 '[]'
row 9 PICKED "" "" 22 '[]'
row 10 PICKED "" "" 25 '[]'

printf '%s\n' "${rows[@]}" | jq -s . >"$dir/publications.json"
echo "$dir"
