#!/usr/bin/env python3
"""Generate src/glyphdata.rs: the Adobe Glyph List and the base encodings.

Generated rather than hand-written. The AGL is 4300 entries and the encodings
are 256 each; typing them is a guaranteed source of silent one-character errors
that would show up as a single wrong glyph on some page, months later.

    python crates/rasura-layout/generate-tables.py \
        glyphlist.txt \
        corpus/external/pdfjs/src/core/encodings.js \
        zapfdingbats.txt \
        > crates/rasura-layout/src/glyphdata.rs

glyphlist.txt and zapfdingbats.txt come from
https://github.com/adobe-type-tools/agl-aglfn (BSD-3-Clause). Its licence is
vendored alongside as AGL-LICENSE.

encodings.js is mozilla/pdf.js (Apache-2.0), already fetched by
corpus/fetch.sh. It supplies the code-to-glyph-*name* tables -- the AGL is
name-to-character and many characters have several names, so inverting it would
pick a plausible name that is not the one the standard-14 metrics are keyed by.
"""

import sys

# ---------------------------------------------------------------------------
# Adobe StandardEncoding, ISO 32000-1 Annex D.2. Written as glyph names and
# resolved through the AGL, so the two cannot disagree.
# ---------------------------------------------------------------------------
STANDARD = {
    32: "space", 33: "exclam", 34: "quotedbl", 35: "numbersign", 36: "dollar",
    37: "percent", 38: "ampersand", 39: "quoteright", 40: "parenleft",
    41: "parenright", 42: "asterisk", 43: "plus", 44: "comma", 45: "hyphen",
    46: "period", 47: "slash", 48: "zero", 49: "one", 50: "two", 51: "three",
    52: "four", 53: "five", 54: "six", 55: "seven", 56: "eight", 57: "nine",
    58: "colon", 59: "semicolon", 60: "less", 61: "equal", 62: "greater",
    63: "question", 64: "at",
    91: "bracketleft", 92: "backslash", 93: "bracketright", 94: "asciicircum",
    95: "underscore", 96: "quoteleft",
    123: "braceleft", 124: "bar", 125: "braceright", 126: "asciitilde",
    161: "exclamdown", 162: "cent", 163: "sterling", 164: "fraction",
    165: "yen", 166: "florin", 167: "section", 168: "currency",
    169: "quotesingle", 170: "quotedblleft", 171: "guillemotleft",
    172: "guilsinglleft", 173: "guilsinglright", 174: "fi", 175: "fl",
    177: "endash", 178: "dagger", 179: "daggerdbl", 180: "periodcentered",
    182: "paragraph", 183: "bullet", 184: "quotesinglbase",
    185: "quotedblbase", 186: "quotedblright", 187: "guillemotright",
    188: "ellipsis", 189: "perthousand", 191: "questiondown", 193: "grave",
    194: "acute", 195: "circumflex", 196: "tilde", 197: "macron", 198: "breve",
    199: "dotaccent", 200: "dieresis", 202: "ring", 203: "cedilla",
    205: "hungarumlaut", 206: "ogonek", 207: "caron", 208: "emdash",
    225: "AE", 227: "ordfeminine", 232: "Lslash", 233: "Oslash", 234: "OE",
    235: "ordmasculine", 241: "ae", 245: "dotlessi", 248: "lslash",
    249: "oslash", 250: "oe", 251: "germandbls",
}
for i in range(65, 91):
    STANDARD[i] = chr(i)
for i in range(97, 123):
    STANDARD[i] = chr(i)

# ISO 32000-1 Annex D: PDF's MacRomanEncoding differs from Mac OS Roman at a
# handful of positions. Python's `mac_roman` codec is the Apple table, so these
# are applied on top of it.
MAC_ROMAN_PDF_OVERRIDES = {
    0xDB: "¤",  # currency, where Apple later put the euro sign
}


def load_agl(path):
    agl = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            name, _, codes = line.partition(";")
            if not codes:
                continue
            try:
                text = "".join(chr(int(c, 16)) for c in codes.split())
            except ValueError:
                continue
            agl[name] = text
    return agl


def rust_str(s):
    out = []
    for ch in s:
        if ch == "\\":
            out.append("\\\\")
        elif ch == '"':
            out.append('\\"')
        elif ch == "\n":
            out.append("\\n")
        elif 0x20 <= ord(ch) < 0x7F:
            out.append(ch)
        else:
            out.append("\\u{%x}" % ord(ch))
    return '"' + "".join(out) + '"'


def emit_encoding(name, doc, table):
    print(f"/// {doc}")
    print(f"pub static {name}: [Option<&str>; 256] = [")
    for i in range(256):
        v = table.get(i)
        print(f"    {rust_str(v) if v is not None else 'None'}," if v is None
              else f"    Some({rust_str(v)}),")
    print("];")
    print()


def emit_names(name, doc, table):
    """A code-to-glyph-name table."""
    print(f"/// {doc}")
    print(f"pub static {name}: [Option<&str>; 256] = [")
    for i in range(256):
        v = table.get(i)
        print("    None," if v is None else f"    Some({rust_str(v)}),")
    print("];")
    print()


def load_pdfjs_encodings(path):
    """The code-to-glyph-name tables from pdf.js encodings.js, Apache-2.0.

    Taken from there rather than reverse-derived from the Unicode tables: the
    AGL is name-to-character and many characters have several names, so
    inverting it would pick a plausible name that is not the one the metrics
    are keyed by.
    """
    import re

    text = open(path, encoding="utf-8").read()
    out = {}
    for const in (
        "StandardEncoding",
        "MacRomanEncoding",
        "WinAnsiEncoding",
        "SymbolSetEncoding",
        "ZapfDingbatsEncoding",
    ):
        m = re.search(rf"const {const} = \[(.*?)\];", text, re.S)
        if not m:
            sys.exit(f"{const} not found in {path}")
        # A flat list of quoted glyph names; "" marks an unused code.
        items = re.findall(r'"([^"]*)"', m.group(1))
        out[const] = {i: n for i, n in enumerate(items) if n}
    return out


def main():
    agl_path = sys.argv[1]
    agl = load_agl(agl_path)

    print("//! Generated tables: the Adobe Glyph List and the base encodings.")
    print("//!")
    print("//! DO NOT EDIT. Regenerate with:")
    print("//!")
    print("//! ```text")
    print("//! python crates/rasura-layout/generate-tables.py glyphlist.txt \\")
    print("//!     > crates/rasura-layout/src/glyphdata.rs")
    print("//! ```")
    print("//!")
    print("//! The glyph list is Adobe's, from")
    print("//! <https://github.com/adobe-type-tools/agl-aglfn>, BSD-3-Clause. Its licence")
    print("//! is vendored at crates/rasura-layout/AGL-LICENSE.")
    print("//!")
    print("//! Q1 established that this table carries the load in spec 7.2: 300 of the")
    print("//! 653 fonts in the corpus without a usable /ToUnicode resolve through it and")
    print("//! nothing else. It has to be the whole list, not a table of common names.")
    print()

    # --- AGL, sorted for binary search --------------------------------------
    names = sorted(agl)
    print(f"/// The Adobe Glyph List: {len(names)} entries, sorted by name so")
    print("/// [`crate::agl::lookup`] can binary-search it.")
    print(f"pub static AGL: [(&str, &str); {len(names)}] = [")
    for n in names:
        print(f"    ({rust_str(n)}, {rust_str(agl[n])}),")
    print("];")
    print()

    # --- StandardEncoding ---------------------------------------------------
    std = {}
    for code, name in STANDARD.items():
        if len(name) == 1 and name.isascii() and name.isalpha():
            std[code] = name
        elif name in agl:
            std[code] = agl[name]
        else:
            sys.stderr.write(f"warning: StandardEncoding {code} -> {name} not in AGL\n")
    emit_encoding(
        "STANDARD_ENCODING",
        "Adobe StandardEncoding. ISO 32000-1 Annex D.2.",
        std,
    )

    # --- WinAnsiEncoding: CP1252 -------------------------------------------
    # Codes below 32 are left undefined. CP1252 maps them to the C0 control
    # characters, but ISO 32000-1 Annex D defines no glyph there, and mapping
    # them would inject control characters into extracted text.
    win = {}
    for i in range(32, 256):
        try:
            ch = bytes([i]).decode("cp1252")
        except UnicodeDecodeError:
            continue
        win[i] = ch
    # ISO 32000-1: the unused CP1252 positions map to bullet in WinAnsi.
    for i in (0x81, 0x8D, 0x8F, 0x90, 0x9D):
        win[i] = "•"
    win[0xA0] = " "  # non-breaking space behaves as space
    win[0xAD] = "-"  # soft hyphen behaves as hyphen
    emit_encoding(
        "WIN_ANSI_ENCODING",
        "WinAnsiEncoding, i.e. CP1252 with the PDF-specific fill-ins. ISO 32000-1 Annex D.2.",
        win,
    )

    # --- MacRomanEncoding ---------------------------------------------------
    mac = {}
    for i in range(32, 256):
        try:
            mac[i] = bytes([i]).decode("mac_roman")
        except UnicodeDecodeError:
            continue
    mac.update(MAC_ROMAN_PDF_OVERRIDES)
    emit_encoding(
        "MAC_ROMAN_ENCODING",
        "MacRomanEncoding. Apple's Mac OS Roman with the PDF differences applied.",
        mac,
    )

    # --- glyph-name tables --------------------------------------------------
    # The encodings above map a code to a *character*, which is what spec 7.2
    # needs. Metrics need the other half: a code to a glyph *name*, because the
    # standard-14 width tables are keyed by name. Emitted from the same source
    # dictionaries so the two can never drift apart.
    if len(sys.argv) <= 2:
        sys.exit("usage: generate-tables.py <glyphlist.txt> <pdfjs encodings.js>")
    enc = load_pdfjs_encodings(sys.argv[2])

    for const, rust, doc in [
        ("StandardEncoding", "STANDARD_NAMES", "Adobe StandardEncoding"),
        ("WinAnsiEncoding", "WIN_ANSI_NAMES", "WinAnsiEncoding"),
        ("MacRomanEncoding", "MAC_ROMAN_NAMES", "MacRomanEncoding"),
    ]:
        emit_names(rust, f"Glyph names of {doc}, for metric lookup.", enc[const])

    # --- Symbol and ZapfDingbats -------------------------------------------
    # These two are the standard 14's symbolic faces, and their built-in
    # encoding is their own: neither StandardEncoding nor WinAnsi describes
    # them. Without these tables a document naming /Symbol and embedding
    # nothing has no encoding and no metrics -- the gap carried since Phase 2.
    symbol, zapf = enc["SymbolSetEncoding"], enc["ZapfDingbatsEncoding"]
    emit_names("SYMBOL_NAMES", "Glyph names of the Symbol font's built-in encoding.", symbol)
    emit_names(
        "ZAPF_DINGBATS_NAMES",
        "Glyph names of the ZapfDingbats font's built-in encoding.",
        zapf,
    )
    emit_encoding(
        "SYMBOL_ENCODING",
        "Symbol's built-in encoding, resolved through the AGL.",
        {c: agl[n] for c, n in symbol.items() if n in agl},
    )
    # The dingbat names -- a1 through a191 -- are not in glyphlist.txt. They
    # have their own file in the same repository, kept separate on purpose:
    # merging them into the AGL would let an ordinary font's `a1` resolve to a
    # dingbat, which is a wrong character rather than a missing one.
    zapf_agl = load_agl(sys.argv[3]) if len(sys.argv) > 3 else {}
    emit_encoding(
        "ZAPF_DINGBATS_ENCODING",
        "ZapfDingbats' built-in encoding, from the dingbats glyph list.",
        {c: zapf_agl[n] for c, n in zapf.items() if n in zapf_agl},
    )
    if not zapf_agl:
        sys.stderr.write("warning: no zapfdingbats.txt given; that table will be empty\n")
    sys.stderr.write(f"symbol {len(symbol)}, zapf {len(zapf)}\n")

    sys.stderr.write(
        f"generated {len(names)} AGL entries, "
        f"{len(std)} StandardEncoding, {len(win)} WinAnsi, {len(mac)} MacRoman\n"
    )


if __name__ == "__main__":
    main()
