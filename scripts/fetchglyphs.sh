#!/usr/bin/env bash
# Takes what the screen draws of the characters Spleen lacks into crates/funcore/assets, each with its font's license: GNU Unifont's drawings (unifont/glyphs.bin) and Noto Emoji's pictures with the flags it carries (noto/emoji.bin), as crates/funcore/examples/glyphs.rs writes them.
set -euo pipefail
export LC_ALL=C
cd "$(dirname "$0")/.."

UNIFONT_VERSION=18.0.01
UNIFONT=https://unifoundry.com/pub/unifont/unifont-$UNIFONT_VERSION
EMOJI=https://www.unicode.org/Public/18.0.0/ucd/emoji/emoji-data.txt
NOTO=https://github.com/googlefonts/noto-emoji.git
NOTO_VERSION=v2026-09-24-unicode18_0
TO=crates/funcore/assets

for tool in curl gunzip tar git cargo; do
    command -v "$tool" > /dev/null || { echo "missing: $tool" >&2; exit 1; }
done
fresh=$(mktemp -d)
trap 'rm -rf "$fresh"' EXIT

curl -fsSL "$UNIFONT/font-builds/unifont_all-$UNIFONT_VERSION.hex.gz" | gunzip > "$fresh/unifont.hex"
curl -fsSL "$EMOJI" > "$fresh/emoji-data.txt"
# Noto's pictures 72 pixels a side and the flags, without the rest of its tree.
git -c advice.detachedHead=false clone --quiet --depth 1 --filter=blob:none --sparse --branch "$NOTO_VERSION" "$NOTO" "$fresh/noto"
git -C "$fresh/noto" sparse-checkout set 2D/png/72 third_party/region-flags/png

mkdir -p "$TO/noto" "$TO/unifont"
cargo run --release --quiet -p r1-funcore --example glyphs -- "$fresh/unifont.hex" "$fresh/emoji-data.txt" "$fresh/noto" "$TO"
curl -fsSL "$UNIFONT/unifont-$UNIFONT_VERSION.tar.gz" | tar -xzO "unifont-$UNIFONT_VERSION/OFL-1.1.txt" > "$TO/unifont/OFL-1.1.txt"
cp "$fresh/noto/LICENSE" "$TO/noto/OFL-1.1.txt"
cp "$fresh/noto/third_party/region-flags/LICENSE" "$TO/noto/flags.txt"
