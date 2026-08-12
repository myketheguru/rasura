#!/usr/bin/env bash
# Fetch the pdfium reference renderer for the spec 14.3 pixel-diff harness.
#
# pdfium is **test-only** (spec 4.2): "PDFium may be used as a test-only
# reference renderer for the pixel-diff harness; it is never shipped." It is
# downloaded rather than vendored, so the repository carries no third-party
# binary and the licence stays with its author.
#
# The build is bblanchon/pdfium-binaries -- MIT around Google's pdfium, which is
# BSD-3-Clause. Pinned to a release rather than `latest`, so a renderer change
# shows up as a deliberate commit and not as yesterday's green build going red.
#
#   ./harness/pixeldiff/fetch.sh
set -euo pipefail

# Pinned. Bump deliberately: a new pdfium can legitimately change anti-aliasing,
# and the pixel diff would report that as a regression in this library.
RELEASE="chromium/7988"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dest="$root/target/pdfium"
mkdir -p "$dest"

case "$(uname -s)" in
  Linux*)  asset="pdfium-linux-x64.tgz"; lib="libpdfium.so" ;;
  Darwin*) asset="pdfium-mac-arm64.tgz"; lib="libpdfium.dylib" ;;
  *)       asset="pdfium-win-x64.tgz";   lib="pdfium.dll" ;;
esac

echo "pdfium: fetching $RELEASE ($asset)"
# --retry-all-errors, as in corpus/fetch-font.sh: a dropped connection is the
# failure that happens here, and plain --retry does not cover it.
curl -fsSL --retry 5 --retry-delay 3 --retry-all-errors --connect-timeout 20 \
  "https://github.com/bblanchon/pdfium-binaries/releases/download/$RELEASE/$asset" \
  -o "/tmp/$asset"
tar -xzf "/tmp/$asset" -C "$dest"

# The harness looks for the library beside itself or in target/pdfium.
cp "$dest/lib/$lib" "$dest/$lib" 2>/dev/null || cp "$dest/bin/$lib" "$dest/$lib"

echo "pdfium: ready at $dest"
echo "  licence: $dest/LICENSE (MIT), third-party in licenses/"
