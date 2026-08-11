//! Is this a document or a photograph of one? Spec 11.2, 3.
//!
//! > `documentKind: 'born-digital' | 'scanned' | 'mixed'`
//!
//! and, from the non-goals:
//!
//! > **Scanned / image-only PDFs.** Not doing OCR. `documentKind === 'scanned'`,
//! > `paragraphs()` empty.
//!
//! This is the first thing a caller needs to know and the last thing the format
//! tells them. A PDF has no flag for it: a scan is an ordinary page that
//! happens to draw one big image and no text, and nothing in the file says the
//! image is a photograph of a page rather than a photograph of a cat.
//!
//! # The rule, and why each half is needed
//!
//! A page is scanned when **an image covers most of it** and **nothing visible
//! is written on top**. Both halves are load-bearing:
//!
//! - Coverage alone would call a full-bleed magazine page scanned. Those are
//!   born-digital and their text is perfectly editable.
//! - Absence of text alone would call a blank page, a chart, or a title page
//!   scanned. A document of diagrams is not a scan.
//!
//! # Invisible text is not text
//!
//! The one subtlety worth the code it costs. Every OCR tool in existence lays
//! its output over the scan in **text rendering mode 3** — invisible — so the
//! page looks like the original and the text is selectable. Counting those
//! glyphs would classify every OCR'd scan as born-digital, which is precisely
//! backwards: they are the scans most likely to be handed to an editor, and the
//! ones where an edit changes an invisible layer and no pixels.
//!
//! So visibility is what counts, `Tr 3` and `Tr 7` are excluded, and a scan
//! with an OCR layer classifies as [`DocumentKind::Scanned`] with its text
//! still readable through [`crate::Page::text`] — the caller is told what they
//! have rather than having it hidden from them.

use rasura_content::matrix::Rect;
use rasura_content::page::Page;
use rasura_cos::Document;

/// How much of the visible page an image must cover for the page to be a
/// candidate scan.
///
/// Scanner output is one image bled slightly past the page edge, so the real
/// figure is at or above 1.0 and the question is only how much slack to allow
/// for a border. Eight tenths is loose enough for a scan with a white margin
/// and tight enough that a half-page photograph does not qualify.
const COVERAGE: f64 = 0.8;

/// What kind of document this is. Spec 11.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    /// Text was written by a producer and can be edited.
    BornDigital,
    /// Every page is a picture of a page. Nothing here is editable as text,
    /// whether or not an OCR layer makes it selectable.
    Scanned,
    /// Some of each — a born-digital report with scanned exhibits appended, or
    /// a scan with a generated cover sheet. Common enough to need its own
    /// answer, because "scanned" would understate what is editable and
    /// "born-digital" would overstate it.
    Mixed,
}

impl DocumentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DocumentKind::BornDigital => "born-digital",
            DocumentKind::Scanned => "scanned",
            DocumentKind::Mixed => "mixed",
        }
    }
}

/// What one page turned out to be, and the evidence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageKind {
    pub scanned: bool,
    /// Largest single image's share of the visible page, 0 to 1.
    pub image_coverage: f64,
    /// Glyphs a reader can actually see. Invisible OCR text is excluded.
    pub visible_glyphs: usize,
    /// Glyphs drawn in an invisible rendering mode: the OCR signature.
    pub invisible_glyphs: usize,
}

/// Classify one page.
pub fn classify_page(doc: &Document, page: &Page) -> PageKind {
    let visible_box = page.visible_box();
    let graphics = rasura_layout::graphics::collect(doc, page);

    // The largest single image rather than the union of all of them. A scan is
    // one image; a mosaic of tiles that happens to cover the page is a figure,
    // and unioning would call it a scan. Being wrong in the direction of
    // "born-digital" is the safer error: it offers editing on a page that has
    // none rather than withholding it from a page that does.
    let image_coverage = graphics
        .images
        .iter()
        .map(|image| overlap_fraction(&image.bbox, &visible_box))
        .fold(0.0f64, f64::max);

    let (runs, _, _) =
        rasura_content::text::extract_page_with(doc, page, &rasura_layout::Standard14Widths);

    let mut visible_glyphs = 0usize;
    let mut invisible_glyphs = 0usize;
    for run in &runs {
        // ISO 32000-1 Table 106: modes 3 and 7 add nothing to the page. Mode 7
        // is clip-only, which is rarer and just as invisible.
        if run.render_mode == 3 || run.render_mode == 7 {
            invisible_glyphs += run.glyphs.len();
        } else {
            visible_glyphs += run.glyphs.len();
        }
    }

    PageKind {
        scanned: image_coverage >= COVERAGE && visible_glyphs == 0,
        image_coverage,
        visible_glyphs,
        invisible_glyphs,
    }
}

/// Classify a document from its pages.
///
/// A document with no pages is born-digital rather than scanned: there is no
/// evidence of scanning, and the alternative reports a property of a file that
/// has no content to have it.
pub fn classify(pages: &[PageKind]) -> DocumentKind {
    let scanned = pages.iter().filter(|p| p.scanned).count();
    match scanned {
        0 => DocumentKind::BornDigital,
        n if n == pages.len() => DocumentKind::Scanned,
        _ => DocumentKind::Mixed,
    }
}

/// How much of `page` the rectangle `image` covers, 0 to 1.
fn overlap_fraction(image: &Rect, page: &Rect) -> f64 {
    let area = (page.x1 - page.x0) * (page.y1 - page.y0);
    if !(area.is_finite() && area > 0.0) {
        return 0.0;
    }
    let w = (image.x1.min(page.x1) - image.x0.max(page.x0)).max(0.0);
    let h = (image.y1.min(page.y1) - image.y0.max(page.y0)).max(0.0);
    let covered = w * h;
    if !covered.is_finite() { 0.0 } else { (covered / area).min(1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    /// A page drawing `content`, with an image XObject available as `/Im1`.
    fn page_bytes(content: &[u8]) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .stream(
                6,
                " /Type /XObject /Subtype /Image /Width 2 /Height 2 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &[0u8, 64, 128, 255],
            )
            .finish("/Root 1 0 R")
    }

    fn kind_of(content: &[u8]) -> PageKind {
        let doc = Document::open(page_bytes(content)).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        classify_page(&doc, &pages.pages[0])
    }

    /// An image scaled to the whole 600x800 page.
    const FULL_PAGE_IMAGE: &[u8] = b"q 600 0 0 800 0 0 cm /Im1 Do Q\n";

    #[test]
    fn a_full_page_image_with_no_text_is_scanned() {
        let page = kind_of(FULL_PAGE_IMAGE);
        assert!(page.scanned, "{page:?}");
        assert!(page.image_coverage > 0.99, "{page:?}");
        assert_eq!(page.visible_glyphs, 0);
    }

    #[test]
    fn an_ocr_layer_does_not_make_a_scan_born_digital() {
        // The subtlety the whole module exists for. Every OCR tool lays its
        // output over the scan in rendering mode 3, so counting those glyphs
        // classifies every OCR'd scan as born-digital -- precisely backwards,
        // since those are the scans most likely to reach an editor.
        let content = b"q 600 0 0 800 0 0 cm /Im1 Do Q\n\
                        BT /F1 12 Tf 3 Tr 1 0 0 1 72 700 Tm (recognised text) Tj ET\n";
        let page = kind_of(content);
        assert!(page.scanned, "{page:?}");
        assert_eq!(page.visible_glyphs, 0);
        assert!(page.invisible_glyphs > 0, "the OCR layer was seen: {page:?}");
    }

    #[test]
    fn a_full_bleed_image_with_visible_text_is_born_digital() {
        // A magazine page. Coverage alone would call it scanned, and its text
        // is perfectly editable.
        let content = b"q 600 0 0 800 0 0 cm /Im1 Do Q\n\
                        BT /F1 24 Tf 1 0 0 1 72 700 Tm (Headline) Tj ET\n";
        let page = kind_of(content);
        assert!(!page.scanned, "{page:?}");
        assert!(page.visible_glyphs > 0);
    }

    #[test]
    fn a_page_with_no_text_and_no_image_is_not_scanned() {
        // A blank page, or one of rules and shapes. Absence of text alone is
        // not evidence of scanning; a document of diagrams is not a scan.
        let page = kind_of(b"0 0 0 RG 1 w 100 100 m 500 700 l S\n");
        assert!(!page.scanned, "{page:?}");
        assert_eq!(page.image_coverage, 0.0);
    }

    #[test]
    fn a_small_image_does_not_cover_the_page() {
        let content = b"q 100 0 0 100 50 50 cm /Im1 Do Q\n";
        let page = kind_of(content);
        assert!(!page.scanned, "{page:?}");
        assert!(page.image_coverage < 0.05, "{page:?}");
    }

    #[test]
    fn the_document_kinds_follow_from_the_pages() {
        let scanned = PageKind {
            scanned: true,
            image_coverage: 1.0,
            visible_glyphs: 0,
            invisible_glyphs: 10,
        };
        let digital = PageKind {
            scanned: false,
            image_coverage: 0.0,
            visible_glyphs: 100,
            invisible_glyphs: 0,
        };

        assert_eq!(classify(&[scanned, scanned]), DocumentKind::Scanned);
        assert_eq!(classify(&[digital, digital]), DocumentKind::BornDigital);
        assert_eq!(classify(&[digital, scanned]), DocumentKind::Mixed);
        // No pages is not evidence of scanning.
        assert_eq!(classify(&[]), DocumentKind::BornDigital);
    }

    #[test]
    fn overlap_is_clamped_to_the_page() {
        // Scanner output bleeds past the edge, so the raw ratio exceeds one.
        let page = Rect { x0: 0.0, y0: 0.0, x1: 100.0, y1: 100.0 };
        let bled = Rect { x0: -10.0, y0: -10.0, x1: 110.0, y1: 110.0 };
        assert_eq!(overlap_fraction(&bled, &page), 1.0);

        let outside = Rect { x0: 200.0, y0: 200.0, x1: 300.0, y1: 300.0 };
        assert_eq!(overlap_fraction(&outside, &page), 0.0);

        let degenerate = Rect { x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.0 };
        assert_eq!(overlap_fraction(&bled, &degenerate), 0.0);
    }
}
