//! Getting patched bytes back into the document. Spec 9.4, steps 3 to 5.
//!
//! > 3. Splice into the decoded stream buffer at the affected spans.
//! > 4. Re-encode the stream with its original filter chain.
//! > 5. Mark the containing object dirty.
//!
//! The complication is step 3's "the" decoded stream: a page's `/Contents` may
//! be an array, and the content layer concatenates those objects into one
//! logical buffer so that operators can be found across the join. Every span
//! this layer works in — `GlyphRun::op_span`, and everything derived from it —
//! addresses that logical buffer. The objects on disk do not exist in it.
//!
//! So a patch has to be translated before it can be applied:
//!
//! ```text
//! logical span --LogicalContent::locate_span--> (part, local range)+
//!              --group by object--> patches per object
//!              --splice--> new decoded bytes
//!              --Stream::set_decoded--> re-encoded at save with the same filters
//! ```
//!
//! An operator never crosses a part boundary — the content layer joins parts
//! with a separator that belongs to no operator — so a patch replacing whole
//! operators always lands in exactly one object. A patch that does not is a bug
//! in the caller, and is refused rather than split, because splitting it would
//! write half an operator into each of two streams.

use crate::patch::{Patch, PatchError, splice};
use rasura_content::content::LogicalContent;
use rasura_cos::object::{Object, Stream};
use rasura_cos::{Document, ObjId};
use std::collections::BTreeMap;

/// Why a content-stream patch could not be applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// The patch spans two `/Contents` objects, or lands on the separator
    /// between them.
    ///
    /// Refused rather than split. The bytes between two parts belong to
    /// neither, and an operator that appears to cross the join is one the
    /// caller assembled from spans it should not have merged.
    #[error("the span {start}..{end} is not contained in a single content stream")]
    NotContiguous { start: usize, end: usize },

    /// The object a span resolves to is not a stream any more.
    #[error("object {0:?} is not a content stream")]
    NotAStream(ObjId),

    /// The span resolves to no object at all.
    #[error("the span {start}..{end} belongs to no content stream")]
    Unattributed { start: usize, end: usize },

    #[error(transparent)]
    Patch(#[from] PatchError),

    #[error("{0}")]
    Cos(String),
}

/// What one commit did to the document's streams.
#[derive(Debug, Clone, Default)]
pub struct StreamEdit {
    /// Objects whose decoded content was replaced, with the byte length before
    /// and after.
    pub touched: Vec<(ObjId, usize, usize)>,
}

impl StreamEdit {
    pub fn is_empty(&self) -> bool {
        self.touched.is_empty()
    }
}

/// Translate patches from logical coordinates into per-object patches.
///
/// Returns them grouped by the object they land in, each span local to that
/// object's own decoded bytes.
pub fn localise(
    content: &LogicalContent,
    patches: &[Patch],
) -> Result<BTreeMap<ObjId, Vec<Patch>>, StreamError> {
    let mut out: BTreeMap<ObjId, Vec<Patch>> = BTreeMap::new();

    let logical_len = content.data().len();
    for p in patches {
        // Bounds first, so a span past the end says so. `is_contiguous` is
        // false for such a span too, and reporting "not contiguous" for what is
        // really an out-of-range offset sends the reader looking for a
        // /Contents array that may not even exist.
        if p.span.start > p.span.end {
            return Err(PatchError::Inverted { start: p.span.start, end: p.span.end }.into());
        }
        if p.span.end > logical_len {
            return Err(PatchError::OutOfBounds {
                start: p.span.start,
                end: p.span.end,
                len: logical_len,
            }
            .into());
        }

        // A zero-width insertion has no span to locate, so it is placed by its
        // offset instead -- and an insertion exactly at a part's end belongs to
        // that part, not the next one.
        let pieces = if p.span.is_empty() {
            match content.locate(p.span.start).or_else(|| {
                p.span
                    .start
                    .checked_sub(1)
                    .and_then(|prev| content.locate(prev))
                    .map(|(part, local)| (part, local + 1))
            }) {
                Some((part, local)) => vec![(part, local..local)],
                None => {
                    return Err(StreamError::Unattributed { start: p.span.start, end: p.span.end });
                }
            }
        } else {
            if !content.is_contiguous(p.span.clone()) {
                return Err(StreamError::NotContiguous { start: p.span.start, end: p.span.end });
            }
            content.locate_span(p.span.clone())
        };

        let [(part, local)] = pieces.as_slice() else {
            return Err(StreamError::NotContiguous { start: p.span.start, end: p.span.end });
        };
        let id = content
            .parts()
            .get(*part)
            .map(|c| c.id)
            .ok_or(StreamError::Unattributed { start: p.span.start, end: p.span.end })?;

        out.entry(id).or_default().push(Patch::new(local.clone(), p.bytes.clone()));
    }

    Ok(out)
}

/// Apply logical-coordinate patches to a document's content streams.
///
/// Nothing is written until every patch has been localised and every splice has
/// succeeded, so a failure part-way leaves the document exactly as it was. Spec
/// 9.1: "`commit()` is atomic: either all patches apply or none do."
pub fn apply(
    doc: &mut Document,
    content: &LogicalContent,
    patches: &[Patch],
) -> Result<StreamEdit, StreamError> {
    let by_object = localise(content, patches)?;

    // Phase one: build every replacement. Any error here has touched nothing.
    let mut staged: Vec<(ObjId, Stream, usize, usize)> = Vec::new();
    for (id, object_patches) in by_object {
        let object = doc.get(id).map_err(|e| StreamError::Cos(e.to_string()))?;
        let Some(stream) = object.as_stream() else {
            return Err(StreamError::NotAStream(id));
        };
        let decoded = doc.decoded_stream(id).map_err(|e| StreamError::Cos(e.to_string()))?;

        let spliced = splice(&decoded, &object_patches)?;
        let (before, after) = (decoded.len(), spliced.bytes.len());

        let mut updated = stream.clone();
        // `set_decoded` keeps the filter chain and re-applies it at save, which
        // is spec 9.4 step 4. Writing `set_raw` here would drop the filters and
        // silently inflate the file.
        updated.set_decoded(spliced.bytes);
        staged.push((id, updated, before, after));
    }

    // Phase two: commit them. `set` cannot fail.
    let mut edit = StreamEdit::default();
    for (id, stream, before, after) in staged {
        doc.set(id, Object::Stream(stream));
        edit.touched.push((id, before, after));
    }
    Ok(edit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    /// A one-page document whose `/Contents` is a single stream.
    fn single_stream() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>")
            .stream(4, "", b"BT /F1 12 Tf (hello) Tj ET\n")
            .finish("/Root 1 0 R")
    }

    /// A one-page document whose `/Contents` is an array of two streams.
    fn split_stream() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Contents [4 0 R 5 0 R] >>",
            )
            .stream(4, "", b"BT /F1 12 Tf (hello) Tj")
            .stream(5, "", b"(world) Tj ET")
            .finish("/Root 1 0 R")
    }

    fn content_of(doc: &Document) -> LogicalContent {
        let pages = rasura_content::page::pages(doc).expect("pages");
        let (content, errors) =
            rasura_content::content::page_content(doc, &pages.pages[0].dict).expect("content");
        assert!(errors.is_empty(), "{errors:?}");
        content
    }

    #[test]
    fn a_patch_lands_in_the_object_that_owns_those_bytes() {
        let mut doc = Document::open(single_stream()).expect("open");
        let content = content_of(&doc);
        let at = content.data().windows(7).position(|w| w == b"(hello)").expect("found");

        let edit = apply(&mut doc, &content, &[Patch::new(at..at + 7, b"(howdy)".to_vec())])
            .expect("apply");
        assert_eq!(edit.touched.len(), 1);
        assert_eq!(edit.touched[0].0, ObjId::new(4, 0));

        let after = doc.decoded_stream(ObjId::new(4, 0)).expect("decoded");
        assert_eq!(&*after, b"BT /F1 12 Tf (howdy) Tj ET\n");
    }

    #[test]
    fn a_patch_in_the_second_of_two_content_streams_finds_it() {
        // The case that makes logical coordinates necessary. In the logical
        // buffer `(world)` sits past the join; in object 5 it starts at zero.
        let mut doc = Document::open(split_stream()).expect("open");
        let content = content_of(&doc);
        let at = content.data().windows(7).position(|w| w == b"(world)").expect("found");
        assert!(at > 20, "the span is well past the start of the logical buffer");

        let edit = apply(&mut doc, &content, &[Patch::new(at..at + 7, b"(earth)".to_vec())])
            .expect("apply");
        assert_eq!(edit.touched.len(), 1);
        assert_eq!(edit.touched[0].0, ObjId::new(5, 0), "the second stream, not the first");

        assert_eq!(&*doc.decoded_stream(ObjId::new(5, 0)).unwrap(), b"(earth) Tj ET");
        assert_eq!(
            &*doc.decoded_stream(ObjId::new(4, 0)).unwrap(),
            b"BT /F1 12 Tf (hello) Tj",
            "the untouched stream is untouched"
        );
    }

    #[test]
    fn patches_to_both_streams_are_localised_separately() {
        let mut doc = Document::open(split_stream()).expect("open");
        let content = content_of(&doc);
        let data = content.data().to_vec();
        let first = data.windows(7).position(|w| w == b"(hello)").expect("found");
        let second = data.windows(7).position(|w| w == b"(world)").expect("found");

        let edit = apply(
            &mut doc,
            &content,
            &[
                Patch::new(first..first + 7, b"(HELLO)".to_vec()),
                Patch::new(second..second + 7, b"(WORLD)".to_vec()),
            ],
        )
        .expect("apply");

        assert_eq!(edit.touched.len(), 2);
        assert_eq!(&*doc.decoded_stream(ObjId::new(4, 0)).unwrap(), b"BT /F1 12 Tf (HELLO) Tj");
        assert_eq!(&*doc.decoded_stream(ObjId::new(5, 0)).unwrap(), b"(WORLD) Tj ET");
    }

    #[test]
    fn a_span_crossing_the_join_is_refused() {
        // Splitting it would write half an operator into each of two streams.
        // There is no correct way to do that, so it is not attempted.
        let mut doc = Document::open(split_stream()).expect("open");
        let content = content_of(&doc);
        let first = content.data().windows(7).position(|w| w == b"(hello)").expect("found");
        let second = content.data().windows(7).position(|w| w == b"(world)").expect("found");

        let err = apply(&mut doc, &content, &[Patch::new(first..second + 7, b"x".to_vec())])
            .expect_err("crossing the join");
        assert!(matches!(err, StreamError::NotContiguous { .. }), "{err:?}");
        assert!(!doc.is_dirty(), "a refused patch changed nothing");
    }

    #[test]
    fn a_failure_leaves_the_document_untouched() {
        // Spec 9.1: commit is atomic. The first patch here is applicable and
        // the second is not, and neither may land.
        let mut doc = Document::open(split_stream()).expect("open");
        let content = content_of(&doc);
        let first = content.data().windows(7).position(|w| w == b"(hello)").expect("found");
        let len = content.data().len();

        let err = apply(
            &mut doc,
            &content,
            &[
                Patch::new(first..first + 7, b"(HELLO)".to_vec()),
                Patch::new(len + 10..len + 20, b"x".to_vec()),
            ],
        )
        .expect_err("out of bounds");
        assert!(
            matches!(err, StreamError::Patch(PatchError::OutOfBounds { .. })),
            "the error names the real problem: {err:?}"
        );

        assert!(!doc.is_dirty(), "nothing was staged");
        assert_eq!(&*doc.decoded_stream(ObjId::new(4, 0)).unwrap(), b"BT /F1 12 Tf (hello) Tj");
    }

    #[test]
    fn an_insertion_at_a_part_boundary_belongs_to_the_part_before_it() {
        // Otherwise appending to a page whose content ends at a stream boundary
        // is unattributable, which is the common case for a producer that put
        // one stream per paragraph.
        let mut doc = Document::open(split_stream()).expect("open");
        let content = content_of(&doc);
        let end_of_first = content.parts()[0].range.end;

        apply(&mut doc, &content, &[Patch::insert(end_of_first, b" (!)Tj".to_vec())])
            .expect("apply");
        assert_eq!(
            &*doc.decoded_stream(ObjId::new(4, 0)).unwrap(),
            b"BT /F1 12 Tf (hello) Tj (!)Tj"
        );
    }

    #[test]
    fn the_filter_chain_survives_the_edit() {
        // Spec 9.4 step 4. A flate-compressed stream must come back
        // flate-compressed; `set_raw` would have silently stored it plain, and
        // nothing downstream would notice until someone measured the file.
        let mut doc =
            Document::open(rasura_cos::testutil::classic_with_flate_content()).expect("open");
        let content = content_of(&doc);
        // Length taken from the needle rather than written out: the two drifted
        // apart the moment the fixture string changed length, and a window of
        // the wrong size fails with "found" rather than with the reason.
        const NEEDLE: &[u8] = b"(Hello, rasura)";
        let at = content.data().windows(NEEDLE.len()).position(|w| w == NEEDLE).expect("found");
        apply(
            &mut doc,
            &content,
            &[Patch::new(at..at + NEEDLE.len(), b"(Howdy, rasura)".to_vec())],
        )
        .expect("apply");

        let saved =
            rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).expect("save").bytes;
        let reopened = Document::open(saved).expect("reopen");

        let stream = reopened.get(ObjId::new(4, 0)).expect("object");
        let dict = &stream.as_stream().expect("stream").dict;
        assert_eq!(
            dict.get("Filter").and_then(Object::as_name).and_then(|n| n.as_str()),
            Some("FlateDecode"),
            "the filter is still declared"
        );
        // And it really is compressed, not merely labelled so.
        let raw = stream.as_stream().expect("stream").raw();
        assert_ne!(&raw[..2.min(raw.len())], b"BT", "the bytes on disk are encoded");
        assert_eq!(
            &*reopened.decoded_stream(ObjId::new(4, 0)).unwrap(),
            b"BT /F1 24 Tf 72 700 Td (Howdy, rasura) Tj ET\n"
        );
    }

    #[test]
    fn no_patches_leaves_the_document_clean() {
        let mut doc = Document::open(single_stream()).expect("open");
        let content = content_of(&doc);
        let edit = apply(&mut doc, &content, &[]).expect("apply");
        assert!(edit.is_empty());
        assert!(!doc.is_dirty(), "an empty edit is not an edit");
    }
}
