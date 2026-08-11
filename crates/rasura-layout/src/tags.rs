//! Whether a tagged document is still coherent. Spec 10.1, invariant I6.
//!
//! > If `/StructTreeRoot` is present, the structure tree is authoritative for
//! > reading order and paragraph boundaries, and it **must be maintained across
//! > edits.** Nobody else does this, and it is a genuine differentiator for
//! > anyone under WCAG, Section 508, EN 301 549, or the European Accessibility
//! > Act.
//!
//! > Expose `document.taggedStatus: 'untagged' | 'tagged' | 'tagged-degraded'`
//! > and a `validateTags()` returning the same class of findings veraPDF would.
//!
//! # What "degraded" means, and why it is a third state
//!
//! A document is not simply tagged or not. The interesting case is a file that
//! *has* a structure tree which no longer describes its content: marked-content
//! identifiers that point at nothing, elements with no `/S`, a `/ParentTree`
//! that disagrees with the tree it indexes. Assistive technology reads such a
//! file worse than an untagged one, because it trusts a map that is wrong
//! rather than falling back to geometry.
//!
//! So `Degraded` is reported separately from `Tagged`, and the findings say
//! which of those it is — an accessibility auditor's first question is not
//! "is it tagged" but "does the tagging survive contact with the content".

use rasura_content::page::PageTree;
use rasura_cos::Document;
use std::collections::{BTreeMap, BTreeSet};

/// Spec 10.1's `taggedStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaggedStatus {
    /// No `/StructTreeRoot`.
    Untagged,
    /// A structure tree that matches the content.
    Tagged,
    /// A structure tree that does not. Worse than untagged for a reader,
    /// because it is trusted.
    Degraded,
}

impl TaggedStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaggedStatus::Untagged => "untagged",
            TaggedStatus::Tagged => "tagged",
            TaggedStatus::Degraded => "tagged-degraded",
        }
    }
}

/// One thing wrong with a document's tagging.
///
/// Each names something an accessibility audit would raise, not an internal
/// detail — the list is meant to be shown to a person deciding whether a file
/// is fit to publish.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Finding {
    /// A structure element claims a marked-content id the page does not draw.
    ///
    /// The commonest damage an editor does: content removed while the element
    /// that described it stayed. A screen reader announces a heading with
    /// nothing under it.
    DanglingMcid { page: usize, mcid: u32 },
    /// Marked content on the page that no structure element claims.
    ///
    /// The opposite failure, and the one that silently loses text: the content
    /// is drawn and read aloud by nobody, because the tree that decides reading
    /// order never mentions it.
    UnclaimedMcid { page: usize, mcid: u32 },
    /// An element with no structure type.
    UntypedElement { index: usize },
    /// Two elements claiming the same marked content.
    DuplicateMcid { page: usize, mcid: u32 },
    /// The tree was too deep or too large to read completely.
    Truncated,
}

/// What `validate_tags` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagReport {
    pub status: TaggedStatus,
    pub findings: Vec<Finding>,
    /// Structure elements read.
    pub elements: usize,
    /// Marked-content ids the tree claims.
    pub claimed: usize,
    /// Marked-content ids the pages actually draw.
    pub drawn: usize,
}

impl TagReport {
    pub fn is_ok(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Spec 10.1's `validateTags()`.
///
/// The comparison is between what the tree *claims* and what the pages actually
/// *draw*, which is the only comparison that can catch an edit going wrong —
/// checking the tree against itself would pass a tree that had been consistently
/// detached from its content.
pub fn validate(doc: &Document, pages: &PageTree) -> TagReport {
    let Some(tree) = crate::structure::read(doc) else {
        return TagReport {
            status: TaggedStatus::Untagged,
            findings: Vec::new(),
            elements: 0,
            claimed: 0,
            drawn: 0,
        };
    };

    let mut findings = Vec::new();
    if tree.truncated {
        findings.push(Finding::Truncated);
    }

    // What the tree claims, and who claims it.
    let mut claimed: BTreeMap<(usize, u32), usize> = BTreeMap::new();
    for (index, element) in tree.elements.iter().enumerate() {
        if element.kind.is_empty() {
            findings.push(Finding::UntypedElement { index });
        }
        for (page, mcid) in &element.mcids {
            if claimed.insert((*page, *mcid), index).is_some() {
                findings.push(Finding::DuplicateMcid { page: *page, mcid: *mcid });
            }
        }
    }

    // What the pages draw. Read from the content streams rather than from the
    // model, because the model is built from the same walk and would agree with
    // itself.
    let drawn = drawn_mcids(doc, pages);

    for (page, mcid) in claimed.keys() {
        if !drawn.contains(&(*page, *mcid)) {
            findings.push(Finding::DanglingMcid { page: *page, mcid: *mcid });
        }
    }
    for (page, mcid) in &drawn {
        if !claimed.contains_key(&(*page, *mcid)) {
            findings.push(Finding::UnclaimedMcid { page: *page, mcid: *mcid });
        }
    }

    findings.sort();
    findings.dedup();
    let status = if findings.is_empty() { TaggedStatus::Tagged } else { TaggedStatus::Degraded };

    TagReport {
        status,
        findings,
        elements: tree.elements.len(),
        claimed: claimed.len(),
        drawn: drawn.len(),
    }
}

/// Every `(page, mcid)` the document's content streams actually mark.
pub fn drawn_mcids(doc: &Document, pages: &PageTree) -> BTreeSet<(usize, u32)> {
    use rasura_content::op::OpKind;
    use rasura_cos::object::Object;

    let mut out = BTreeSet::new();
    for page in &pages.pages {
        let Ok((content, _)) = rasura_content::content::page_content(doc, &page.dict) else {
            continue;
        };
        let (ops, _) = rasura_content::tokenize(content.data());
        for op in &ops {
            if op.kind != OpKind::BeginMarkedProps {
                continue;
            }
            // `/Tag << /MCID n >> BDC`, or a named property set the caller
            // resolves through /Properties. Only the inline form carries the
            // id where it can be read without resources.
            let Some(properties) = op.operands.get(1) else { continue };
            let Some(dict) = properties.as_dict() else { continue };
            if let Some(mcid) = dict.get("MCID").and_then(Object::as_i64)
                && let Ok(mcid) = u32::try_from(mcid)
            {
                out.insert((page.index, mcid));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    /// A tagged page: one `/P` element owning MCID 0.
    fn tagged(content: &[u8], struct_extra: &str) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 8 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> /StructParents 0 >>",
            )
            .stream(4, "", content)
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .object(8, "<< /Type /StructTreeRoot /K [9 0 R] >>")
            .object(9, &format!("<< /Type /StructElem /S /P /P 8 0 R /Pg 3 0 R {struct_extra} >>"))
            .finish("/Root 1 0 R")
    }

    fn report_of(bytes: Vec<u8>) -> TagReport {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        validate(&doc, &pages)
    }

    #[test]
    fn an_untagged_document_is_untagged_not_degraded() {
        let report = report_of(rasura_cos::testutil::classic_with_flate_content());
        assert_eq!(report.status, TaggedStatus::Untagged);
        assert!(report.is_ok(), "no tree is not a defect");
        assert_eq!(report.elements, 0);
    }

    #[test]
    fn a_coherent_tree_is_tagged() {
        let content = b"/P << /MCID 0 >> BDC BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello) Tj ET EMC\n";
        let report = report_of(tagged(content, "/K 0"));

        assert_eq!(report.status, TaggedStatus::Tagged, "{:?}", report.findings);
        assert_eq!(report.elements, 1);
        assert_eq!(report.claimed, 1);
        assert_eq!(report.drawn, 1);
    }

    #[test]
    fn an_element_claiming_content_that_is_not_drawn_is_dangling() {
        // The commonest damage an editor does: content removed while the
        // element describing it stayed. A screen reader announces a heading
        // with nothing under it.
        let content = b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello) Tj ET\n";
        let report = report_of(tagged(content, "/K 0"));

        assert_eq!(report.status, TaggedStatus::Degraded);
        assert!(
            report.findings.contains(&Finding::DanglingMcid { page: 0, mcid: 0 }),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn content_no_element_claims_is_unclaimed() {
        // The opposite failure, and the one that silently loses text: it is
        // drawn, and the tree that decides reading order never mentions it.
        let content = b"/P << /MCID 7 >> BDC BT /F1 12 Tf 1 0 0 1 72 700 Tm (Hello) Tj ET EMC\n";
        let report = report_of(tagged(content, "/K 0"));

        assert_eq!(report.status, TaggedStatus::Degraded);
        assert!(
            report.findings.contains(&Finding::UnclaimedMcid { page: 0, mcid: 7 }),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn degraded_is_a_third_state_not_a_kind_of_tagged() {
        // Assistive technology reads a wrongly-tagged file worse than an
        // untagged one, because it trusts a map that is wrong rather than
        // falling back to geometry. Collapsing the two would hide that.
        assert_ne!(TaggedStatus::Degraded, TaggedStatus::Tagged);
        assert_eq!(TaggedStatus::Degraded.as_str(), "tagged-degraded");
        assert_eq!(TaggedStatus::Untagged.as_str(), "untagged");
    }

    #[test]
    fn the_drawn_set_comes_from_the_content_not_the_model() {
        // Checking the tree against a model built by the same walk would pass a
        // tree that had been consistently detached from its content.
        let content = b"/P << /MCID 0 >> BDC BT /F1 12 Tf 1 0 0 1 72 700 Tm (a) Tj ET EMC\n\
                        /P << /MCID 1 >> BDC BT /F1 12 Tf 1 0 0 1 72 680 Tm (b) Tj ET EMC\n";
        let doc = Document::open(tagged(content, "/K 0")).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let drawn = drawn_mcids(&doc, &pages);

        assert_eq!(drawn.len(), 2);
        assert!(drawn.contains(&(0, 0)) && drawn.contains(&(0, 1)));
    }
}
