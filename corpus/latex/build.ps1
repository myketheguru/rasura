# Generate the LaTeX corpus for spec 18 question Q1.
#
# The point is to span the font setups that actually differ in /ToUnicode
# behaviour, not to produce many similar files:
#
#   pdflatex + Computer Modern (Type1)      the classic case, and the worst one
#   pdflatex + T1 encoding (cm-super)       8-bit re-encoded Type1
#   pdflatex + Latin Modern                 the modern CM replacement
#   pdflatex + cmap package                 the documented fix; proves the fix works
#   pdflatex + Times/Helvetica (URW)        non-CM Type1
#   pdflatex + microtype                    what real documents actually use
#   xelatex  + OpenType via fontspec        expected to be much better
#   lualatex + OpenType via fontspec        expected to be much better
#   xelatex/lualatex + maths                maths fonts are their own problem
#
# Requires MiKTeX or TeX Live on PATH. MiKTeX installs missing packages on
# demand; TeX Live users may need `tlmgr install cmap cm-super microtype`.
#
#   pwsh corpus/latex/build.ps1

$ErrorActionPreference = 'Continue'
$here = $PSScriptRoot
$out  = Join-Path (Split-Path $here -Parent) 'external/latex'
$work = Join-Path $env:TEMP 'rasura-latex'

New-Item -ItemType Directory -Force -Path $out, $work | Out-Null

$miktex = "$env:LOCALAPPDATA\Programs\MiKTeX\miktex\bin\x64"
if (Test-Path $miktex) { $env:PATH = "$miktex;$env:PATH" }

# Body text shared by every sample. Deliberately includes ligatures (fi, ffi),
# an em dash, accented characters and quotes: these are exactly the characters
# whose extraction breaks when /ToUnicode is missing.
$body = @'
\section{Reconstruction}
The efficient office finding affluent workflows---and the na\"ive
r\^ole of ``quoted'' text---is where extraction usually fails.
Ligatures such as fi, fl, ffi and ffl are single glyphs in the font,
so a missing \texttt{/ToUnicode} turns them into unmapped codes.
\subsection{More text}
\lipsumlike
'@

$lipsum = ('The quick brown fox jumps over the lazy dog. ' * 12)
$body = $body -replace '\\lipsumlike', $lipsum

$samples = [ordered]@{
  # --- pdflatex: the cases the spec says are the problem -------------------
  'pdflatex-cm-default' = @{
    engine = 'pdflatex'
    pre = ''
  }
  'pdflatex-t1-cmsuper' = @{
    engine = 'pdflatex'
    pre = '\usepackage[T1]{fontenc}'
  }
  'pdflatex-lmodern' = @{
    engine = 'pdflatex'
    pre = '\usepackage{lmodern}' + "`n" + '\usepackage[T1]{fontenc}'
  }
  # The documented remedy: \usepackage{cmap} emits /ToUnicode for Type1 fonts.
  'pdflatex-cmap-package' = @{
    engine = 'pdflatex'
    pre = '\usepackage{cmap}' + "`n" + '\usepackage[T1]{fontenc}'
  }
  'pdflatex-urw-times' = @{
    engine = 'pdflatex'
    pre = '\usepackage{mathptmx}'
  }
  # microtype's font expansion requires scalable fonts; the default Computer
  # Modern bitmaps make it a fatal error, so lmodern comes first -- which is
  # what real documents using microtype do anyway.
  'pdflatex-microtype' = @{
    engine = 'pdflatex'
    pre = '\usepackage{lmodern}' + "`n" + '\usepackage[T1]{fontenc}' + "`n" + '\usepackage{microtype}'
  }
  'pdflatex-truetype-via-fontspec-fails' = @{
    engine = 'pdflatex'
    pre = '\usepackage[T1]{fontenc}' + "`n" + '\usepackage{textcomp}'
  }
  # --- unicode engines: expected to be much better -------------------------
  'xelatex-opentype' = @{
    engine = 'xelatex'
    pre = '\usepackage{fontspec}' + "`n" + '\setmainfont{Latin Modern Roman}'
  }
  'lualatex-opentype' = @{
    engine = 'lualatex'
    pre = '\usepackage{fontspec}' + "`n" + '\setmainfont{Latin Modern Roman}'
  }
  'xelatex-system-font' = @{
    engine = 'xelatex'
    pre = '\usepackage{fontspec}' + "`n" + '\setmainfont{Georgia}'
  }
  'lualatex-system-font' = @{
    engine = 'lualatex'
    pre = '\usepackage{fontspec}' + "`n" + '\setmainfont{Times New Roman}'
  }
  # --- maths, which uses its own font families -----------------------------
  'pdflatex-maths' = @{
    engine = 'pdflatex'
    pre = '\usepackage{amsmath,amssymb}'
    extra = '\begin{equation} \int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}, \quad \alpha\beta\gamma \in \mathbb{R}^n \end{equation}'
  }
  # Referenced by filename rather than family name: luaotfload resolves the
  # file without needing the font registered with the system.
  'lualatex-maths-unicode' = @{
    engine = 'lualatex'
    pre = '\usepackage{unicode-math}' + "`n" + '\setmathfont{latinmodern-math.otf}'
    extra = '\begin{equation} \int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}, \quad \alpha\beta\gamma \end{equation}'
  }
}

$made = 0
foreach ($name in $samples.Keys) {
  $s = $samples[$name]
  $extra = if ($s.extra) { $s.extra } else { '' }
  $tex = @"
\documentclass[11pt]{article}
$($s.pre)
\usepackage[margin=2.5cm]{geometry}
\title{Rasura corpus: $name}
\author{generated}
\begin{document}
\maketitle
$body
$extra
\end{document}
"@
  $src = Join-Path $work "$name.tex"
  Set-Content -LiteralPath $src -Value $tex -Encoding utf8

  Write-Host -NoNewline "$($s.engine): $name ... "
  Push-Location $work
  # Two passes so \maketitle and any references settle.
  foreach ($pass in 1..2) {
    & $s.engine -interaction=nonstopmode -halt-on-error "$name.tex" *> "$name.log$pass"
  }
  Pop-Location

  $pdf = Join-Path $work "$name.pdf"
  if (Test-Path $pdf) {
    Copy-Item $pdf (Join-Path $out "$name.pdf") -Force
    Write-Host "ok"
    $made++
  } else {
    Write-Host "FAILED (see $work\$name.log2)"
  }
}

Write-Host ""
Write-Host "$made/$($samples.Count) built into $out"
Write-Host "Run: cargo run --release -p rasura-fontsurvey -- corpus/external/latex"
