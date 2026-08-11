#!/usr/bin/env bash
# Fetch external test corpora into corpus/external/ (git-ignored).
#
# These are other people's files under other people's licences. They are not
# committed: the repository records *how* to obtain them, so the corpus is
# reproducible without redistributing 100+ MB that is not ours to redistribute.
#
#   ./corpus/fetch.sh
#   cargo run --release -p rasura-invariants
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ext="$root/corpus/external"
mkdir -p "$ext"

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
pdfjs="$ext/pdfjs"
if [ -d "$pdfjs/.git" ]; then
    echo 'pdf.js: updating'
    git -C "$pdfjs" fetch --depth 1 origin master --quiet
    git -C "$pdfjs" checkout --quiet FETCH_HEAD
else
    echo 'pdf.js: cloning test/pdfs (Apache-2.0)'
    git clone --filter=blob:none --sparse --depth 1 --quiet \
        https://github.com/mozilla/pdf.js "$pdfjs"
    # src/core is fetched too: its metrics.js is the source for the standard-14
    # AFM widths that spec 8.2 requires, and encodings.js for the Symbol and
    # ZapfDingbats built-in encodings. Apache-2.0, like the rest.
    #
    # --no-cone because LICENSE is a file, and cone mode takes directory
    # prefixes only: it fails with "fatal: 'LICENSE' is not a directory". The
    # licence is fetched deliberately — the corpus is Apache-2.0 and its terms
    # should travel with the files they cover.
    git -C "$pdfjs" sparse-checkout set --no-cone test/pdfs src/core LICENSE
fi
echo "pdf.js: $(find "$pdfjs/test/pdfs" -name '*.pdf' | wc -l) files"

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
# ---------------------------------------------------------------------------
# LaTeX samples are *generated*, not fetched: corpus/latex/build.ps1 needs a TeX
# distribution on PATH (MiKTeX or TeX Live) and produces 13 documents spanning
# the font setups that differ in /ToUnicode behaviour. See
# docs/q1-tounicode-coverage.md.
# ---------------------------------------------------------------------------
if command -v pdflatex >/dev/null 2>&1; then
    echo 'latex: a TeX distribution is present; run corpus/latex/build.ps1 to generate those samples'
else
    echo 'latex: no TeX distribution on PATH; skipping (see corpus/latex/build.ps1)'
fi

echo
echo 'Done. Run: cargo run --release -p rasura-invariants'
