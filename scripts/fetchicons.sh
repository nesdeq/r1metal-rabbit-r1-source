#!/usr/bin/env bash
# Takes every icon of Google's Material Symbols Rounded at 48 px into crates/funcore/assets/material and every icon of the older Material Icons Round at 24 px into crates/funcore/assets/material-icons, each file as Google serves it, with the license.
set -euo pipefail
cd "$(dirname "$0")/.."

FAMILY="Material Symbols Rounded"
STYLE=materialsymbolsrounded
SIZE=48px
LIST='https://fonts.google.com/metadata/icons?key=material_symbols&incomplete=true'
ICONS=https://fonts.gstatic.com/s/i/short-term/release/$STYLE
TO=crates/funcore/assets/material
OLDER_FAMILY="Material Icons Round"
OLDER_STYLE=materialiconsround
OLDER_SIZE=24px
OLDER_LIST='https://fonts.google.com/metadata/icons'
OLDER_ICONS=https://fonts.gstatic.com/s/i/$OLDER_STYLE
OLDER_TO=crates/funcore/assets/material-icons
LICENSE=https://raw.githubusercontent.com/google/material-design-icons/master/LICENSE

for tool in curl jq xargs nproc; do
    command -v "$tool" > /dev/null || { echo "missing: $tool" >&2; exit 1; }
done
fresh=$(mktemp -d)
trap 'rm -rf "$fresh"' EXIT
mkdir "$fresh/symbols" "$fresh/icons"
# Google's lists begin with a line that keeps them from being run as a script; an older icon's file is served by its version.
curl -fsSL "$LIST" | sed 1d \
    | jq -r --arg family "$FAMILY" '.icons[] | select(.unsupported_families | index($family) | not) | .name' \
    > "$fresh/names"
curl -fsSL "$OLDER_LIST" | sed 1d \
    | jq -r --arg family "$OLDER_FAMILY" '.icons[] | select(.unsupported_families | index($family) | not) | "\(.name) \(.version)"' \
    > "$fresh/older"
export ICONS SIZE OLDER_ICONS OLDER_SIZE fresh
xargs -P "$(nproc)" -I '{}' sh -c 'curl -fsS --retry 3 -o "$fresh/symbols/$1.svg" "$ICONS/$1/default/$SIZE.svg"' _ '{}' \
    < "$fresh/names"
xargs -P "$(nproc)" -L 1 sh -c 'curl -fsS --retry 3 -o "$fresh/icons/$0.svg" "$OLDER_ICONS/$0/v$1/$OLDER_SIZE.svg"' \
    < "$fresh/older"
curl -fsSL -o "$fresh/LICENSE" "$LICENSE"
mkdir -p "$TO" "$OLDER_TO"
rm -f "$TO"/*.svg "$OLDER_TO"/*.svg
mv "$fresh"/symbols/*.svg "$TO"/
mv "$fresh"/icons/*.svg "$OLDER_TO"/
cp "$fresh/LICENSE" "$TO"/
cp "$fresh/LICENSE" "$OLDER_TO"/
echo "$(wc -l < "$fresh/names") icons of $FAMILY in $TO, $(wc -l < "$fresh/older") of $OLDER_FAMILY in $OLDER_TO"
