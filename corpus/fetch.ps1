# Fetch external test corpora into corpus/external/ (git-ignored).
#
# These are other people's files under other people's licences. They are not
# committed: the repository records *how* to obtain them, so the corpus is
# reproducible without redistributing 100+ MB that is not ours to redistribute.
#
#   pwsh corpus/fetch.ps1
#   cargo run --release -p rasura-invariants

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$ext  = Join-Path $root 'corpus/external'
New-Item -ItemType Directory -Force -Path $ext | Out-Null

# ---------------------------------------------------------------------------
# mozilla/pdf.js -- Apache-2.0. ~990 files, ~119 MB.
#
# The single most valuable corpus available: two decades of known-hard cases,
# each one kept because it broke something. A blob-filtered sparse checkout
# fetches only test/pdfs rather than the whole ~1 GB history.
#
# The 315 `.link` stubs in that directory are files pdf.js deliberately does
# NOT redistribute. They are left alone.
# ---------------------------------------------------------------------------
$pdfjs = Join-Path $ext 'pdfjs'
if (Test-Path (Join-Path $pdfjs '.git')) {
    Write-Host 'pdf.js: updating'
    git -C $pdfjs fetch --depth 1 origin master --quiet
    git -C $pdfjs checkout --quiet FETCH_HEAD
} else {
    Write-Host 'pdf.js: cloning test/pdfs (Apache-2.0)'
    git clone --filter=blob:none --sparse --depth 1 --quiet https://github.com/mozilla/pdf.js $pdfjs
    # src/core is fetched too: its metrics.js is the source for the standard-14
    # AFM widths that spec 8.2 requires, and encodings.js for the Symbol and
    # ZapfDingbats built-in encodings. Apache-2.0, like the rest.
    git -C $pdfjs sparse-checkout set test/pdfs src/core LICENSE
}
$n = (Get-ChildItem (Join-Path $pdfjs 'test/pdfs') -Filter *.pdf).Count
Write-Host "pdf.js: $n files"

# ---------------------------------------------------------------------------
# Wanted but not fetched here, because the licence is not clear enough to
# vendor even into a git-ignored directory. Add them by hand if you have
# checked the terms yourself:
#
#   veraPDF/veraPDF-corpus        PDF/A and PDF/UA conformance. No LICENCE file
#                                 in the repository. Needed for Phase 7 tagging.
#   pdf-association/pdf20examples PDF 2.0 features. Licence NOASSERTION.
#   digitalcorpora govdocs1       Public-domain US government documents, but
#                                 distributed as multi-GB archives.
#
# Still missing entirely, and these matter most for the open questions in
# spec 18: LaTeX output (pdftex/xetex/lualatex), which is where /ToUnicode
# coverage is worst. Question Q1 cannot be answered without it.
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host 'Done. Run: cargo run --release -p rasura-invariants'
