//! Editing table cells. Spec 9.2.
//!
//! > `set_cell(table, row, col, text)`, `insert_row`, `delete_row`,
//! > `insert_column`, `delete_column`, `set_column_width`
//!
//! # A PDF table is not a table
//!
//! §7.7 *detects* tables — from a drawn grid, or from columns of text that line
//! up — and the detection is good enough to read one. It is not good enough to
//! restructure one, and the difference is worth being precise about, because
//! the six operations above are not six variations on a theme.
//!
//! `set_cell` edits text that already exists, inside a region §7.7 identified.
//! If the detection was wrong, the wrong text changes and the caller can see
//! that immediately. It is an ordinary text edit with a cell-shaped address.
//!
//! The other five *move* content. Inserting a row means shifting every cell
//! below it down, redrawing the ruling lines that separate them, resizing the
//! grid, and reflowing any cell whose new width no longer fits its text — on a
//! structure that was **inferred** rather than declared. A misdetected column
//! edge turns into a visibly broken table, and unlike a wrong `set_cell` the
//! damage is spread across the whole figure.
//!
//! So `set_cell` is implemented and the structural five decline by name. That
//! is not a gap to be filled by writing more of the same code: it needs a table
//! model the *producer* declared, which in a PDF means `/StructTreeRoot` with
//! `/Table`, `/TR` and `/TD` elements. Where that exists the structure is known
//! rather than guessed, and the operations become tractable. Where it does not,
//! no amount of geometry makes them safe.

use crate::locate::{EditablePage, ParagraphId};
use crate::reflow::Policy;
use crate::text::{Edit, TextError};
use rasura_cos::Document;
use rasura_layout::tables::Table;

/// Why a table edit could not be made.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TableError {
    #[error("the table has no cell at row {row}, column {column}")]
    NoSuchCell { row: usize, column: usize },

    /// The cell's text is drawn by operators this layer cannot address.
    #[error(transparent)]
    Text(#[from] TextError),

    /// A cell whose text this page's paragraphs do not account for.
    #[error("row {row}, column {column} has no editable paragraph")]
    NotEditable { row: usize, column: usize },

    /// One of spec 9.2's structural operations.
    ///
    /// Declined rather than approximated: they move content on a grid that was
    /// *inferred*, and a misdetected edge turns into a visibly broken table.
    /// See the module note — the fix is a producer-declared table structure,
    /// not more geometry.
    #[error(
        "{0} restructures a table whose grid was inferred rather than declared; \
         it needs /StructTreeRoot table elements"
    )]
    NeedsDeclaredStructure(&'static str),
}

/// Replace a cell's text. Spec 9.2.
///
/// An ordinary text edit with a cell-shaped address: the cell's paragraph is
/// found by matching §7.7's detected cell back to the page's paragraphs, and
/// the replacement goes through [`crate::replace_text`] unchanged. Nothing
/// about a cell makes its text special, and giving it a separate path would
/// mean two places where encoding and reflow could diverge.
pub fn set_cell(
    doc: &Document,
    page: &EditablePage,
    table: &Table,
    row: usize,
    column: usize,
    text: &str,
    policy: Policy,
) -> Result<Edit, TableError> {
    let cell = table
        .cells
        .iter()
        .find(|c| c.row == row && c.column == column)
        .ok_or(TableError::NoSuchCell { row, column })?;

    // The paragraph whose glyphs sit inside the cell. Matched by position
    // rather than by index: §7.7's cells carry their own copies of the lines,
    // and the page's paragraphs are numbered independently of them.
    let id = paragraph_in(page, cell).ok_or(TableError::NotEditable { row, column })?;
    let existing = page.text_of(id);

    Ok(crate::replace_text(doc, page, id, 0..existing.chars().count(), text, policy)?)
}

/// The paragraph whose glyphs fall inside a cell.
fn paragraph_in(page: &EditablePage, cell: &rasura_layout::tables::Cell) -> Option<ParagraphId> {
    let inside = |x: f64, y: f64| {
        x >= cell.bbox.x0 - 0.5
            && x <= cell.bbox.x1 + 0.5
            && y >= cell.bbox.y0 - 0.5
            && y <= cell.bbox.y1 + 0.5
    };

    page.paragraphs.iter().find_map(|(id, _)| {
        let lines = page.lines_of(*id)?;
        // Every glyph of the paragraph, so a paragraph straddling a cell
        // boundary is not claimed by either -- editing it would change text
        // outside the cell the caller addressed.
        let mut any = false;
        for line in lines {
            for glyph in &line.glyphs {
                if !inside(glyph.origin.x, glyph.origin.y) {
                    return None;
                }
                any = true;
            }
        }
        any.then_some(*id)
    })
}

/// Spec 9.2's structural table operations, declined by name.
///
/// Each is a real operation with a known shape; none is safe on an inferred
/// grid. They are listed individually rather than behind one function so that
/// the API says what it will eventually do, and a caller discovers the limit at
/// the call site rather than in a changelog.
pub fn insert_row(_table: &Table, _at: usize) -> Result<Edit, TableError> {
    Err(TableError::NeedsDeclaredStructure("insert_row"))
}

pub fn delete_row(_table: &Table, _at: usize) -> Result<Edit, TableError> {
    Err(TableError::NeedsDeclaredStructure("delete_row"))
}

pub fn insert_column(_table: &Table, _at: usize) -> Result<Edit, TableError> {
    Err(TableError::NeedsDeclaredStructure("insert_column"))
}

pub fn delete_column(_table: &Table, _at: usize) -> Result<Edit, TableError> {
    Err(TableError::NeedsDeclaredStructure("delete_column"))
}

pub fn set_column_width(_table: &Table, _column: usize, _width: f64) -> Result<Edit, TableError> {
    Err(TableError::NeedsDeclaredStructure("set_column_width"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditSession;
    use rasura_cos::SaveOptions;
    use rasura_cos::testutil::ClassicBuilder;

    /// A two-by-two grid drawn with rules, with a word in each cell.
    fn table_page() -> Vec<u8> {
        let content = b"0 0 0 RG 1 w\n\
            100 600 m 300 600 l S\n\
            100 650 m 300 650 l S\n\
            100 700 m 300 700 l S\n\
            100 600 m 100 700 l S\n\
            200 600 m 200 700 l S\n\
            300 600 m 300 700 l S\n\
            BT /F1 10 Tf 1 0 0 1 110 670 Tm (alpha) Tj ET\n\
            BT /F1 10 Tf 1 0 0 1 210 670 Tm (beta) Tj ET\n\
            BT /F1 10 Tf 1 0 0 1 110 620 Tm (gamma) Tj ET\n\
            BT /F1 10 Tf 1 0 0 1 210 620 Tm (delta) Tj ET\n";

        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", content)
            .object(
                5,
                "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                 /Encoding /WinAnsiEncoding >>",
            )
            .finish("/Root 1 0 R")
    }

    fn detected(bytes: Vec<u8>) -> (Document, EditablePage, Vec<Table>) {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let page = EditablePage::analyse(&doc, &pages.pages[0]).expect("analyse");

        let rules = rasura_layout::rules::collect(&doc, &pages.pages[0]);
        let tables = rasura_layout::tables::detect_page(&page.regions, &rules, &page.runs);
        (doc, page, tables)
    }

    #[test]
    fn a_ruled_grid_is_detected_as_a_table() {
        let (_doc, _page, tables) = detected(table_page());
        assert_eq!(tables.len(), 1, "one table");
        assert!(
            tables[0].rows >= 2 && tables[0].cols >= 2,
            "{:?}",
            (tables[0].rows, tables[0].cols)
        );
    }

    #[test]
    fn setting_a_cell_changes_that_cell_and_no_other() {
        let (doc, page, tables) = detected(table_page());
        let table = &tables[0];

        // Whichever cell holds "beta".
        let target = table
            .cells
            .iter()
            .find(|c| c.paragraphs.iter().any(|_| true) && cell_text(&page, c).contains("beta"))
            .map(|c| (c.row, c.column))
            .expect("the beta cell");

        let edit = set_cell(
            &doc,
            &page,
            table,
            target.0,
            target.1,
            "BETA",
            Policy { breaking: crate::Breaking::Greedy, overflow: crate::Overflow::Allow },
        )
        .expect("set_cell");

        let mut doc = doc;
        let content = page.content;
        let mut session = EditSession::new(&mut doc);
        session.patch_content("set cell", &content, &edit.patches, edit.fidelity).expect("patch");
        let saved = session.commit(&SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        let pages = rasura_content::page::pages(&after).expect("pages");
        let after_page = EditablePage::analyse(&after, &pages.pages[0]).expect("analyse");
        let all: String =
            after_page.paragraphs.iter().map(|(id, _)| after_page.text_of(*id)).collect();

        assert!(all.contains("BETA"), "{all:?}");
        assert!(!all.contains("beta"), "{all:?}");
        // The neighbours are untouched.
        for word in ["alpha", "gamma", "delta"] {
            assert!(all.contains(word), "{word} survived: {all:?}");
        }
    }

    fn cell_text(page: &EditablePage, cell: &rasura_layout::tables::Cell) -> String {
        let _ = page;
        cell.lines.iter().map(|l| l.text()).collect()
    }

    #[test]
    fn a_cell_that_does_not_exist_is_an_error() {
        let (doc, page, tables) = detected(table_page());
        let err = set_cell(&doc, &page, &tables[0], 99, 99, "x", Policy::default())
            .expect_err("no such cell");
        assert!(matches!(err, TableError::NoSuchCell { .. }), "{err:?}");
    }

    #[test]
    fn the_structural_operations_decline_by_name() {
        // Each names itself, so a caller discovers the limit at the call site
        // rather than in a changelog -- and the message says what would make
        // them possible rather than just refusing.
        let (_doc, _page, tables) = detected(table_page());
        let t = &tables[0];

        for err in [
            insert_row(t, 0).unwrap_err(),
            delete_row(t, 0).unwrap_err(),
            insert_column(t, 0).unwrap_err(),
            delete_column(t, 0).unwrap_err(),
            set_column_width(t, 0, 10.0).unwrap_err(),
        ] {
            let TableError::NeedsDeclaredStructure(which) = err else {
                panic!("expected a named decline, got {err:?}");
            };
            assert!(!which.is_empty());
            assert!(
                format!("{}", TableError::NeedsDeclaredStructure(which)).contains("StructTreeRoot"),
                "the message says what would make it possible"
            );
        }
    }

    #[test]
    fn a_paragraph_straddling_a_cell_boundary_is_not_claimed() {
        // Editing it would change text outside the cell the caller addressed,
        // which on a grid that was *inferred* is how a wrong detection becomes
        // a wrong edit.
        let (_doc, page, tables) = detected(table_page());
        let table = &tables[0];

        for cell in &table.cells {
            if let Some(id) = paragraph_in(&page, cell) {
                let lines = page.lines_of(id).expect("lines");
                for line in lines {
                    for glyph in &line.glyphs {
                        assert!(
                            glyph.origin.x >= cell.bbox.x0 - 0.5
                                && glyph.origin.x <= cell.bbox.x1 + 0.5,
                            "every claimed glyph is inside the cell"
                        );
                    }
                }
            }
        }
    }
}
