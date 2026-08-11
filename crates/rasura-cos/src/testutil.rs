//! Programmatically generated PDF fixtures.
//!
//! Spec 14.1 calls for a corpus of real producer output under `corpus/`, which
//! is where the interesting failures live. These fixtures are the other half:
//! small, exactly-known files that pin down specific structural cases, so a
//! failure points at one mechanism rather than at "some 4 MB InDesign file".
//!
//! Public rather than `#[cfg(test)]` so the invariant harness can use them too.

use crate::filters;
use crate::object::{Dictionary, Name, Object};

/// Accumulates objects and emits a file with a classic cross-reference table.
pub struct ClassicBuilder {
    body: Vec<u8>,
    offsets: Vec<(u32, usize)>,
    version: &'static str,
}

impl Default for ClassicBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClassicBuilder {
    pub fn new() -> Self {
        ClassicBuilder { body: Vec::new(), offsets: Vec::new(), version: "1.4" }
    }

    pub fn version(mut self, v: &'static str) -> Self {
        self.version = v;
        self
    }

    fn start(&mut self) {
        if self.body.is_empty() {
            self.body.extend_from_slice(format!("%PDF-{}\n", self.version).as_bytes());
            // A binary comment line, which every real producer writes so that
            // transfer software treats the file as binary.
            self.body.extend_from_slice(b"%\xe2\xe3\xcf\xd3\n");
        }
    }

    /// Append `n 0 obj\n<body>\nendobj\n`.
    pub fn object(mut self, number: u32, body: &str) -> Self {
        self.start();
        self.offsets.push((number, self.body.len()));
        self.body.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes());
        self
    }

    /// Append an object whose body is written by hand, for cases the
    /// convenience methods cannot express (an indirect `/Length`, say).
    pub fn raw_object(mut self, number: u32, f: impl FnOnce(&mut Vec<u8>)) -> Self {
        self.start();
        self.offsets.push((number, self.body.len()));
        self.body.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        f(&mut self.body);
        self.body.extend_from_slice(b"\nendobj\n");
        self
    }

    /// Append a stream object, computing `/Length` correctly.
    pub fn stream(mut self, number: u32, dict_extra: &str, data: &[u8]) -> Self {
        self.start();
        self.offsets.push((number, self.body.len()));
        self.body.extend_from_slice(
            format!(
                "{number} 0 obj\n<< /Length {}{}{} >>\nstream\n",
                data.len(),
                if dict_extra.is_empty() { "" } else { " " },
                dict_extra
            )
            .as_bytes(),
        );
        self.body.extend_from_slice(data);
        self.body.extend_from_slice(b"\nendstream\nendobj\n");
        self
    }

    /// Emit the xref table, trailer, and `%%EOF`.
    pub fn finish(mut self, trailer_extra: &str) -> Vec<u8> {
        self.start();
        self.offsets.sort_by_key(|(n, _)| *n);
        let max = self.offsets.last().map_or(0, |(n, _)| *n);
        let size = max + 1;

        let xref_at = self.body.len();
        self.body.extend_from_slice(b"xref\n");

        // Emit one subsection per contiguous run of object numbers.
        let mut i = 0usize;
        let mut wrote_zero = false;
        while i < self.offsets.len() {
            let mut j = i;
            while j + 1 < self.offsets.len() && self.offsets[j + 1].0 == self.offsets[j].0 + 1 {
                j += 1;
            }
            let first = self.offsets[i].0;
            let count = j - i + 1;
            if first == 1 && !wrote_zero {
                // Fold the mandatory free entry for object 0 into this run.
                self.body.extend_from_slice(format!("0 {}\n", count + 1).as_bytes());
                self.body.extend_from_slice(b"0000000000 65535 f \n");
                wrote_zero = true;
            } else {
                self.body.extend_from_slice(format!("{first} {count}\n").as_bytes());
            }
            for (_, off) in &self.offsets[i..=j] {
                self.body.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            i = j + 1;
        }
        if !wrote_zero {
            self.body.extend_from_slice(b"0 1\n0000000000 65535 f \n");
        }

        self.body.extend_from_slice(
            format!("trailer\n<< /Size {size} {trailer_extra} >>\nstartxref\n{xref_at}\n%%EOF\n")
                .as_bytes(),
        );
        self.body
    }
}

/// Catalog, page tree, one page. The smallest thing that is unambiguously a PDF.
pub fn minimal_classic() -> Vec<u8> {
    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>")
        .finish("/Root 1 0 R")
}

/// Adds a Flate-compressed content stream as object 4.
pub fn classic_with_flate_content() -> Vec<u8> {
    let content = b"BT /F1 24 Tf 72 700 Td (Hello, rasura) Tj ET\n";
    let chain = filters::FilterChain::build(Some(&Object::name("FlateDecode")), None);
    let compressed = filters::encode(&chain, content, 1).unwrap();

    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>",
        )
        .stream(4, "/Filter /FlateDecode", &compressed)
        .object(
            5,
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        )
        .finish("/Root 1 0 R /ID [<0123456789ABCDEF0123456789ABCDEF> <0123456789ABCDEF0123456789ABCDEF>]")
}

/// A file whose catalog, page tree and page all live inside an object stream,
/// indexed by a cross-reference stream. This is what every modern producer
/// emits, and it exercises `/Type /ObjStm` plus type-2 xref entries.
pub fn xref_stream_with_objstm() -> Vec<u8> {
    let members: [(u32, &str); 3] = [
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ];

    // Lay out the object stream: a header of `num offset` pairs, then bodies.
    let mut bodies = Vec::new();
    let mut header = String::new();
    for (num, body) in members {
        header.push_str(&format!("{num} {} ", bodies.len()));
        bodies.extend_from_slice(body.as_bytes());
        bodies.push(b'\n');
    }
    let first = header.len();
    let mut objstm_data = header.into_bytes();
    objstm_data.extend_from_slice(&bodies);

    let chain = filters::FilterChain::build(Some(&Object::name("FlateDecode")), None);
    let objstm_compressed = filters::encode(&chain, &objstm_data, 1).unwrap();

    let mut buf = Vec::new();
    buf.extend_from_slice(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n");

    // Object 4: the object stream.
    let objstm_at = buf.len();
    buf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Type /ObjStm /N {} /First {first} /Length {} /Filter /FlateDecode >>\nstream\n",
            members.len(),
            objstm_compressed.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(&objstm_compressed);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    // Object 5: the cross-reference stream, describing objects 0..=5.
    let xref_at = buf.len();
    let mut rows: Vec<[u8; 7]> = Vec::new();
    let row = |kind: u8, field2: u32, field3: u16| {
        let mut r = [0u8; 7];
        r[0] = kind;
        r[1..5].copy_from_slice(&field2.to_be_bytes());
        r[5..7].copy_from_slice(&field3.to_be_bytes());
        r
    };
    rows.push(row(0, 0, 65535)); // object 0, free
    for (i, (num, _)) in members.iter().enumerate() {
        debug_assert_eq!(*num as usize, i + 1);
        rows.push(row(2, 4, i as u16)); // in object stream 4, at index i
    }
    rows.push(row(1, objstm_at as u32, 0));
    rows.push(row(1, xref_at as u32, 0));

    let xref_data: Vec<u8> = rows.concat();
    buf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /XRef /Size 6 /W [1 4 2] /Root 1 0 R /Length {} >>\nstream\n",
            xref_data.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(&xref_data);
    buf.extend_from_slice(b"\nendstream\nendobj\n");
    buf.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
    buf
}

/// A two-revision file: the base document plus an incremental update that
/// rotates the page. Exercises `/Prev` chain walking.
pub fn two_revisions() -> Vec<u8> {
    let base = minimal_classic();
    // Note the leading newline: a bare `xref\n` also matches inside
    // `startxref\n`, which would point the /Prev chain at nonsense.
    let base_xref =
        crate::parser::rfind_bytes(&base, b"\nxref\n").expect("fixture has a classic table") + 1;

    let mut buf = base;
    let obj_at = buf.len();
    buf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Rotate 90 >>\nendobj\n",
    );
    let xref_at = buf.len();
    buf.extend_from_slice(
        format!(
            "xref\n3 1\n{obj_at:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Prev {base_xref} >>\nstartxref\n{xref_at}\n%%EOF\n"
        )
        .as_bytes(),
    );
    buf
}

// ---------------------------------------------------------------------------
// Encrypted fixtures
// ---------------------------------------------------------------------------

/// Which cipher an encrypted fixture should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureCipher {
    Rc4_128,
    Aes128,
}

/// A document with an encrypted content stream and an encrypted `/Info` string,
/// using the empty user password -- the overwhelmingly common shape.
///
/// Built by this crate rather than captured from a producer, so the plaintext is
/// known exactly and a decryption bug shows up as a mismatch rather than as
/// plausible-looking garbage.
pub fn encrypted(cipher: FixtureCipher) -> Vec<u8> {
    use crate::crypt::fixture;
    use crate::object::ObjId;

    let id0: &[u8; 16] = b"RasuraFixture001";
    let p = -1i32;
    let (encrypt_dict, dec) = match cipher {
        FixtureCipher::Rc4_128 => fixture::rc4_128(id0, p),
        FixtureCipher::Aes128 => fixture::aes_128(id0, p),
    };

    let content = ENCRYPTED_FIXTURE_CONTENT.as_bytes();
    let content_enc = dec.encrypt_stream(ObjId::new(4, 0), content).expect("fixture encrypt");
    let title_enc = dec
        .encrypt_string(ObjId::new(5, 0), ENCRYPTED_FIXTURE_TITLE.as_bytes())
        .expect("fixture encrypt");

    let mut encrypt_bytes = Vec::new();
    crate::writer::write_object(&mut encrypt_bytes, &Object::Dictionary(encrypt_dict));

    let id_hex: String = id0.iter().map(|b| format!("{b:02X}")).collect();
    let title_hex: String = title_enc.iter().map(|b| format!("{b:02X}")).collect();

    ClassicBuilder::new()
        .version("1.6")
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>")
        .stream(4, "", &content_enc)
        .object(5, &format!("<< /Title <{title_hex}> >>"))
        .object(6, &String::from_utf8_lossy(&encrypt_bytes))
        .finish(&format!("/Root 1 0 R /Info 5 0 R /Encrypt 6 0 R /ID [<{id_hex}> <{id_hex}>]"))
}

/// The same document as `encrypted`, but indexed by a cross-reference stream
/// rather than a classic table -- which is what every modern encrypted producer
/// emits.
///
/// This combination is the one that matters: the trailer then lives *inside* a
/// stream, and that stream is exempt from encryption. A writer that does not
/// know about the exemption encrypts the `/ID` sitting in it, which changes the
/// input to the key derivation and leaves the file unable to open itself.
pub fn encrypted_xref_stream(cipher: FixtureCipher) -> Vec<u8> {
    use crate::crypt::fixture;
    use crate::object::ObjId;

    let id0: &[u8; 16] = b"RasuraStream0001";
    let p = -1i32;
    let (encrypt_dict, dec) = match cipher {
        FixtureCipher::Rc4_128 => fixture::rc4_128(id0, p),
        FixtureCipher::Aes128 => fixture::aes_128(id0, p),
    };

    let content_enc = dec
        .encrypt_stream(ObjId::new(4, 0), ENCRYPTED_FIXTURE_CONTENT.as_bytes())
        .expect("fixture encrypt");
    let title_enc = dec
        .encrypt_string(ObjId::new(5, 0), ENCRYPTED_FIXTURE_TITLE.as_bytes())
        .expect("fixture encrypt");
    let title_hex: String = title_enc.iter().map(|b| format!("{b:02X}")).collect();
    let id_hex: String = id0.iter().map(|b| format!("{b:02X}")).collect();

    let mut encrypt_bytes = Vec::new();
    crate::writer::write_object(&mut encrypt_bytes, &Object::Dictionary(encrypt_dict));

    let mut buf = Vec::new();
    buf.extend_from_slice(b"%PDF-1.6\n%\xe2\xe3\xcf\xd3\n");
    let mut offsets: Vec<(u32, usize)> = Vec::new();

    let plain = |buf: &mut Vec<u8>, offsets: &mut Vec<(u32, usize)>, n: u32, body: &str| {
        offsets.push((n, buf.len()));
        buf.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    };
    plain(&mut buf, &mut offsets, 1, "<< /Type /Catalog /Pages 2 0 R >>");
    plain(&mut buf, &mut offsets, 2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    plain(
        &mut buf,
        &mut offsets,
        3,
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
    );

    offsets.push((4, buf.len()));
    buf.extend_from_slice(
        format!("4 0 obj\n<< /Length {} >>\nstream\n", content_enc.len()).as_bytes(),
    );
    buf.extend_from_slice(&content_enc);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    plain(&mut buf, &mut offsets, 5, &format!("<< /Title <{title_hex}> >>"));
    plain(&mut buf, &mut offsets, 6, &String::from_utf8_lossy(&encrypt_bytes));

    // Object 7: the cross-reference stream. Deliberately unencrypted and
    // uncompressed, so a failure here is legible in a hex dump.
    let xref_at = buf.len();
    let mut rows: Vec<u8> = Vec::new();
    let row = |kind: u8, f2: u32, f3: u16, rows: &mut Vec<u8>| {
        rows.push(kind);
        rows.extend_from_slice(&f2.to_be_bytes());
        rows.extend_from_slice(&f3.to_be_bytes());
    };
    row(0, 0, 65535, &mut rows);
    for (_, off) in &offsets {
        row(1, *off as u32, 0, &mut rows);
    }
    row(1, xref_at as u32, 0, &mut rows);

    buf.extend_from_slice(
        format!(
            "7 0 obj\n<< /Type /XRef /Size 8 /W [1 4 2] /Root 1 0 R /Info 5 0 R /Encrypt 6 0 R \
             /ID [<{id_hex}> <{id_hex}>] /Length {} >>\nstream\n",
            rows.len()
        )
        .as_bytes(),
    );
    buf.extend_from_slice(&rows);
    buf.extend_from_slice(b"\nendstream\nendobj\n");
    buf.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());
    buf
}

/// The plaintext an `encrypted()` fixture's content stream must decrypt to.
pub const ENCRYPTED_FIXTURE_CONTENT: &str = "BT /F1 12 Tf 72 720 Td (Encrypted body text) Tj ET\n";
/// The plaintext an `encrypted()` fixture's `/Info` `/Title` must decrypt to.
pub const ENCRYPTED_FIXTURE_TITLE: &str = "A title behind the standard security handler";

// ---------------------------------------------------------------------------
// Adversarial fixtures (spec 14.1)
// ---------------------------------------------------------------------------

/// Files that are wrong in a specific, named way. Each one pins a recovery path
/// that would otherwise only be exercised by whatever damaged file a user
/// happens to open.
pub fn adversarial() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("wrong-stream-length", wrong_stream_length()),
        ("indirect-stream-length", indirect_stream_length()),
        ("startxref-into-the-void", bad_startxref()),
        ("no-eof-marker", no_eof_marker()),
        ("cr-only-line-endings", cr_only_line_endings()),
        ("bytes-before-header", bytes_before_header()),
        ("cyclic-page-tree", cyclic_page_tree()),
        ("reused-object-number", reused_object_number()),
        ("odd-encodings-preserved", odd_encodings()),
        ("free-entry-in-the-middle", free_entry_in_the_middle()),
    ]
}

/// `/Length` overstates the stream; the parser must scan for `endstream`.
fn wrong_stream_length() -> Vec<u8> {
    let good = classic_with_flate_content();
    let s = String::from_utf8_lossy(&good).replacen("/Length ", "/Length 99999 % was ", 1);
    s.into_bytes()
}

/// `/Length` is an indirect reference, which must be resolved before the stream
/// body can be read.
fn indirect_stream_length() -> Vec<u8> {
    let content: &[u8] = b"BT /F1 12 Tf 72 720 Td (indirect length) Tj ET\n";
    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>")
        .raw_object(4, |body| {
            body.extend_from_slice(b"<< /Length 5 0 R >>\nstream\n");
            body.extend_from_slice(content);
            body.extend_from_slice(b"\nendstream");
        })
        .object(5, &content.len().to_string())
        .finish("/Root 1 0 R")
}

/// `startxref` points past the end of the file: recovery must rebuild.
fn bad_startxref() -> Vec<u8> {
    let good = minimal_classic();
    let s =
        String::from_utf8_lossy(&good).replacen("startxref\n", "startxref\n999999999\n%old:", 1);
    s.into_bytes()
}

/// Truncated after the last object, with no table and no `%%EOF`.
fn no_eof_marker() -> Vec<u8> {
    let good = minimal_classic();
    let at = crate::parser::rfind_bytes(&good, b"\nxref\n").unwrap();
    good[..at].to_vec()
}

/// Every line ending is a lone CR, which was legal on classic Mac OS and still
/// appears in files produced by old tooling.
fn cr_only_line_endings() -> Vec<u8> {
    // Rebuilt rather than search-and-replaced, because the xref offsets have to
    // stay correct and a naive replacement would keep the byte count the same
    // only by accident.
    let mut body = Vec::new();
    body.extend_from_slice(b"%PDF-1.4\r");
    let mut offsets = Vec::new();
    for (n, content) in [
        (1u32, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ] {
        offsets.push(body.len());
        body.extend_from_slice(format!("{n} 0 obj\r{content}\rendobj\r").as_bytes());
    }
    let xref_at = body.len();
    body.extend_from_slice(b"xref\r0 4\r0000000000 65535 f \r");
    for off in &offsets {
        body.extend_from_slice(format!("{off:010} 00000 n \r").as_bytes());
    }
    body.extend_from_slice(
        format!("trailer\r<< /Size 4 /Root 1 0 R >>\rstartxref\r{xref_at}\r%%EOF\r").as_bytes(),
    );
    body
}

/// An HTTP-style preamble before `%PDF-`, which shifts every recorded offset.
fn bytes_before_header() -> Vec<u8> {
    let mut out = b"HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\n\r\n".to_vec();
    out.extend_from_slice(&minimal_classic());
    out
}

/// A page tree whose child points back at its own ancestor. Walking it naively
/// never terminates.
fn cyclic_page_tree() -> Vec<u8> {
    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(3, "<< /Type /Pages /Parent 2 0 R /Kids [2 0 R] /Count 1 >>")
        .finish("/Root 1 0 R")
}

/// Object number 3 is freed and reissued at generation 1 by a second revision,
/// so resolving it must use the generation the table gives rather than assuming
/// zero.
fn reused_object_number() -> Vec<u8> {
    let base = minimal_classic();
    let base_xref = crate::parser::rfind_bytes(&base, b"\nxref\n").unwrap() + 1;

    let mut buf = base;
    let obj_at = buf.len();
    buf.extend_from_slice(
        b"3 1 obj\n<< /Type /Page /Parent 2 0 R /Note (generation one) >>\nendobj\n",
    );
    let xref_at = buf.len();
    buf.extend_from_slice(
        format!(
            "xref\n3 1\n{obj_at:010} 00001 n \ntrailer\n<< /Size 4 /Root 1 0 R /Prev {base_xref} >>\nstartxref\n{xref_at}\n%%EOF\n"
        )
        .as_bytes(),
    );
    buf
}

/// Names and strings whose raw encoding differs from their decoded value. If
/// byte preservation regresses, this file stops round-tripping.
fn odd_encodings() -> Vec<u8> {
    ClassicBuilder::new()
        .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .object(
            3,
            // `/N#61me` decodes to `/Name`, `/Odd#23Key` to `/Odd#Key`, the
            // literal string carries an octal escape and a line continuation,
            // and `4.` / `-.002` are real forms most parsers get wrong.
            "<< /Type /Page /Parent 2 0 R /N#61me (esc\\101pes and \\(parens\\)) \
             /Hex <48656C6C6F> /Odd#23Key (line\\\ncontinuation) /R#65al 4. /Neg -.002 >>",
        )
        .finish("/Root 1 0 R")
}

/// A free entry between two in-use ones, so the subsection layout is not one
/// contiguous run.
fn free_entry_in_the_middle() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (n, content) in [
        (1u32, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [4 0 R] /Count 1 >>"),
        (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ] {
        offsets.push((n, body.len()));
        body.extend_from_slice(format!("{n} 0 obj\n{content}\nendobj\n").as_bytes());
    }
    let xref_at = body.len();
    // Object 3 is free, so the table is 0..=4 with a hole.
    body.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    body.extend_from_slice(format!("{:010} 00000 n \n", offsets[0].1).as_bytes());
    body.extend_from_slice(format!("{:010} 00000 n \n", offsets[1].1).as_bytes());
    body.extend_from_slice(b"0000000000 00001 f \n");
    body.extend_from_slice(format!("{:010} 00000 n \n", offsets[2].1).as_bytes());
    body.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
    );
    body
}

/// Build a dictionary from `(key, value)` pairs, for tests that need one
/// without the ceremony.
pub fn dict(pairs: &[(&str, Object)]) -> Dictionary {
    let mut d = Dictionary::new();
    for (k, v) in pairs {
        d.insert(Name::new(*k), v.clone());
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::object::ObjId;

    #[test]
    fn every_fixture_opens() {
        for (name, bytes) in [
            ("minimal_classic", minimal_classic()),
            ("classic_with_flate_content", classic_with_flate_content()),
            ("xref_stream_with_objstm", xref_stream_with_objstm()),
            ("two_revisions", two_revisions()),
        ] {
            let doc = Document::open(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(doc.catalog().is_ok(), "{name} has no usable catalog");
            assert!(doc.leniencies().is_empty(), "{name}: {:?}", doc.leniencies());
        }
    }

    #[test]
    fn encrypted_fixtures_decrypt_to_the_known_plaintext() {
        for cipher in [FixtureCipher::Rc4_128, FixtureCipher::Aes128] {
            let doc =
                Document::open(encrypted(cipher)).unwrap_or_else(|e| panic!("{cipher:?}: {e}"));
            assert!(doc.is_encrypted());

            // The stream must decrypt *and* decode to exactly what went in.
            let content = doc.decoded_stream(ObjId::new(4, 0)).unwrap();
            assert_eq!(
                String::from_utf8_lossy(&content),
                ENCRYPTED_FIXTURE_CONTENT,
                "{cipher:?} stream did not round-trip"
            );

            // And so must a string, via a different code path.
            let info = doc.get(ObjId::new(5, 0)).unwrap();
            let title = info.as_dict().unwrap().get("Title").unwrap().as_string().unwrap();
            assert_eq!(title.as_bytes(), ENCRYPTED_FIXTURE_TITLE.as_bytes(), "{cipher:?} string");
        }
    }

    #[test]
    fn an_encrypted_document_still_satisfies_i1() {
        for cipher in [FixtureCipher::Rc4_128, FixtureCipher::Aes128] {
            let original = encrypted(cipher);
            let doc = Document::open(original.clone()).unwrap();
            let out = crate::writer::save(&doc, &crate::writer::SaveOptions::default()).unwrap();
            assert_eq!(out.bytes, original, "{cipher:?}");
        }
    }

    #[test]
    fn a_saved_encrypted_document_can_still_decrypt_itself() {
        // Regression. A cross-reference stream is never encrypted, because it
        // has to be readable before the file key exists -- and when the trailer
        // lives inside one, its /ID goes with it. Encrypting those strings
        // changes the input to the key derivation, and the file then rejects
        // its own password. The symptom appears only on reopen, so nothing
        // before this point would have caught it.
        let cases = [
            ("classic", encrypted(FixtureCipher::Rc4_128)),
            ("classic-aes", encrypted(FixtureCipher::Aes128)),
            ("xref-stream", encrypted_xref_stream(FixtureCipher::Rc4_128)),
            ("xref-stream-aes", encrypted_xref_stream(FixtureCipher::Aes128)),
        ];
        for (cipher, bytes) in cases {
            for mode in
                [crate::writer::SaveOptions::default(), crate::writer::SaveOptions::full_rewrite()]
            {
                let mut doc = Document::open(bytes.clone()).unwrap();
                let page = doc.get(ObjId::new(3, 0)).unwrap();
                let mut d = page.as_dict().unwrap().clone();
                d.insert(Name::new("Rotate"), Object::Integer(90));
                doc.set(ObjId::new(3, 0), Object::Dictionary(d));

                let out = crate::writer::save(&doc, &mode).unwrap();
                let reopened = Document::open(out.bytes)
                    .unwrap_or_else(|e| panic!("{cipher} {:?}: {e}", out.mode));

                assert!(reopened.is_encrypted());
                let content = reopened.decoded_stream(ObjId::new(4, 0)).unwrap();
                assert_eq!(
                    String::from_utf8_lossy(&content),
                    ENCRYPTED_FIXTURE_CONTENT,
                    "{cipher} {:?}: content did not survive",
                    out.mode
                );
                let info = reopened.get(ObjId::new(5, 0)).unwrap();
                let title = info.as_dict().unwrap().get("Title").unwrap().as_string().unwrap();
                assert_eq!(title.as_bytes(), ENCRYPTED_FIXTURE_TITLE.as_bytes());
            }
        }
    }

    #[test]
    fn adversarial_fixtures_all_open_and_are_honest_about_it() {
        for (name, bytes) in adversarial() {
            let doc = Document::open(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(doc.catalog().is_ok(), "{name}: no usable catalog");

            // Anything that needed a lenient path must have said so. A silent
            // recovery is the failure mode this assertion exists to prevent --
            // and so is the reverse: a file the parser handles by the book must
            // not accumulate spurious leniencies.
            let must_be_lenient = matches!(
                name,
                "wrong-stream-length"
                    | "startxref-into-the-void"
                    | "no-eof-marker"
                    | "bytes-before-header"
            );
            if must_be_lenient {
                assert!(
                    !doc.leniencies().is_empty(),
                    "{name} took a lenient path without recording it"
                );
            } else {
                assert!(doc.leniencies().is_empty(), "{name}: {:?}", doc.leniencies());
            }
        }
    }

    #[test]
    fn a_reissued_object_resolves_at_its_new_generation() {
        let bytes =
            adversarial().into_iter().find(|(n, _)| *n == "reused-object-number").unwrap().1;
        let doc = Document::open(bytes).unwrap();
        match doc.xref().get(3) {
            Some(crate::xref::XrefEntry::InFile { generation: 1, .. }) => {}
            other => panic!("expected generation 1, got {other:?}"),
        }
        let page = doc.get(ObjId::new(3, 1)).unwrap();
        assert_eq!(
            page.as_dict().unwrap().get("Note").unwrap().as_string().unwrap().as_bytes(),
            b"generation one"
        );
    }

    #[test]
    fn odd_encodings_survive_a_no_op_save() {
        // The byte-preservation guarantee, end to end.
        let original =
            adversarial().into_iter().find(|(n, _)| *n == "odd-encodings-preserved").unwrap().1;
        let doc = Document::open(original.clone()).unwrap();

        let page = doc.get(ObjId::new(3, 0)).unwrap();
        let d = page.as_dict().unwrap();
        assert_eq!(d.get("Name").unwrap().as_string().unwrap().as_bytes(), b"escApes and (parens)");
        assert_eq!(d.get("Odd#Key").unwrap().as_string().unwrap().as_bytes(), b"linecontinuation");
        assert_eq!(d.get("Real").unwrap().as_f64(), Some(4.0));
        assert_eq!(d.get("Neg").unwrap().as_f64(), Some(-0.002));

        let out = crate::writer::save(&doc, &crate::writer::SaveOptions::default()).unwrap();
        assert_eq!(out.bytes, original);
    }

    #[test]
    fn indirect_length_is_resolved_not_scanned() {
        let bytes =
            adversarial().into_iter().find(|(n, _)| *n == "indirect-stream-length").unwrap().1;
        let doc = Document::open(bytes).unwrap();
        let content = doc.decoded_stream(ObjId::new(4, 0)).unwrap();
        assert!(String::from_utf8_lossy(&content).contains("indirect length"));
        assert!(
            !doc.leniencies().iter().any(|l| l.kind == crate::error::LeniencyKind::LengthRecovered),
            "an indirect /Length that resolves is not a leniency"
        );
    }

    #[test]
    fn two_revisions_exposes_both() {
        let doc = Document::open(two_revisions()).unwrap();
        assert_eq!(doc.revisions().len(), 2);
        let page = doc.get(ObjId::new(3, 0)).unwrap();
        assert_eq!(
            page.as_dict().unwrap().get("Rotate").and_then(Object::as_i64),
            Some(90),
            "the newer revision must win"
        );
    }
}
