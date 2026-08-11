//! Stream filters and predictors. ISO 32000-1 §7.4, spec 5.4.
//!
//! # Re-encoding policy
//!
//! When a stream's content is unchanged, the writer re-emits the original raw
//! bytes verbatim; nothing here runs at all. Encoding is reached only for a
//! stream whose decoded content was actually replaced, and then it re-applies
//! the *same* filter chain. Rasura never helpfully recompresses an untouched
//! stream -- that would churn bytes the caller never asked to change and break
//! the locality property in spec 2.
//!
//! # Image filters
//!
//! `DCTDecode`, `JPXDecode`, `JBIG2Decode` and `CCITTFaxDecode` are
//! pass-through: pixel data stays in its original codec unless an image-editing
//! operation demands otherwise. A chain like `[/FlateDecode /DCTDecode]` decodes
//! the Flate layer and stops, leaving JPEG bytes.

use crate::error::{CosError, Result};
use crate::object::{Dictionary, Name, Object, hex_val, is_whitespace};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterKind {
    Flate,
    Lzw,
    AsciiHex,
    Ascii85,
    RunLength,
    Dct,
    Jpx,
    Jbig2,
    CcittFax,
    Crypt,
    Unknown(String),
}

impl FilterKind {
    pub fn from_name(name: &Name) -> Self {
        // Both the full names and the inline-image abbreviations of
        // ISO 32000-1 Table 93.
        match name.as_bytes() {
            b"FlateDecode" | b"Fl" => FilterKind::Flate,
            b"LZWDecode" | b"LZW" => FilterKind::Lzw,
            b"ASCIIHexDecode" | b"AHx" => FilterKind::AsciiHex,
            b"ASCII85Decode" | b"A85" => FilterKind::Ascii85,
            b"RunLengthDecode" | b"RL" => FilterKind::RunLength,
            b"DCTDecode" | b"DCT" => FilterKind::Dct,
            b"JPXDecode" => FilterKind::Jpx,
            b"JBIG2Decode" => FilterKind::Jbig2,
            b"CCITTFaxDecode" | b"CCF" => FilterKind::CcittFax,
            b"Crypt" => FilterKind::Crypt,
            other => FilterKind::Unknown(String::from_utf8_lossy(other).into_owned()),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            FilterKind::Flate => "FlateDecode",
            FilterKind::Lzw => "LZWDecode",
            FilterKind::AsciiHex => "ASCIIHexDecode",
            FilterKind::Ascii85 => "ASCII85Decode",
            FilterKind::RunLength => "RunLengthDecode",
            FilterKind::Dct => "DCTDecode",
            FilterKind::Jpx => "JPXDecode",
            FilterKind::Jbig2 => "JBIG2Decode",
            FilterKind::CcittFax => "CCITTFaxDecode",
            FilterKind::Crypt => "Crypt",
            FilterKind::Unknown(s) => s,
        }
    }

    /// True for filters whose payload Rasura deliberately leaves encoded.
    pub fn is_image_passthrough(&self) -> bool {
        matches!(self, FilterKind::Dct | FilterKind::Jpx | FilterKind::Jbig2 | FilterKind::CcittFax)
    }

    pub fn can_encode(&self) -> bool {
        matches!(
            self,
            FilterKind::Flate | FilterKind::AsciiHex | FilterKind::Ascii85 | FilterKind::RunLength
        )
    }
}

/// One `/Filter` entry with its matching `/DecodeParms`.
#[derive(Debug, Clone)]
pub struct FilterStep {
    pub kind: FilterKind,
    pub parms: Option<Dictionary>,
}

#[derive(Debug, Clone, Default)]
pub struct FilterChain {
    pub steps: Vec<FilterStep>,
}

impl FilterChain {
    /// Build from already-resolved `/Filter` and `/DecodeParms` objects. Both
    /// may be a single value or an array; a shorter `/DecodeParms` array pads
    /// with `None`.
    pub fn build(filter: Option<&Object>, parms: Option<&Object>) -> Self {
        let names: Vec<&Name> = match filter {
            Some(Object::Name(n)) => vec![n],
            Some(Object::Array(a)) => a.iter().filter_map(Object::as_name).collect(),
            _ => Vec::new(),
        };
        let parm_list: Vec<Option<Dictionary>> = match parms {
            Some(Object::Dictionary(d)) => vec![Some(d.clone())],
            Some(Object::Array(a)) => a
                .iter()
                .map(|o| match o {
                    Object::Dictionary(d) => Some(d.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let steps = names
            .into_iter()
            .enumerate()
            .map(|(i, n)| FilterStep {
                kind: FilterKind::from_name(n),
                parms: parm_list.get(i).cloned().flatten(),
            })
            .collect();
        FilterChain { steps }
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// How many leading steps Rasura actually decodes before hitting a
    /// pass-through image filter.
    pub fn decodable_prefix(&self) -> usize {
        self.steps.iter().take_while(|s| !s.kind.is_image_passthrough()).count()
    }

    /// True when the whole chain decodes to plain bytes.
    pub fn fully_decodable(&self) -> bool {
        self.decodable_prefix() == self.steps.len()
    }
}

/// Result of decoding: the bytes, plus how many chain steps were applied.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub data: Vec<u8>,
    pub steps_applied: usize,
}

/// Apply the chain in order, stopping at the first pass-through image filter.
///
/// `Crypt` steps are no-ops here: decryption already happened at object load
/// time, driven by the document's security handler.
pub fn decode(chain: &FilterChain, raw: &[u8]) -> Result<Decoded> {
    let mut data = raw.to_vec();
    let mut applied = 0usize;
    for step in &chain.steps {
        if step.kind.is_image_passthrough() {
            break;
        }
        data = match step.kind {
            FilterKind::Flate => flate_decode(&data)?,
            FilterKind::Lzw => lzw_decode(&data, early_change(step.parms.as_ref()))?,
            FilterKind::AsciiHex => ascii_hex_decode(&data),
            FilterKind::Ascii85 => ascii85_decode(&data)?,
            FilterKind::RunLength => run_length_decode(&data),
            FilterKind::Crypt => data,
            FilterKind::Unknown(ref n) => return Err(CosError::UnsupportedFilter(n.clone())),
            _ => unreachable!("pass-through handled above"),
        };
        if matches!(step.kind, FilterKind::Flate | FilterKind::Lzw) {
            data = apply_predictor(&data, step.parms.as_ref())?;
        }
        applied += 1;
    }
    Ok(Decoded { data, steps_applied: applied })
}

/// Re-encode `data` through the first `steps` of the chain, in reverse.
///
/// Only reached for a stream whose content genuinely changed.
pub fn encode(chain: &FilterChain, data: &[u8], steps: usize) -> Result<Vec<u8>> {
    let mut out = data.to_vec();
    for step in chain.steps.iter().take(steps).rev() {
        if matches!(step.kind, FilterKind::Flate | FilterKind::Lzw) {
            out = undo_predictor(&out, step.parms.as_ref())?;
        }
        out = match step.kind {
            FilterKind::Flate => flate_encode(&out, flate_level(step.parms.as_ref())),
            FilterKind::AsciiHex => ascii_hex_encode(&out),
            FilterKind::Ascii85 => ascii85_encode(&out),
            FilterKind::RunLength => run_length_encode(&out),
            FilterKind::Crypt => out,
            // Spec 5.4 lists LZW as decode-only: nobody should ship new LZW.
            ref other => {
                return Err(CosError::FilterFailed {
                    filter: other.name().to_string(),
                    reason: "encoding is not supported for this filter".into(),
                });
            }
        };
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Flate
// ---------------------------------------------------------------------------

/// A well-formed zlib header: CM 8, and the two header bytes a multiple of 31.
///
/// Cheap, and it is the difference between "a deflate stream that got cut
/// short" and "these are not deflate bytes at all".
fn has_zlib_header(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] & 0x0f == 8 && u16::from_be_bytes([data[0], data[1]]) % 31 == 0
}

/// Inflate, tolerating the damage patterns that show up constantly in real
/// files -- but not so tolerantly that noise decodes to noise.
///
/// # Why the strictness matters
///
/// Arbitrary bytes very often begin a valid *raw* deflate block: `0x2b` is
/// BFINAL set with a fixed-Huffman block, and inflating random data from there
/// happily yields hundreds of bytes of garbage. An earlier version of this
/// function accepted exactly that, so a stream that failed to *decrypt* came
/// back as plausible-looking rubbish instead of an error. Text extraction would
/// then have produced nonsense with nothing to indicate anything was wrong --
/// the silent degradation spec 2 forbids. It took a corpus walk to notice,
/// because the garbage was the right sort of length.
///
/// So: a genuine zlib stream may be truncated and we keep what we got, because
/// that damage is real and common. Anything *without* a zlib header must
/// inflate cleanly to the end of the stream before it is believed.
fn flate_decode(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }

    // 1. A proper zlib stream, possibly cut short.
    if has_zlib_header(data)
        && let Ok((out, _ended, _)) = inflate(data, true)
        && !out.is_empty()
    {
        return Ok(out);
    }

    // 2. The same, behind stray leading whitespace, which some producers emit
    //    between `stream` and the data.
    let trimmed = {
        let mut i = 0;
        while i < data.len() && is_whitespace(data[i]) {
            i += 1;
        }
        &data[i..]
    };
    if trimmed.len() != data.len()
        && has_zlib_header(trimmed)
        && let Ok((out, _ended, _)) = inflate(trimmed, true)
        && !out.is_empty()
    {
        return Ok(out);
    }

    // 3. Headerless raw deflate, which a few producers really do emit.
    //
    //    Two conditions, and both are needed. Reaching the end marker is not
    //    enough on its own: a fixed-Huffman block decoded from noise hits an
    //    end-of-block symbol quickly and reports a clean finish having eaten a
    //    handful of bytes. Requiring that it also *consumed the stream* is what
    //    separates real headerless deflate from an accident.
    if let Ok((out, true, consumed)) = inflate(data, false)
        && !out.is_empty()
        && consumed + 2 >= data.len()
    {
        return Ok(out);
    }

    Err(CosError::FilterFailed {
        filter: "FlateDecode".into(),
        reason: "stream is not recoverable deflate data".into(),
    })
}

/// Returns the output, whether the stream reached its end cleanly, and how many
/// input bytes were consumed.
fn inflate(data: &[u8], zlib_header: bool) -> std::result::Result<(Vec<u8>, bool, usize), ()> {
    use flate2::{Decompress, FlushDecompress, Status};
    let mut d = Decompress::new(zlib_header);
    // Grow geometrically. A hard ceiling keeps a decompression bomb from
    // exhausting memory on adversarial input (spec 14.4).
    const MAX_OUT: usize = 512 * 1024 * 1024;
    let mut out = Vec::with_capacity((data.len() * 4).clamp(1024, 1 << 20));
    loop {
        let before_out = out.len();
        if out.capacity() == out.len() {
            let grow = out.capacity().max(1024);
            out.reserve(grow);
        }
        let before_in = d.total_in() as usize;
        let status =
            d.decompress_vec(&data[before_in.min(data.len())..], &mut out, FlushDecompress::None);
        match status {
            Ok(Status::StreamEnd) => {
                let consumed = d.total_in() as usize;
                return Ok((out, true, consumed));
            }
            Ok(Status::Ok) | Ok(Status::BufError) => {
                let consumed = d.total_in() as usize;
                let progressed = out.len() > before_out || consumed > before_in;
                if !progressed {
                    // Out of input with no end marker: a truncated stream.
                    return if out.is_empty() { Err(()) } else { Ok((out, false, consumed)) };
                }
                if out.len() > MAX_OUT {
                    return Ok((out, false, consumed));
                }
            }
            Err(_) => {
                let consumed = d.total_in() as usize;
                return if out.is_empty() { Err(()) } else { Ok((out, false, consumed)) };
            }
        }
    }
}

fn flate_encode(data: &[u8], level: u32) -> Vec<u8> {
    use flate2::{Compress, Compression, FlushCompress};
    let mut c = Compress::new(Compression::new(level), true);
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    let mut consumed = 0usize;
    loop {
        if out.capacity() == out.len() {
            out.reserve(out.capacity().max(1024));
        }
        let before = out.len();
        let status = c.compress_vec(&data[consumed..], &mut out, FlushCompress::Finish);
        consumed = c.total_in() as usize;
        match status {
            Ok(flate2::Status::StreamEnd) => return out,
            Ok(_) => {
                if out.len() == before && consumed >= data.len() {
                    return out;
                }
            }
            Err(_) => return out,
        }
    }
}

/// Spec 5.4: match the original compression level where detectable. The zlib
/// FLEVEL bits in byte 1 are the only signal a PDF carries.
fn flate_level(_parms: Option<&Dictionary>) -> u32 {
    6
}

/// Recover the compression level a stream was written with, from its zlib
/// header. Used by the writer to keep re-encoded streams close to the original.
pub fn detect_flate_level(raw: &[u8]) -> Option<u32> {
    if raw.len() < 2 {
        return None;
    }
    let cmf = raw[0];
    let flg = raw[1];
    // CM must be 8 (deflate) and the two header bytes must be a multiple of 31.
    if cmf & 0x0f != 8 || u16::from_be_bytes([cmf, flg]) % 31 != 0 {
        return None;
    }
    // FLEVEL: 0 fastest, 1 fast, 2 default, 3 maximum.
    Some(match flg >> 6 {
        0 => 1,
        1 => 3,
        2 => 6,
        _ => 9,
    })
}

// ---------------------------------------------------------------------------
// LZW
// ---------------------------------------------------------------------------

fn early_change(parms: Option<&Dictionary>) -> bool {
    parms
        .and_then(|d| d.get("EarlyChange"))
        .and_then(Object::as_i64)
        .map(|v| v != 0)
        .unwrap_or(true)
}

fn lzw_decode(data: &[u8], early: bool) -> Result<Vec<u8>> {
    use weezl::{BitOrder, decode::Decoder};
    // `/EarlyChange 1` (the default) switches to a longer code one symbol early,
    // which is weezl's TIFF variant. `/EarlyChange 0` is the plain variant.
    let mut dec = if early {
        Decoder::with_tiff_size_switch(BitOrder::Msb, 8)
    } else {
        Decoder::new(BitOrder::Msb, 8)
    };
    let mut out = Vec::with_capacity(data.len() * 3);
    let result = dec.into_stream(&mut out).decode_all(data);
    match result.status {
        Ok(()) => Ok(out),
        // Truncated LZW is common; keep what decoded.
        Err(e) if !out.is_empty() => {
            let _ = e;
            Ok(out)
        }
        Err(e) => Err(CosError::FilterFailed { filter: "LZWDecode".into(), reason: e.to_string() }),
    }
}

// ---------------------------------------------------------------------------
// ASCIIHex
// ---------------------------------------------------------------------------

fn ascii_hex_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut hi: Option<u8> = None;
    for &b in data {
        if b == b'>' {
            break;
        }
        let Some(v) = hex_val(b) else { continue };
        match hi {
            None => hi = Some(v),
            Some(h) => {
                out.push(h << 4 | v);
                hi = None;
            }
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    out
}

fn ascii_hex_encode(data: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::with_capacity(data.len() * 2 + 1);
    for (i, &b) in data.iter().enumerate() {
        if i > 0 && i % 40 == 0 {
            out.push(b'\n');
        }
        out.push(HEX[(b >> 4) as usize]);
        out.push(HEX[(b & 0x0f) as usize]);
    }
    out.push(b'>');
    out
}

// ---------------------------------------------------------------------------
// ASCII85
// ---------------------------------------------------------------------------

fn ascii85_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut src = data;
    // The `<~` prefix is optional in PDF and mandatory in some other dialects.
    if src.starts_with(b"<~") {
        src = &src[2..];
    }
    let mut out = Vec::with_capacity(src.len() * 4 / 5 + 4);
    let mut group = [0u8; 5];
    let mut n = 0usize;

    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        i += 1;
        if is_whitespace(b) {
            continue;
        }
        if b == b'~' {
            break;
        }
        if b == b'z' && n == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&b) {
            return Err(CosError::FilterFailed {
                filter: "ASCII85Decode".into(),
                reason: format!("byte {b:#04x} is outside the base-85 alphabet"),
            });
        }
        group[n] = b - b'!';
        n += 1;
        if n == 5 {
            let v = group.iter().fold(0u32, |acc, &d| acc.wrapping_mul(85).wrapping_add(d as u32));
            out.extend_from_slice(&v.to_be_bytes());
            n = 0;
        }
    }

    if n > 0 {
        if n == 1 {
            return Err(CosError::FilterFailed {
                filter: "ASCII85Decode".into(),
                reason: "final group has a single character".into(),
            });
        }
        // Pad the partial group with the maximum digit, then keep n-1 bytes.
        for slot in group.iter_mut().skip(n) {
            *slot = 84;
        }
        let v = group.iter().fold(0u32, |acc, &d| acc.wrapping_mul(85).wrapping_add(d as u32));
        out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
    }
    Ok(out)
}

fn ascii85_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 5 / 4 + 4);
    let mut col = 0usize;
    let push = |out: &mut Vec<u8>, col: &mut usize, b: u8| {
        out.push(b);
        *col += 1;
        if *col >= 75 {
            out.push(b'\n');
            *col = 0;
        }
    };
    for chunk in data.chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        let v = u32::from_be_bytes(word);
        if chunk.len() == 4 && v == 0 {
            push(&mut out, &mut col, b'z');
            continue;
        }
        let mut digits = [0u8; 5];
        let mut acc = v;
        for slot in digits.iter_mut().rev() {
            *slot = (acc % 85) as u8 + b'!';
            acc /= 85;
        }
        for &d in digits.iter().take(chunk.len() + 1) {
            push(&mut out, &mut col, d);
        }
    }
    out.extend_from_slice(b"~>");
    out
}

// ---------------------------------------------------------------------------
// RunLength
// ---------------------------------------------------------------------------

fn run_length_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    let mut i = 0usize;
    while i < data.len() {
        let len = data[i];
        i += 1;
        match len {
            0..=127 => {
                let n = len as usize + 1;
                let end = (i + n).min(data.len());
                out.extend_from_slice(&data[i..end]);
                i = end;
            }
            128 => break, // EOD
            129..=255 => {
                let Some(&b) = data.get(i) else { break };
                out.extend(std::iter::repeat_n(b, 257 - len as usize));
                i += 1;
            }
        }
    }
    out
}

fn run_length_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 64 + 2);
    let mut i = 0usize;
    while i < data.len() {
        // Count the run at `i`.
        let mut run = 1usize;
        while i + run < data.len() && data[i + run] == data[i] && run < 128 {
            run += 1;
        }
        if run >= 2 {
            out.push((257 - run) as u8);
            out.push(data[i]);
            i += run;
        } else {
            // Gather literals until a run of 2+ starts or 128 bytes accumulate.
            let start = i;
            while i < data.len()
                && i - start < 128
                && !(i + 1 < data.len() && data[i] == data[i + 1])
            {
                i += 1;
            }
            if i == start {
                i += 1;
            }
            out.push((i - start - 1) as u8);
            out.extend_from_slice(&data[start..i]);
        }
    }
    out.push(128);
    out
}

// ---------------------------------------------------------------------------
// Predictors (ISO 32000-1 Table 10)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct PredictorSpec {
    predictor: i64,
    colors: usize,
    bpc: usize,
    columns: usize,
}

impl PredictorSpec {
    fn read(parms: Option<&Dictionary>) -> Self {
        let get =
            |k: &str, d: i64| parms.and_then(|p| p.get(k)).and_then(Object::as_i64).unwrap_or(d);
        PredictorSpec {
            predictor: get("Predictor", 1),
            colors: get("Colors", 1).clamp(1, 32) as usize,
            bpc: get("BitsPerComponent", 8).clamp(1, 16) as usize,
            columns: get("Columns", 1).max(1) as usize,
        }
    }

    /// Bytes per pixel, rounded up; the PNG "bpp" offset.
    fn bpp(&self) -> usize {
        (self.colors * self.bpc).div_ceil(8).max(1)
    }

    fn row_bytes(&self) -> usize {
        (self.columns * self.colors * self.bpc).div_ceil(8)
    }
}

fn apply_predictor(data: &[u8], parms: Option<&Dictionary>) -> Result<Vec<u8>> {
    let spec = PredictorSpec::read(parms);
    match spec.predictor {
        p if p < 2 => Ok(data.to_vec()),
        2 => tiff_predictor_decode(data, &spec),
        10..=15 => png_predictor_decode(data, &spec),
        other => Err(CosError::FilterFailed {
            filter: "Predictor".into(),
            reason: format!("unsupported /Predictor {other}"),
        }),
    }
}

fn undo_predictor(data: &[u8], parms: Option<&Dictionary>) -> Result<Vec<u8>> {
    let spec = PredictorSpec::read(parms);
    match spec.predictor {
        p if p < 2 => Ok(data.to_vec()),
        2 => tiff_predictor_encode(data, &spec),
        // Re-encoding uses PNG Up, which every decoder handles and which
        // compresses about as well as the adaptive choice for text-like data.
        10..=15 => png_predictor_encode(data, &spec),
        other => Err(CosError::FilterFailed {
            filter: "Predictor".into(),
            reason: format!("unsupported /Predictor {other}"),
        }),
    }
}

fn png_predictor_decode(data: &[u8], spec: &PredictorSpec) -> Result<Vec<u8>> {
    let row_bytes = spec.row_bytes();
    let bpp = spec.bpp();
    if row_bytes == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row_bytes];
    let mut cur = vec![0u8; row_bytes];
    let mut i = 0usize;

    while i < data.len() {
        let ft = data[i];
        i += 1;
        let avail = (data.len() - i).min(row_bytes);
        cur[..avail].copy_from_slice(&data[i..i + avail]);
        // A truncated final row is zero-filled rather than dropped.
        cur[avail..].fill(0);
        i += avail;

        for x in 0..row_bytes {
            let a = if x >= bpp { cur[x - bpp] } else { 0 };
            let b = prev[x];
            let c = if x >= bpp { prev[x - bpp] } else { 0 };
            cur[x] = match ft {
                0 => cur[x],
                1 => cur[x].wrapping_add(a),
                2 => cur[x].wrapping_add(b),
                3 => cur[x].wrapping_add((((a as u16) + (b as u16)) / 2) as u8),
                4 => cur[x].wrapping_add(paeth(a, b, c)),
                other => {
                    return Err(CosError::FilterFailed {
                        filter: "Predictor".into(),
                        reason: format!("unknown PNG filter type {other}"),
                    });
                }
            };
        }
        out.extend_from_slice(&cur);
        std::mem::swap(&mut prev, &mut cur);
        if avail < row_bytes {
            break;
        }
    }
    Ok(out)
}

fn png_predictor_encode(data: &[u8], spec: &PredictorSpec) -> Result<Vec<u8>> {
    let row_bytes = spec.row_bytes();
    if row_bytes == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity(data.len() + data.len() / row_bytes + 1);
    let mut prev = vec![0u8; row_bytes];
    for row in data.chunks(row_bytes) {
        out.push(2); // PNG Up
        for (x, &above) in prev.iter().enumerate() {
            let v = row.get(x).copied().unwrap_or(0);
            out.push(v.wrapping_sub(above));
        }
        let n = row.len().min(row_bytes);
        prev.fill(0);
        prev[..n].copy_from_slice(&row[..n]);
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let pa = (p - a as i16).abs();
    let pb = (p - b as i16).abs();
    let pc = (p - c as i16).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn tiff_predictor_decode(data: &[u8], spec: &PredictorSpec) -> Result<Vec<u8>> {
    let row_bytes = spec.row_bytes();
    let mut out = data.to_vec();
    match spec.bpc {
        8 => {
            for row in out.chunks_mut(row_bytes) {
                for x in spec.colors..row.len() {
                    row[x] = row[x].wrapping_add(row[x - spec.colors]);
                }
            }
            Ok(out)
        }
        16 => {
            let stride = spec.colors * 2;
            for row in out.chunks_mut(row_bytes) {
                let mut x = stride;
                while x + 1 < row.len() {
                    let prev = u16::from_be_bytes([row[x - stride], row[x - stride + 1]]);
                    let cur = u16::from_be_bytes([row[x], row[x + 1]]);
                    let v = cur.wrapping_add(prev);
                    row[x] = (v >> 8) as u8;
                    row[x + 1] = v as u8;
                    x += 2;
                }
            }
            Ok(out)
        }
        other => Err(CosError::FilterFailed {
            filter: "Predictor".into(),
            reason: format!("TIFF predictor with {other} bits per component is not supported"),
        }),
    }
}

fn tiff_predictor_encode(data: &[u8], spec: &PredictorSpec) -> Result<Vec<u8>> {
    let row_bytes = spec.row_bytes();
    let mut out = data.to_vec();
    match spec.bpc {
        8 => {
            for row in out.chunks_mut(row_bytes) {
                for x in (spec.colors..row.len()).rev() {
                    row[x] = row[x].wrapping_sub(row[x - spec.colors]);
                }
            }
            Ok(out)
        }
        16 => {
            let stride = spec.colors * 2;
            for row in out.chunks_mut(row_bytes) {
                let mut x = if row.len() % 2 == 0 { row.len() } else { row.len() - 1 };
                while x >= stride + 2 {
                    x -= 2;
                    let prev = u16::from_be_bytes([row[x - stride], row[x - stride + 1]]);
                    let cur = u16::from_be_bytes([row[x], row[x + 1]]);
                    let v = cur.wrapping_sub(prev);
                    row[x] = (v >> 8) as u8;
                    row[x + 1] = v as u8;
                }
            }
            Ok(out)
        }
        other => Err(CosError::FilterFailed {
            filter: "Predictor".into(),
            reason: format!("TIFF predictor with {other} bits per component is not supported"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Name;

    fn parms(pairs: &[(&str, i64)]) -> Dictionary {
        let mut d = Dictionary::new();
        for (k, v) in pairs {
            d.insert(Name::new(*k), Object::Integer(*v));
        }
        d
    }

    #[test]
    fn flate_round_trips() {
        let data = b"the quick brown fox jumps over the lazy dog".repeat(40);
        let enc = flate_encode(&data, 6);
        assert_eq!(flate_decode(&enc).unwrap(), data);
    }

    #[test]
    fn flate_keeps_what_it_got_from_a_truncated_stream() {
        // Truncated streams are endemic in the wild. Whatever inflated before
        // the data ran out is kept: a short stream beats a hard failure, and
        // the caller can see the shortfall.
        let data: Vec<u8> =
            (0..40_000u32).flat_map(|i| format!("line {i} of some prose\n").into_bytes()).collect();
        let enc = flate_encode(&data, 6);
        let truncated = &enc[..enc.len() * 3 / 5];
        let out = flate_decode(truncated).unwrap();
        assert!(!out.is_empty());
        assert!(out.len() < data.len());
        assert!(data.starts_with(&out));
    }

    #[test]
    fn noise_is_rejected_rather_than_decoded_to_noise() {
        // Regression, found by walking the corpus. Arbitrary bytes frequently
        // begin a valid raw-deflate block -- 0x2b is BFINAL with fixed Huffman
        // -- and inflating from there yields hundreds of bytes of garbage. When
        // this was accepted, a stream that failed to decrypt came back as
        // plausible-looking rubbish instead of an error.
        let noise: [&[u8]; 4] = [
            &[0x2b, 0xc7, 0x7d, 0x1e, 0x9a, 0x44, 0x01, 0xf3],
            &[0x71, 0x8d, 0xdb, 0x37, 0x22, 0x11, 0x08, 0x99],
            &[0x9e, 0x1b, 0x4c, 0x5f, 0x00, 0xab, 0xcd, 0xef],
            &[0x83, 0x8b, 0x4c, 0x24, 0x77, 0x31, 0x62, 0x5a],
        ];
        for bytes in noise {
            assert!(
                flate_decode(bytes).is_err(),
                "{bytes:02x?} is not deflate data and must not decode"
            );
        }
    }

    #[test]
    fn partial_output_is_believed_only_behind_a_zlib_header() {
        let data = b"real content here".repeat(200);
        let good = flate_encode(&data, 6);
        assert!(has_zlib_header(&good));

        // Truncated but properly headered: keep what inflated. This damage is
        // real and common, and a short stream beats no stream.
        let truncated = &good[..good.len() * 3 / 5];
        let partial = flate_decode(truncated).expect("a headered truncation is recoverable");
        assert!(!partial.is_empty() && partial.len() < data.len());

        // The same truncation with the header destroyed is indistinguishable
        // from noise, and must be refused rather than half-decoded.
        let mut headerless = truncated.to_vec();
        headerless[0] = 0x2b;
        assert!(!has_zlib_header(&headerless));
        assert!(
            flate_decode(&headerless).is_err(),
            "partial output with no zlib header is not evidence of anything"
        );
    }

    #[test]
    fn genuine_headerless_raw_deflate_still_decodes() {
        // A few producers really do emit raw deflate, so a *complete* raw
        // stream is accepted -- it is only partial ones that are refused.
        use flate2::{Compress, Compression, FlushCompress};
        let data = b"headerless but genuine".repeat(20);
        let mut c = Compress::new(Compression::new(6), false);
        let mut raw = Vec::with_capacity(1024);
        c.compress_vec(&data, &mut raw, FlushCompress::Finish).unwrap();
        assert!(!has_zlib_header(&raw), "this fixture must have no zlib header");
        assert_eq!(flate_decode(&raw).unwrap(), data);
    }

    #[test]
    fn ascii_hex_round_trips_and_stops_at_gt() {
        let out = ascii_hex_decode(b"48656C6C6F>ignored");
        assert_eq!(out, b"Hello");
        assert_eq!(ascii_hex_decode(&ascii_hex_encode(b"Hello")), b"Hello");
    }

    #[test]
    fn ascii_hex_pads_odd_digit() {
        assert_eq!(ascii_hex_decode(b"4A5>"), &[0x4a, 0x50]);
    }

    #[test]
    fn ascii85_round_trips_including_z_shorthand() {
        let data = b"\0\0\0\0Man is distinguished\0\0\0\0";
        let enc = ascii85_encode(data);
        assert!(enc.windows(1).any(|w| w == b"z"));
        assert_eq!(ascii85_decode(&enc).unwrap(), data);
    }

    #[test]
    fn ascii85_handles_partial_final_group() {
        for len in 1..=9 {
            let data: Vec<u8> =
                (0..len as u8).map(|b| b.wrapping_mul(37).wrapping_add(1)).collect();
            let enc = ascii85_encode(&data);
            assert_eq!(ascii85_decode(&enc).unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn ascii85_accepts_the_optional_prefix() {
        assert_eq!(ascii85_decode(b"<~87cURD]~>").unwrap(), b"Hello");
    }

    #[test]
    fn run_length_round_trips() {
        for data in [
            b"aaaaaaaaaabbbbbbcdefgh".to_vec(),
            vec![7u8; 500],
            (0u8..=255).collect::<Vec<_>>(),
            Vec::new(),
        ] {
            let enc = run_length_encode(&data);
            assert_eq!(run_length_decode(&enc), data);
        }
    }

    #[test]
    fn png_predictor_round_trips() {
        let spec =
            parms(&[("Predictor", 12), ("Colors", 3), ("BitsPerComponent", 8), ("Columns", 4)]);
        let raw: Vec<u8> = (0..(4 * 3 * 5) as u8).collect();
        let encoded = undo_predictor(&raw, Some(&spec)).unwrap();
        assert_eq!(apply_predictor(&encoded, Some(&spec)).unwrap(), raw);
    }

    #[test]
    fn png_predictor_handles_every_filter_type() {
        // Hand-build one row per filter type and check none of them error.
        let spec = PredictorSpec { predictor: 12, colors: 1, bpc: 8, columns: 4 };
        for ft in 0u8..=4 {
            let data = vec![ft, 1, 2, 3, 4];
            assert!(png_predictor_decode(&data, &spec).is_ok(), "filter {ft}");
        }
        assert!(png_predictor_decode(&[9, 1, 2, 3, 4], &spec).is_err());
    }

    #[test]
    fn tiff_predictor_round_trips_8bpc() {
        let spec =
            parms(&[("Predictor", 2), ("Colors", 3), ("BitsPerComponent", 8), ("Columns", 4)]);
        let raw: Vec<u8> = (0..(4 * 3 * 5) as u8).map(|b| b.wrapping_mul(11)).collect();
        let encoded = undo_predictor(&raw, Some(&spec)).unwrap();
        assert_eq!(apply_predictor(&encoded, Some(&spec)).unwrap(), raw);
    }

    #[test]
    fn chain_stops_at_an_image_filter() {
        let filter = Object::Array(vec![Object::name("FlateDecode"), Object::name("DCTDecode")]);
        let chain = FilterChain::build(Some(&filter), None);
        assert_eq!(chain.decodable_prefix(), 1);
        assert!(!chain.fully_decodable());

        let jpeg_bytes = b"\xff\xd8\xff\xe0 pretend JPEG";
        let raw = flate_encode(jpeg_bytes, 6);
        let out = decode(&chain, &raw).unwrap();
        assert_eq!(out.data, jpeg_bytes);
        assert_eq!(out.steps_applied, 1);
    }

    #[test]
    fn unknown_filter_is_reported_not_guessed() {
        let filter = Object::name("MadeUpDecode");
        let chain = FilterChain::build(Some(&filter), None);
        let err = decode(&chain, b"data").unwrap_err();
        assert!(matches!(err, CosError::UnsupportedFilter(ref n) if n == "MadeUpDecode"));
    }

    #[test]
    fn full_chain_round_trips_through_encode() {
        let filter =
            Object::Array(vec![Object::name("ASCII85Decode"), Object::name("FlateDecode")]);
        let chain = FilterChain::build(Some(&filter), None);
        let data = b"content stream bytes ".repeat(30);
        let encoded = encode(&chain, &data, 2).unwrap();
        assert_eq!(decode(&chain, &encoded).unwrap().data, data);
    }
}
