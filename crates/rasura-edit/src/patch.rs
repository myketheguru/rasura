//! Splicing bytes into a decoded content stream. Spec 9.4, step 3.
//!
//! > Splice into the decoded stream buffer at the affected spans. **Every byte
//! > outside those spans is copied verbatim.**
//!
//! This is the smallest module in the edit layer and the one the whole product
//! rests on. Spec §2's first property —
//!
//! > An edit on page 40 must not change the rendered output of any other page
//! > by a single pixel, nor alter the bytes of any object it did not need to
//! > touch.
//!
//! — is not a property of the reflow engine or the font engine. Those decide
//! *what* to write. This decides *where*, and it is the only place that can
//! break the guarantee no matter how correct everything above it is.
//!
//! So the verbatim rule is enforced here rather than assumed: [`splice`]
//! reconstructs the untouched regions from the original buffer and nothing
//! else, and returns a [`Spliced`] that can map any old offset to its new
//! position — because the callers holding byte spans into the old buffer (every
//! glyph run on the page) need to keep pointing at the right bytes afterwards.

use std::ops::Range;

/// One replacement: these bytes, in place of that range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// The half-open byte range in the *original* buffer being replaced.
    pub span: Range<usize>,
    /// What goes there. Empty is a deletion.
    pub bytes: Vec<u8>,
}

impl Patch {
    pub fn new(span: Range<usize>, bytes: impl Into<Vec<u8>>) -> Patch {
        Patch { span, bytes: bytes.into() }
    }

    /// An insertion: no bytes removed, new bytes placed at `at`.
    pub fn insert(at: usize, bytes: impl Into<Vec<u8>>) -> Patch {
        Patch { span: at..at, bytes: bytes.into() }
    }

    /// A deletion: the range removed, nothing put back.
    pub fn delete(span: Range<usize>) -> Patch {
        Patch { span, bytes: Vec::new() }
    }

    /// How much the buffer grows (or shrinks) when this is applied.
    fn delta(&self) -> isize {
        self.bytes.len() as isize - (self.span.end - self.span.start) as isize
    }
}

/// Why a set of patches could not be applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    /// A span reaches past the end of the buffer it addresses.
    #[error("patch span {start}..{end} runs past the {len}-byte stream")]
    OutOfBounds { start: usize, end: usize, len: usize },

    /// A span is backwards.
    #[error("patch span {start}..{end} is inverted")]
    Inverted { start: usize, end: usize },

    /// Two patches claim the same bytes.
    ///
    /// Rejected rather than resolved. Two operations that both rewrite the same
    /// operator have no defined combined meaning, and picking one silently
    /// would apply half of what the caller asked for while reporting success.
    #[error("patches {first_end} and {second_start} overlap: the same bytes are claimed twice")]
    Overlap { first_end: usize, second_start: usize },
}

/// Where one patch's bytes ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// The range it replaced, in the original buffer.
    pub old: Range<usize>,
    /// Where its replacement bytes begin in the new buffer.
    pub new_start: usize,
    /// How many bytes it wrote.
    pub new_len: usize,
}

impl Placement {
    /// The replacement's range in the new buffer.
    pub fn new_span(&self) -> Range<usize> {
        self.new_start..self.new_start + self.new_len
    }
}

/// The result of applying patches, and the map back to where things went.
#[derive(Debug, Clone)]
pub struct Spliced {
    /// The new buffer.
    pub bytes: Vec<u8>,
    /// Every applied patch, in ascending order of the range it replaced.
    applied: Vec<Placement>,
    /// The original buffer's length, to bound `remap`.
    original_len: usize,
}

impl Spliced {
    /// Where an offset in the original buffer now lives.
    ///
    /// Offsets *inside* a replaced span have no honest answer — the bytes they
    /// addressed are gone — so those return `None` rather than a nearby guess.
    /// A caller holding a span into a region it just rewrote should be using
    /// the new bytes it supplied, not hunting for the old ones.
    ///
    /// A span's *boundaries* are not inside it. `start` means "just before what
    /// was there" and `end` means "just after"; both still name a real position
    /// once the bytes between them are gone. Only offsets strictly between them
    /// are unanswerable.
    pub fn remap(&self, offset: usize) -> Option<usize> {
        if offset > self.original_len {
            return None;
        }
        let mut delta: isize = 0;
        for p in &self.applied {
            if p.old.end <= offset {
                delta += p.new_len as isize - (p.old.end - p.old.start) as isize;
            } else if p.old.start < offset {
                return None;
            } else {
                break;
            }
        }
        usize::try_from(offset as isize + delta).ok()
    }

    /// Where each patch's replacement bytes landed.
    pub fn placements(&self) -> &[Placement] {
        &self.applied
    }
}

/// Apply `patches` to `original`, copying every unclaimed byte verbatim.
///
/// The patches may arrive in any order; they are sorted here. Overlap is an
/// error, not a merge.
pub fn splice(original: &[u8], patches: &[Patch]) -> Result<Spliced, PatchError> {
    let mut ordered: Vec<&Patch> = patches.iter().collect();
    // By start, then by end, so a zero-width insertion at the same position as
    // a replacement sorts before it and both survive the overlap check.
    ordered.sort_by_key(|p| (p.span.start, p.span.end));

    for p in &ordered {
        if p.span.start > p.span.end {
            return Err(PatchError::Inverted { start: p.span.start, end: p.span.end });
        }
        if p.span.end > original.len() {
            return Err(PatchError::OutOfBounds {
                start: p.span.start,
                end: p.span.end,
                len: original.len(),
            });
        }
    }
    for pair in ordered.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        // Touching is fine: a..b then b..c claim disjoint bytes. Two insertions
        // at the same offset are also fine, and apply in the order given.
        if a.span.end > b.span.start {
            return Err(PatchError::Overlap { first_end: a.span.end, second_start: b.span.start });
        }
    }

    let grown: isize = ordered.iter().map(|p| p.delta()).sum();
    let capacity = usize::try_from(original.len() as isize + grown).unwrap_or(original.len());
    let mut out = Vec::with_capacity(capacity);
    let mut applied = Vec::with_capacity(ordered.len());

    let mut cursor = 0usize;
    for p in &ordered {
        // The verbatim run before this patch. This is the line that makes the
        // guarantee true: untouched bytes are copied from the original, never
        // regenerated from a parsed representation of them.
        out.extend_from_slice(&original[cursor..p.span.start]);
        applied.push(Placement {
            old: p.span.clone(),
            new_start: out.len(),
            new_len: p.bytes.len(),
        });
        out.extend_from_slice(&p.bytes);
        cursor = p.span.end;
    }
    out.extend_from_slice(&original[cursor..]);

    Ok(Spliced { bytes: out, applied, original_len: original.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_bytes_are_copied_verbatim() {
        let original = b"BT /F1 12 Tf (hello) Tj ET";
        let at = original.windows(7).position(|w| w == b"(hello)").expect("found");
        let out =
            splice(original, &[Patch::new(at..at + 7, b"(goodbye)".to_vec())]).expect("splice");

        assert_eq!(out.bytes, b"BT /F1 12 Tf (goodbye) Tj ET");
        // And specifically: the prefix and suffix are the original's own bytes.
        assert_eq!(&out.bytes[..at], &original[..at]);
        assert_eq!(&out.bytes[at + 9..], &original[at + 7..]);
    }

    #[test]
    fn no_patches_returns_the_input_byte_for_byte() {
        // The degenerate case, and the one an editor hits constantly: a session
        // that touched nothing must produce the file it opened. Spec 14.2 I1.
        let original = b"q 1 0 0 1 0 0 cm Q";
        let out = splice(original, &[]).expect("splice");
        assert_eq!(out.bytes, original);
    }

    #[test]
    fn patches_apply_in_position_order_however_they_arrive() {
        let original = b"AAAABBBBCCCC";
        let patches = [
            Patch::new(8..12, b"cc".to_vec()),
            Patch::new(0..4, b"aa".to_vec()),
            Patch::new(4..8, b"bb".to_vec()),
        ];
        let out = splice(original, &patches).expect("splice");
        assert_eq!(out.bytes, b"aabbcc");
    }

    #[test]
    fn insertion_and_deletion_are_ordinary_patches() {
        let original = b"AC";
        let inserted = splice(original, &[Patch::insert(1, b"B".to_vec())]).expect("splice");
        assert_eq!(inserted.bytes, b"ABC");

        let deleted = splice(b"ABC", &[Patch::delete(1..2)]).expect("splice");
        assert_eq!(deleted.bytes, b"AC");
    }

    #[test]
    fn overlapping_patches_are_refused_rather_than_merged() {
        // Two operations rewriting the same operator have no defined combined
        // meaning. Applying one and reporting success would be the worst of the
        // available answers.
        let original = b"AAAABBBB";
        let err =
            splice(original, &[Patch::new(0..5, b"x".to_vec()), Patch::new(4..8, b"y".to_vec())])
                .expect_err("overlap");
        assert!(matches!(err, PatchError::Overlap { .. }), "{err:?}");
    }

    #[test]
    fn abutting_patches_are_allowed() {
        // a..b then b..c claim disjoint bytes. Refusing these would make it
        // impossible to rewrite two adjacent operators in one commit.
        let out = splice(
            b"AAAABBBB",
            &[Patch::new(0..4, b"x".to_vec()), Patch::new(4..8, b"y".to_vec())],
        )
        .expect("abutting is fine");
        assert_eq!(out.bytes, b"xy");
    }

    #[test]
    fn a_span_past_the_end_is_refused() {
        let err = splice(b"ABC", &[Patch::new(2..9, b"x".to_vec())]).expect_err("out of bounds");
        assert!(matches!(err, PatchError::OutOfBounds { len: 3, .. }), "{err:?}");
    }

    #[test]
    fn offsets_after_a_patch_move_by_its_delta() {
        // The reason `remap` exists: every glyph on the page holds a byte span
        // into the old buffer, and after a splice those spans have to still
        // address the same content.
        let original = b"AAAABBBBCCCC";
        let out = splice(original, &[Patch::new(4..8, b"BB".to_vec())]).expect("splice");

        assert_eq!(out.remap(0), Some(0), "before the patch, unmoved");
        assert_eq!(out.remap(4), Some(4), "the patch's own start");
        assert_eq!(out.remap(8), Some(6), "just after it, shifted by -2");
        assert_eq!(out.remap(12), Some(10), "the end of the buffer");
    }

    #[test]
    fn an_offset_inside_a_replaced_span_has_no_answer() {
        // The bytes it addressed are gone. Returning a nearby offset would let
        // a caller keep using a span that now means something else, which is
        // worse than making it handle the None.
        let out = splice(b"AAAABBBBCCCC", &[Patch::new(4..8, b"xx".to_vec())]).expect("splice");
        assert_eq!(out.remap(5), None);
        assert_eq!(out.remap(7), None);
    }

    #[test]
    fn remap_accumulates_across_several_patches() {
        let original = b"AAAABBBBCCCCDDDD";
        let out = splice(
            original,
            &[Patch::new(0..4, b"a".to_vec()), Patch::new(8..12, b"cccccc".to_vec())],
        )
        .expect("splice");

        assert_eq!(out.bytes, b"aBBBBccccccDDDD");
        assert_eq!(out.remap(4), Some(1), "after -3");
        assert_eq!(out.remap(8), Some(5), "still after -3 only");
        assert_eq!(out.remap(12), Some(11), "after -3 then +2");
        assert_eq!(out.remap(16), Some(15));
    }

    #[test]
    fn placements_say_where_each_replacement_landed() {
        let out = splice(b"AAAABBBB", &[Patch::new(4..8, b"xyz".to_vec())]).expect("splice");
        let places = out.placements();
        assert_eq!(places.len(), 1);
        assert_eq!(places[0].old, 4..8);
        assert_eq!(places[0].new_span(), 4..7);
        assert_eq!(&out.bytes[places[0].new_span()], b"xyz");
    }
}
