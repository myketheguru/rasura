#!/usr/bin/env bash
# Fetch a real typeface for the glyph-injection tests.
#
# Everything else in the font tests runs against a font this library
# synthesised, which means it shares this library's assumptions about what a
# well-formed sfnt looks like. A typeface drawn by someone else does not: it has
# composite glyphs, hinting programs, a `post` table, `GSUB`, several `cmap`
# subtables and 2048 units per em. Injection has to survive all of that.
#
# Roboto, Apache-2.0 -- already on the spec 4.3 allowlist, so no licence
# exception is needed. Downloaded rather than vendored, in keeping with
# corpus/fetch.sh and harness/pixeldiff/fetch.sh: the repository carries no
# third-party binary and the licence stays with its author.
#
#   ./corpus/fetch-font.sh
set -euo pipefail

# Pinned to a commit, not a branch. A font revision can change glyph ids and
# advance widths, and the injection tests assert on both.
COMMIT="38062f4b4a0be4346d07a928408da21602545e9e"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/corpus/fonts"
mkdir -p "$dest"

url="https://github.com/googlefonts/roboto-2/raw/$COMMIT/src/hinted/Roboto-Regular.ttf"

echo "font: fetching Roboto-Regular.ttf"
curl -fsSL "$url" -o "$dest/Roboto-Regular.ttf"
curl -fsSL "https://github.com/googlefonts/roboto-2/raw/$COMMIT/LICENSE" \
  -o "$dest/Roboto-LICENSE.txt"

echo "font: ready at $dest/Roboto-Regular.ttf"
echo "  licence: $dest/Roboto-LICENSE.txt (Apache-2.0)"
