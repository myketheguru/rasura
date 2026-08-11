# Fetch a real typeface for the glyph-injection tests. See fetch-font.sh for
# why: the synthesised fixture font shares this library's assumptions about a
# well-formed sfnt, and a typeface drawn by someone else does not.
#
# Roboto, Apache-2.0 -- already on the spec 4.3 allowlist.
#
#   pwsh corpus/fetch-font.ps1
$ErrorActionPreference = "Stop"

# Pinned to a commit, not a branch: a font revision can change glyph ids and
# advance widths, and the injection tests assert on both.
$Commit = "38062f4b4a0be4346d07a928408da21602545e9e"

$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "corpus/fonts"
New-Item -ItemType Directory -Force $dest | Out-Null

$base = "https://github.com/googlefonts/roboto-2/raw/$Commit"

Write-Host "font: fetching Roboto-Regular.ttf"
Invoke-WebRequest "$base/src/hinted/Roboto-Regular.ttf" -OutFile (Join-Path $dest "Roboto-Regular.ttf")
Invoke-WebRequest "$base/LICENSE" -OutFile (Join-Path $dest "Roboto-LICENSE.txt")

Write-Host "font: ready at $dest/Roboto-Regular.ttf"
Write-Host "  licence: $dest/Roboto-LICENSE.txt (Apache-2.0)"
