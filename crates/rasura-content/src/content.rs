//! The logical content stream. Spec 6.4.
//!
//! A page's `/Contents` may be a single stream or an array of them. ISO 32000-1
//! §7.8.2 says the array's streams concatenate as if they were one, and that
//! the division between them is "logically a single stream" -- but with the
//! important caveat that a lexical token may not span the boundary. Producers
//! split content mid-page for all sorts of reasons, and some split it *between*
//! every operator.
//!
//! Two consequences shape this module:
//!
//! 1. A whitespace separator is inserted between parts. Without it,
//!    `...Tj` followed by `72 700 Td` would lex as `Tj72`.
//! 2. The mapping from logical offset back to `(stream index, offset)` is
//!    retained. Spans in `Op` are logical; the edit layer needs to know which
//!    *object* to patch, and an operator near a boundary must not send its
//!    replacement bytes into the wrong stream.

use rasura_cos::document::Document;
use rasura_cos::{CosError, ObjId, Object, Result};
use std::ops::Range;

/// The separator inserted between concatenated parts. A single newline: enough
/// to break a token, and cheap to account for in the offset map.
const SEPARATOR: u8 = b'\n';

/// One source stream within a logical content stream.
#[derive(Debug, Clone)]
pub struct ContentPart {
    /// The stream object this came from, which is what a patch has to target.
    pub id: ObjId,
    /// Where this part's bytes live in the logical buffer.
    pub range: Range<usize>,
}

/// A page's content streams concatenated, with the map back to their sources.
#[derive(Debug, Clone, Default)]
pub struct LogicalContent {
    data: Vec<u8>,
    parts: Vec<ContentPart>,
}

impl LogicalContent {
    /// Build from a resolved `/Contents` value.
    ///
    /// A stream that will not decode is skipped and reported rather than
    /// aborting the page: one broken content stream out of five should still
    /// leave four pages' worth of readable text.
    pub fn build(doc: &Document, contents: &Object) -> (LogicalContent, Vec<CosError>) {
        let mut out = LogicalContent::default();
        let mut errors = Vec::new();

        let ids: Vec<ObjId> = match contents {
            Object::Reference(id) => {
                // A reference to either a stream or an array of them.
                match doc.get(*id) {
                    Ok(obj) => match &*obj {
                        Object::Array(items) => {
                            items.iter().filter_map(Object::as_reference).collect()
                        }
                        Object::Stream(_) => vec![*id],
                        _ => Vec::new(),
                    },
                    Err(e) => {
                        errors.push(e);
                        Vec::new()
                    }
                }
            }
            Object::Array(items) => items.iter().filter_map(Object::as_reference).collect(),
            _ => Vec::new(),
        };

        for id in ids {
            match doc.decoded_stream(id) {
                Ok(data) => out.push_part(id, &data),
                Err(e) => errors.push(e),
            }
        }
        (out, errors)
    }

    /// Build from a single already-decoded stream, for form XObjects, tiling
    /// patterns and Type 3 glyph procedures -- which are content streams too and
    /// go through the same machinery.
    pub fn single(id: ObjId, data: &[u8]) -> LogicalContent {
        let mut out = LogicalContent::default();
        out.push_part(id, data);
        out
    }

    fn push_part(&mut self, id: ObjId, data: &[u8]) {
        if !self.parts.is_empty() {
            // Spec 6.4: a token may not span the boundary.
            self.data.push(SEPARATOR);
        }
        let start = self.data.len();
        self.data.extend_from_slice(data);
        self.parts.push(ContentPart { id, range: start..self.data.len() });
    }

    /// The concatenated bytes. `Op` spans index into this.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn parts(&self) -> &[ContentPart] {
        &self.parts
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Map a logical offset back to `(part index, offset within that part)`.
    ///
    /// Returns `None` for an offset that lands on an inserted separator, which
    /// belongs to no source stream.
    pub fn locate(&self, offset: usize) -> Option<(usize, usize)> {
        // Parts are sorted and disjoint, so a binary search beats a scan on the
        // thousand-part streams some producers emit.
        let idx = self
            .parts
            .binary_search_by(|p| {
                if offset < p.range.start {
                    std::cmp::Ordering::Greater
                } else if offset >= p.range.end {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .ok()?;
        Some((idx, offset - self.parts[idx].range.start))
    }

    /// Map a logical span onto the parts it covers.
    ///
    /// Usually one part. An operator cannot span a boundary, but a *paragraph*
    /// can -- its span is the union of its operators' -- and the edit layer has
    /// to patch each underlying object separately.
    pub fn locate_span(&self, span: Range<usize>) -> Vec<(usize, Range<usize>)> {
        let mut out = Vec::new();
        if span.start >= span.end {
            return out;
        }
        for (i, part) in self.parts.iter().enumerate() {
            if part.range.end <= span.start || part.range.start >= span.end {
                continue;
            }
            let lo = span.start.max(part.range.start) - part.range.start;
            let hi = span.end.min(part.range.end) - part.range.start;
            if lo < hi {
                out.push((i, lo..hi));
            }
        }
        out
    }

    /// The object a logical offset came from.
    pub fn source_of(&self, offset: usize) -> Option<ObjId> {
        self.locate(offset).map(|(i, _)| self.parts[i].id)
    }

    /// True when a span lies wholly within one source stream, which is the
    /// condition for patching it as a single splice.
    pub fn is_contiguous(&self, span: Range<usize>) -> bool {
        self.locate_span(span).len() == 1
    }
}

/// Resolve `/Contents` on a page dictionary and build the logical stream.
pub fn page_content(
    doc: &Document,
    page: &rasura_cos::Dictionary,
) -> Result<(LogicalContent, Vec<CosError>)> {
    let Some(contents) = page.get("Contents") else {
        // A page with no content is legal and renders blank.
        return Ok((LogicalContent::default(), Vec::new()));
    };
    // Deliberately not resolved first: `build` needs to see whether this is a
    // reference to an array or to a stream.
    Ok(LogicalContent::build(doc, contents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenize;
    use rasura_cos::testutil::ClassicBuilder;

    fn three_part_page() -> Vec<u8> {
        // A page whose /Contents is an array of three streams, split so that
        // the split points are mid-construct.
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Contents [4 0 R 5 0 R 6 0 R] >>",
            )
            .stream(4, "", b"BT /F1 12 Tf")
            .stream(5, "", b"72 700 Td (Hello) Tj")
            .stream(6, "", b"ET")
            .finish("/Root 1 0 R")
    }

    fn open(bytes: Vec<u8>) -> Document {
        Document::open(bytes).unwrap()
    }

    #[test]
    fn parts_are_separated_so_tokens_cannot_merge() {
        let doc = open(three_part_page());
        let page = doc.get(ObjId::new(3, 0)).unwrap();
        let (content, errors) = page_content(&doc, page.as_dict().unwrap()).unwrap();
        assert!(errors.is_empty(), "{errors:?}");

        // Without a separator, "Tf" and "72" would lex as one token.
        let text = String::from_utf8_lossy(content.data());
        assert!(text.contains("Tf\n72"), "{text:?}");

        let ops = tokenize(content.data()).0;
        let kinds: Vec<_> = ops.iter().map(|o| o.kind).collect();
        assert_eq!(
            kinds,
            vec![
                crate::OpKind::BeginText,
                crate::OpKind::SetFont,
                crate::OpKind::TextMove,
                crate::OpKind::ShowText,
                crate::OpKind::EndText,
            ]
        );
    }

    #[test]
    fn offsets_map_back_to_the_right_object() {
        let doc = open(three_part_page());
        let page = doc.get(ObjId::new(3, 0)).unwrap();
        let (content, _) = page_content(&doc, page.as_dict().unwrap()).unwrap();

        let ops = tokenize(content.data()).0;
        let by_kind = |k| ops.iter().find(|o| o.kind == k).unwrap();

        // Each operator has to be attributed to the stream it actually lives in,
        // or a patch would be written into the wrong object.
        assert_eq!(
            content.source_of(by_kind(crate::OpKind::SetFont).span.start),
            Some(ObjId::new(4, 0))
        );
        assert_eq!(
            content.source_of(by_kind(crate::OpKind::TextMove).span.start),
            Some(ObjId::new(5, 0))
        );
        assert_eq!(
            content.source_of(by_kind(crate::OpKind::ShowText).span.start),
            Some(ObjId::new(5, 0))
        );
        assert_eq!(
            content.source_of(by_kind(crate::OpKind::EndText).span.start),
            Some(ObjId::new(6, 0))
        );
    }

    #[test]
    fn separator_offsets_belong_to_no_part() {
        let mut c = LogicalContent::default();
        c.push_part(ObjId::new(1, 0), b"AB");
        c.push_part(ObjId::new(2, 0), b"CD");
        assert_eq!(c.data(), b"AB\nCD");
        assert_eq!(c.locate(0), Some((0, 0)));
        assert_eq!(c.locate(1), Some((0, 1)));
        assert_eq!(c.locate(2), None, "the separator is not in any source stream");
        assert_eq!(c.locate(3), Some((1, 0)));
        assert_eq!(c.locate(4), Some((1, 1)));
        assert_eq!(c.locate(5), None, "past the end");
    }

    #[test]
    fn a_span_within_one_part_is_contiguous() {
        let mut c = LogicalContent::default();
        c.push_part(ObjId::new(1, 0), b"HELLO");
        c.push_part(ObjId::new(2, 0), b"WORLD");
        assert!(c.is_contiguous(1..4));
        let located = c.locate_span(1..4);
        assert_eq!(located, vec![(0, 1..4)]);
    }

    #[test]
    fn a_span_crossing_a_boundary_reports_every_part() {
        // The case the edit layer has to handle: one logical range, two objects.
        let mut c = LogicalContent::default();
        c.push_part(ObjId::new(1, 0), b"HELLO");
        c.push_part(ObjId::new(2, 0), b"WORLD");
        c.push_part(ObjId::new(3, 0), b"AGAIN");
        assert!(!c.is_contiguous(3..14));
        let located = c.locate_span(3..14);
        assert_eq!(located, vec![(0, 3..5), (1, 0..5), (2, 0..2)]);
    }

    #[test]
    fn empty_span_locates_nothing() {
        let mut c = LogicalContent::default();
        c.push_part(ObjId::new(1, 0), b"HELLO");
        assert!(c.locate_span(2..2).is_empty());
        // A reversed range, which a caller could compute from bad arithmetic.
        #[allow(clippy::reversed_empty_ranges)]
        let reversed = 4..1;
        assert!(c.locate_span(reversed).is_empty());
    }

    #[test]
    fn a_single_stream_page_has_one_part_and_no_separator() {
        let doc = open(rasura_cos::testutil::classic_with_flate_content());
        let page = doc.get(ObjId::new(3, 0)).unwrap();
        let (content, errors) = page_content(&doc, page.as_dict().unwrap()).unwrap();
        assert!(errors.is_empty());
        assert_eq!(content.parts().len(), 1);
        assert_eq!(content.parts()[0].id, ObjId::new(4, 0));
        assert!(String::from_utf8_lossy(content.data()).contains("Hello, rasura"));
        // The whole buffer belongs to that one stream.
        assert!(content.is_contiguous(0..content.len()));
    }

    #[test]
    fn a_page_with_no_contents_is_empty_not_an_error() {
        let doc = open(rasura_cos::testutil::minimal_classic());
        let page = doc.get(ObjId::new(3, 0)).unwrap();
        let (content, errors) = page_content(&doc, page.as_dict().unwrap()).unwrap();
        assert!(content.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn a_broken_part_is_reported_and_the_rest_survives() {
        // One unreadable stream out of three must not cost the other two.
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /Contents [4 0 R 9 0 R 5 0 R] >>")
            .stream(4, "", b"BT")
            .stream(5, "", b"ET")
            .finish("/Root 1 0 R");
        let doc = open(bytes);
        let page = doc.get(ObjId::new(3, 0)).unwrap();
        let (content, errors) = page_content(&doc, page.as_dict().unwrap()).unwrap();
        assert_eq!(content.parts().len(), 2);
        assert_eq!(errors.len(), 1, "the missing stream must be reported");
        assert_eq!(tokenize(content.data()).0.len(), 2);
    }

    #[test]
    fn locate_is_correct_across_many_parts() {
        // Exercises the binary search rather than the two-part happy path.
        let mut c = LogicalContent::default();
        for i in 1..=200u32 {
            c.push_part(ObjId::new(i, 0), format!("part{i:04}").as_bytes());
        }
        for i in 1..=200u32 {
            let part = &c.parts()[(i - 1) as usize];
            assert_eq!(c.source_of(part.range.start), Some(ObjId::new(i, 0)));
            assert_eq!(c.source_of(part.range.end - 1), Some(ObjId::new(i, 0)));
            assert_eq!(c.locate(part.range.start), Some(((i - 1) as usize, 0)));
        }
    }
}
