#!/usr/bin/env python3
"""Generate src/standard14.rs: metrics for the 14 standard PDF fonts.

Spec 8.2: "Ship AFM metrics for the standard 14 so that layout is correct even
without the outlines."

The source is mozilla/pdf.js `src/core/metrics.js`, Apache-2.0, which is already
vendored by corpus/fetch.sh. Adobe's own Core14 AFM files would be equally
usable and equally permissive, but they are one more thing to fetch and one more
licence to vendor; pdf.js is already here, already on the licence allowlist, and
already the differential oracle, so its numbers are the ones a disagreement
would be measured against anyway.

Usage:

    python crates/rasura-font/generate-metrics.py \
        corpus/external/pdfjs/src/core/metrics.js \
        corpus/external/pdfjs/src/core/fonts_utils.js \
        > crates/rasura-font/src/standard14.rs

The second argument supplies the 258 standard Macintosh glyph names, which a
format 2.0 `post` table indexes rather than spelling out.
"""

import re
import sys

# The 14, in the order ISO 32000-1 Annex D lists them.
ORDER = [
    "Courier",
    "Courier-Bold",
    "Courier-BoldOblique",
    "Courier-Oblique",
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-BoldOblique",
    "Helvetica-Oblique",
    "Times-Roman",
    "Times-Bold",
    "Times-BoldItalic",
    "Times-Italic",
    "Symbol",
    "ZapfDingbats",
]


def parse(text):
    """Return {font: (fixed_width | None, {glyph: width})}."""
    fonts = {}

    # The whole file is one factory; the fixed-pitch fonts are plain
    # assignments and the rest open a nested factory.
    body = text[text.index("const getMetrics"):]

    # Fixed-pitch: `t.Courier = 600;` or `t["Courier-Bold"] = 600;`
    for m in re.finditer(r'^\s{2}t(?:\.(\w[\w-]*)|\["([^"]+)"\])\s*=\s*(\d+);', body, re.M):
        name = m.group(1) or m.group(2)
        fonts[name] = (int(m.group(3)), {})

    # Proportional: a nested `getLookupTableFactory` block per font.
    for m in re.finditer(
        r'^\s{2}t(?:\.(\w[\w-]*)|\["([^"]+)"\])\s*=\s*getLookupTableFactory\(function \(t\) \{(.*?)^\s{2}\}\);',
        body,
        re.M | re.S,
    ):
        name = m.group(1) or m.group(2)
        widths = {}
        for g in re.finditer(r't(?:\.(\w+)|\["([^"]+)"\])\s*=\s*(-?\d+);', m.group(3)):
            widths[g.group(1) or g.group(2)] = int(g.group(3))
        fonts[name] = (None, widths)

    return fonts


def parse_basic(text):
    """Return {font: (ascent, descent, cap_height, x_height)} from getFontBasicMetrics."""
    start = text.index("const getFontBasicMetrics")
    body = text[start:]
    out = {}
    for m in re.finditer(
        r'^\s{2}t(?:\.(\w[\w-]*)|\["([^"]+)"\])\s*=\s*\{(.*?)^\s{2}\}[,;]',
        body,
        re.M | re.S,
    ):
        name = m.group(1) or m.group(2)
        fields = {}
        for f in re.finditer(r'(\w+):\s*(Math\.NaN|-?[\d.]+)', m.group(3)):
            v = f.group(2)
            fields[f.group(1)] = None if v == "Math.NaN" else float(v)
        out[name] = fields
    return out


def rust_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    text = open(sys.argv[1], encoding="utf-8").read()
    fonts = parse(text)
    basic = parse_basic(text)

    missing = [n for n in ORDER if n not in fonts]
    if missing:
        sys.exit(f"metrics.js is missing: {missing}")

    out = sys.stdout
    out.write("//! Metrics for the 14 standard PDF fonts. Spec 8.2.\n")
    out.write("//!\n")
    out.write("//! DO NOT EDIT. Regenerate with:\n//!\n//! ```text\n")
    out.write("//! python crates/rasura-font/generate-metrics.py \\\n")
    out.write("//!     corpus/external/pdfjs/src/core/metrics.js \\\n")
    out.write("//!     > crates/rasura-font/src/standard14.rs\n//! ```\n//!\n")
    out.write("//! The widths are mozilla/pdf.js's, Apache-2.0, from\n")
    out.write("//! <https://github.com/mozilla/pdf.js> `src/core/metrics.js`. Its licence is\n")
    out.write("//! vendored at crates/rasura-font/PDFJS-LICENSE.\n//!\n")
    out.write("//! A width of -1 means the glyph is absent from that font.\n\n")

    out.write("/// A standard font's metrics: glyph widths in 1/1000 em.\n")
    out.write("pub struct StandardFont {\n")
    out.write("    pub name: &'static str,\n")
    out.write("    /// Set for the fixed-pitch faces, where every glyph is the same width.\n")
    out.write("    pub fixed_width: Option<i32>,\n")
    out.write("    /// Sorted by glyph name, so lookup can binary-search.\n")
    out.write("    pub widths: &'static [(&'static str, i32)],\n")
    out.write("    pub ascent: Option<f64>,\n")
    out.write("    pub descent: Option<f64>,\n")
    out.write("    pub cap_height: Option<f64>,\n")
    out.write("    pub x_height: Option<f64>,\n")
    out.write("}\n\n")

    for name in ORDER:
        fixed, widths = fonts[name]
        ident = "WIDTHS_" + re.sub(r"[^A-Za-z0-9]", "_", name).upper()
        out.write(f"static {ident}: &[(&str, i32)] = &[\n")
        for glyph in sorted(widths):
            out.write(f"    ({rust_str(glyph)}, {widths[glyph]}),\n")
        out.write("];\n\n")

    def opt(v):
        return "None" if v is None else f"Some({v!r})"

    out.write("/// The 14, in the order ISO 32000-1 Annex D lists them.\n")
    out.write("pub static STANDARD_14: &[StandardFont] = &[\n")
    for name in ORDER:
        fixed, _ = fonts[name]
        ident = "WIDTHS_" + re.sub(r"[^A-Za-z0-9]", "_", name).upper()
        b = basic.get(name, {})
        out.write("    StandardFont {\n")
        out.write(f"        name: {rust_str(name)},\n")
        out.write(f"        fixed_width: {'None' if fixed is None else f'Some({fixed})'},\n")
        out.write(f"        widths: {ident},\n")
        out.write(f"        ascent: {opt(b.get('ascent'))},\n")
        out.write(f"        descent: {opt(b.get('descent'))},\n")
        out.write(f"        cap_height: {opt(b.get('capHeight'))},\n")
        out.write(f"        x_height: {opt(b.get('xHeight'))},\n")
        out.write("    },\n")
    out.write("];\n\n")

    if len(sys.argv) > 2:
        emit_mac_ordering(out, sys.argv[2])


def emit_mac_ordering(out, path):
    """The 258 standard Macintosh glyph names, for the `post` table.

    A format 2.0 `post` table stores an index per glyph; indices below 258 name
    one of these rather than carrying a string. Without the list, two-thirds of
    the glyph names in a typical TrueType font are unreadable.
    """
    text = open(path, encoding="utf-8").read()
    m = re.search(r"const MacStandardGlyphOrdering = \[(.*?)\];", text, re.S)
    if not m:
        sys.exit(f"MacStandardGlyphOrdering not found in {path}")
    names = re.findall(r'"([^"]*)"', m.group(1))
    if len(names) != 258:
        sys.exit(f"expected 258 Macintosh glyph names, found {len(names)}")

    out.write("/// The 258 standard Macintosh glyph names, indexed as a format 2.0\n")
    out.write("/// `post` table's glyph-name indices are.\n")
    out.write("pub static MAC_GLYPH_ORDER: [&str; 258] = [\n")
    for n in names:
        out.write(f"    {rust_str(n)},\n")
    out.write("];\n")
    sys.stderr.write(f"{len(names)} Macintosh glyph names\n")


if __name__ == "__main__":
    main()
