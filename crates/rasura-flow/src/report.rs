//! What the conversion guessed, and what it dropped.
//!
//! The project's second correctness property is that fidelity is reported and
//! never assumed, and it applies here more than anywhere else in the codebase:
//! a flow document is a *reconstruction*, and unlike an edit it has no original
//! to be compared against. `docs/flow-model.md` is blunt about the risk —
//!
//! > Reconstruction is heuristic, and regeneration compounds it. [...] unlike a
//! > bad extraction, a bad regeneration is what the user now has.
//!
//! — so every inference this crate makes is counted and named. A caller
//! deciding whether to trust an export reads this; a corpus measurement counts
//! it; and a `Guess` that turns out to fire on almost every document is a
//! heuristic that needs replacing rather than a caveat to live with.

use std::collections::BTreeMap;

/// An inference the conversion had to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Guess {
    /// The block order came from page geometry rather than `/StructTreeRoot`.
    /// Reported once for the document, not once per block.
    ReadingOrderInferred,
    /// A paragraph was promoted to a heading on typographic evidence.
    HeadingInferred,
    /// A paragraph was taken to be a list item because of its marker.
    ListInferred,
    /// A structure-tree list label (`Lbl`) was dropped, the flow model
    /// generating its own markers.
    ListLabelDropped,
    /// Bold or italic was taken from the PostScript font name.
    EmphasisFromFontName,
    /// Inline styling inside table cells was flattened to plain text.
    TableCellStyleDropped,
    /// A word broken across two lines was rejoined and its hyphen removed.
    ///
    /// Right for a soft hyphen the producer inserted and wrong for a compound
    /// word that happened to break at its own hyphen. The layer below flags the
    /// paragraph, not the individual break, so this is a guess per join.
    HyphenationJoined,
}

impl Guess {
    pub fn as_str(self) -> &'static str {
        match self {
            Guess::ReadingOrderInferred => "reading order inferred from geometry",
            Guess::HeadingInferred => "heading inferred from type size",
            Guess::ListInferred => "list item inferred from its marker",
            Guess::ListLabelDropped => "structure-tree list label dropped",
            Guess::EmphasisFromFontName => "emphasis taken from the font name",
            Guess::TableCellStyleDropped => "inline style dropped inside a table cell",
            Guess::HyphenationJoined => "a hyphenated line break was rejoined",
        }
    }
}

/// What a conversion did beyond succeeding.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Each inference, with how many times it was made.
    pub guesses: BTreeMap<Guess, usize>,
    /// Blocks in the placed model's reading order.
    pub blocks_in: usize,
    /// Blocks in the flow document. Lower is normal — running elements are
    /// lifted out and list items are gathered — and *much* lower is a signal.
    pub blocks_out: usize,
    /// Vector blocks that were a rule rather than a picture, and so were not
    /// carried into the flow.
    ///
    /// An underline, a table border, the line under a running head. Marking
    /// each of them in an export would bury the text they decorate, and unlike
    /// a chart nothing is lost by leaving one out — but it is a drop, so it is
    /// counted like every other.
    pub rules_dropped: usize,
    /// Opaque blocks whose text was empty, so nothing was carried.
    pub empty_opaque_dropped: usize,
    /// Paragraphs whose glyphs resolved to no text at all.
    ///
    /// Separate from `empty_opaque_dropped` because the two mean opposite
    /// things about the document: an opaque block was *declined* by the
    /// reconstruction, whereas a paragraph it accepted and this crate then
    /// found empty is a paragraph whose characters went missing between the two
    /// — usually a font whose glyphs map to nothing, sometimes a page whose
    /// text is entirely inside annotations this layer does not read.
    pub empty_paragraphs_dropped: usize,
    /// Running headers and footers lifted out of the flow.
    ///
    /// Counted, because a document that is *entirely* running furniture
    /// produces an empty export, and "the export is empty" and "everything in
    /// it was a header" should not be the same observation.
    pub running_lifted: usize,
    /// Annotation text recovered: form-field values and note contents.
    ///
    /// Not part of the block accounting — an annotation was never in the
    /// reading order, because the cut tree only ever saw the content stream.
    pub notes_recovered: usize,
    /// Annotations whose text was not carried because they are hidden.
    ///
    /// `/F` bit 2 or bit 6. Their text is in the file and not on the page, and
    /// an export that included it would show a reader something the document
    /// does not.
    pub hidden_annotations_skipped: usize,
}

impl Report {
    pub(crate) fn note(&mut self, guess: Guess) {
        *self.guesses.entry(guess).or_default() += 1;
    }

    pub fn made(&self, guess: Guess) -> usize {
        self.guesses.get(&guess).copied().unwrap_or(0)
    }

    /// Whether the conversion had to guess at anything at all.
    ///
    /// False for a well-tagged document with no emphasis, which is the case
    /// this crate should be trusted in and the population `docs/flow-model.md`
    /// says to serve first.
    pub fn is_exact(&self) -> bool {
        self.guesses.is_empty() && self.rules_dropped == 0
    }

    /// One line per thing worth knowing, for a caller who logs rather than
    /// branches.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (guess, count) in &self.guesses {
            out.push(format!("{} ({count})", guess.as_str()));
        }
        if self.rules_dropped > 0 {
            out.push(format!("{} rule(s) or border(s) omitted as decoration", self.rules_dropped));
        }
        if self.empty_opaque_dropped > 0 {
            out.push(format!(
                "{} unclassified block(s) had no recoverable text",
                self.empty_opaque_dropped
            ));
        }
        if self.empty_paragraphs_dropped > 0 {
            out.push(format!(
                "{} paragraph(s) resolved to no text: their glyphs map to nothing",
                self.empty_paragraphs_dropped
            ));
        }
        if self.notes_recovered > 0 {
            out.push(format!(
                "{} annotation(s) carried text the content stream does not",
                self.notes_recovered
            ));
        }
        if self.hidden_annotations_skipped > 0 {
            out.push(format!("{} hidden annotation(s) skipped", self.hidden_annotations_skipped));
        }
        if self.running_lifted > 0 {
            out.push(format!(
                "{} running header/footer instance(s) lifted out of the flow",
                self.running_lifted
            ));
        }
        out
    }

    /// Every block that entered, accounted for exactly once.
    ///
    /// The invariant an export has to hold and cannot demonstrate by looking
    /// right: a block that silently disappears leaves output that still reads
    /// like a document. `gathered` is the number of blocks folded into a list
    /// that the caller must supply, because only the caller can see the list
    /// structure this returns nothing about.
    pub fn accounts_for_everything(&self, gathered: usize) -> bool {
        self.blocks_out
            + gathered
            + self.running_lifted
            + self.rules_dropped
            + self.empty_opaque_dropped
            + self.empty_paragraphs_dropped
            // A structure-tree `Lbl` is a block that becomes no block: the flow
            // model generates list markers rather than storing them. Found by
            // this check failing on the four corpus documents that have any,
            // which is what the check is for — the drop was deliberate and its
            // absence from the sum was not.
            + self.made(Guess::ListLabelDropped)
            == self.blocks_in
    }
}
