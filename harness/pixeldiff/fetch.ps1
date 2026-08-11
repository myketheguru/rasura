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
#   pwsh harness/pixeldiff/fetch.ps1

$ErrorActionPreference = 'Stop'

# Pinned. Bump deliberately: a new pdfium can legitimately change anti-aliasing,
# and the pixel diff would report that as a regression in this library.
$Release = 'chromium/7988'

$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$dest = Join-Path $root 'target/pdfium'
New-Item -ItemType Directory -Force -Path $dest | Out-Null

$asset = 'pdfium-win-x64.tgz'
$url = "https://github.com/bblanchon/pdfium-binaries/releases/download/$Release/$asset"

Write-Host "pdfium: fetching $Release"
$archive = Join-Path $env:TEMP $asset
Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
tar -xzf $archive -C $dest

# The harness looks for the library beside itself or in target/pdfium.
$dll = Join-Path $dest 'bin/pdfium.dll'
if (-not (Test-Path $dll)) { throw "pdfium.dll not found in $dest" }
Copy-Item $dll (Join-Path $dest 'pdfium.dll') -Force

Write-Host "pdfium: ready at $dest"
Write-Host "  licence: $(Join-Path $dest 'LICENSE') (MIT), third-party in licenses/"
