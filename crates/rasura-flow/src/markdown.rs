//! The flow model as Markdown.
//!
//! `docs/flow-model.md` puts export first in the order of work for a reason
//! that is about testing rather than about features:
//!
//! > It is also the fastest way to find out how good the reconstruction really
//! > is, because an export is read by a human who will notice a scrambled
//! > paragraph immediately.
//!
//! So this renderer optimises for *legibility of the mistakes*. Structure that
//! was recovered shows up as structure; structure that was not shows up as
//! plain paragraphs, which look wrong to a reader in exactly the way they
//! should. Nothing is invented to make the output look tidier than the model
//! underneath it.
//!
//! GitHub-flavoured, because pipe tables are the only widely-read table syntax
//! and a table is the thing a reconstruction most often gets wrong.

use crate::flow::{Block, Cell, Emphasis, FlowDocument, Inline, Item, List, Table};

/// What to include beyond the text.
#[derive(Debug, Clone)]
pub struct Options {
    /// Emit running headers and footers as a trailing section.
    ///
    /// Off by default: they are furniture, and a reader of the export wants the
    /// document. On for anyone auditing what the reconstruction found.
    pub include_running: bool,
    /// Emit unclassified blocks, marked as such.
    ///
    /// On, and it should stay on for anything a person will read: text this
    /// crate could not classify is still text that was in the document, and an
    /// export that silently omits it is an export that lies by omission.
    pub include_opaque: bool,
    /// Emit an image reference for each figure.
    pub include_figures: bool,
    /// Include text recovered from annotations.
    ///
    /// On. A filled form is a document whose text is entirely in annotations,
    /// and an export that omitted them would report it as blank.
    pub include_notes: bool,
    /// Mark vector artwork.
    ///
    /// On. Rules and borders never reach here — they are dropped as decoration
    /// during the conversion — so what is left is charts and diagrams, and an
    /// export that omits them silently tells the reader the page was empty.
    pub include_drawings: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            include_running: false,
            include_opaque: true,
            include_figures: true,
            include_notes: true,
            include_drawings: true,
        }
    }
}

/// Render a flow document as GitHub-flavoured Markdown.
pub fn render(doc: &FlowDocument, opts: &Options) -> String {
    let mut out = String::new();
    for block in &doc.blocks {
        render_block(block, opts, 0, &mut out);
    }

    if opts.include_running && !doc.running.is_empty() {
        out.push_str("\n---\n\n");
        // Not a heading in the document's own numbering: this section is about
        // the document rather than part of it, and giving it an `#` would put
        // it in the table of contents of anything that builds one.
        out.push_str("**Running heads and feet**\n\n");
        for running in &doc.running {
            let where_ = if running.top { "header" } else { "footer" };
            let pages = running.pages.len();
            let numbered = if running.is_page_number { ", page numbered" } else { "" };
            out.push_str(&format!(
                "- {} ({where_}, {pages} page(s){numbered})\n",
                escape(&running.template)
            ));
        }
    }

    // Exactly one trailing newline, whatever the last block was.
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn render_block(block: &Block, opts: &Options, indent: usize, out: &mut String) {
    match block {
        Block::Heading { level, inlines, .. } => {
            out.push_str(&"#".repeat((*level).clamp(1, 6) as usize));
            out.push(' ');
            // Bold is dropped inside a heading. Headings are set in a bold face
            // in most documents, so carrying the emphasis through renders
            // `# **Title**` — which adds nothing a heading did not already say
            // and is the standard tell of a mechanical conversion. Italic
            // survives: inside a heading it still distinguishes something.
            let plain: Vec<Inline> = inlines
                .iter()
                .map(|inline| match inline {
                    Inline::Text { text, emphasis } => Inline::Text {
                        text: text.clone(),
                        emphasis: Emphasis { bold: false, ..*emphasis },
                    },
                    other => other.clone(),
                })
                .collect();
            out.push_str(&inlines_to_markdown(&plain));
            out.push_str("\n\n");
        }

        Block::Paragraph { inlines, .. } => {
            let text = inlines_to_markdown(inlines);
            if text.trim().is_empty() {
                return;
            }
            out.push_str(&text);
            out.push_str("\n\n");
        }

        Block::List(list) => {
            render_list(list, opts, indent, out);
            if indent == 0 {
                out.push('\n');
            }
        }

        Block::Table(table) => {
            render_table(table, out);
            out.push('\n');
        }

        Block::Figure { alt, image, .. } => {
            if !opts.include_figures {
                return;
            }
            // The path is a name, not a promise: no pixels are extracted here,
            // so it points at a file the caller may or may not have written.
            // Named after the object so two references to one XObject produce
            // one filename, which is what a caller extracting them wants.
            let name = match image.object {
                Some(id) => format!("image-{}-{}.png", id.number, id.generation),
                None => format!("inline-image-page-{}.png", image.page + 1),
            };
            let alt = alt.as_deref().unwrap_or("");
            out.push_str(&format!("![{}]({})\n\n", escape(alt), name));
        }

        Block::Drawing(drawing) => {
            if !opts.include_drawings {
                return;
            }
            // Marked rather than rendered. There is no image file to point at
            // and no path renderer here, and a reader who cannot see the chart
            // is much better served by knowing one is missing than by a gap.
            let (w, h) = drawing.size;
            out.push_str(&format!(
                "*[drawing: {} path(s), {:.0}x{:.0}pt]*\n\n",
                drawing.paths, w, h
            ));
        }

        Block::Note(note) => {
            if !opts.include_notes {
                return;
            }
            // Labelled, because this text was not on the page in the way the
            // rest of the export was: the viewer drew it from an annotation. A
            // reader who cannot tell the difference will attribute a form
            // field's value to the sentence above it.
            let label = match &note.field {
                Some(name) => format!("{} “{}”", note.kind, name),
                None => note.kind.clone(),
            };
            out.push_str(&format!("> **[{}]** {}\n\n", escape(&label), escape(&note.text)));
        }

        Block::Opaque { text, reason, .. } => {
            if !opts.include_opaque {
                return;
            }
            // A blockquote with a note, rather than a code fence: the text is
            // prose that could not be trusted, not code, and the marker has to
            // survive a reader skimming past it.
            out.push_str(&format!("> **[unclassified: {}]**\n>\n", reason.as_str()));
            for line in text.lines() {
                out.push_str("> ");
                out.push_str(&escape(line));
                out.push('\n');
            }
            out.push('\n');
        }
    }
}

fn render_list(list: &List, opts: &Options, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    for (i, item) in list.items.iter().enumerate() {
        let marker = if list.ordered { format!("{}. ", i + 1) } else { "- ".to_string() };
        render_item(item, &marker, &pad, opts, indent, out);
    }
}

fn render_item(
    item: &Item,
    marker: &str,
    pad: &str,
    opts: &Options,
    indent: usize,
    out: &mut String,
) {
    let mut body = String::new();
    for block in &item.blocks {
        render_block(block, opts, indent + 1, &mut body);
    }
    let body = body.trim_end();
    if body.is_empty() {
        return;
    }

    // Continuation lines are indented to the marker's width so a multi-block
    // item stays one item. Getting this wrong does not look like a bug — it
    // looks like the list ended early, which is the kind of mistake an export
    // reader blames on the document.
    let continuation = " ".repeat(marker.chars().count());
    for (i, line) in body.lines().enumerate() {
        if i == 0 {
            out.push_str(pad);
            out.push_str(marker);
        } else if line.is_empty() {
            out.push('\n');
            continue;
        } else {
            out.push_str(pad);
            out.push_str(&continuation);
        }
        out.push_str(line);
        out.push('\n');
    }
}

fn render_table(table: &Table, out: &mut String) {
    let width = table.rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 || table.rows.is_empty() {
        return;
    }

    // GFM has no syntax for a table without a header row. Rather than invent
    // one by promoting the first row — which would delete a row of data from
    // the reader's view — an empty header is emitted and the data stays whole.
    let (header, body) = if table.has_header {
        (table.rows[0].clone(), &table.rows[1..])
    } else {
        (vec![Cell::default(); width], &table.rows[..])
    };

    write_row(&header, width, out);
    out.push('|');
    for _ in 0..width {
        out.push_str(" --- |");
    }
    out.push('\n');
    for row in body {
        write_row(row, width, out);
    }
}

fn write_row(row: &[Cell], width: usize, out: &mut String) {
    out.push('|');
    for i in 0..width {
        out.push(' ');
        let text = row.get(i).map(Cell::text).unwrap_or_default();
        // A newline inside a cell would end the table, and a pipe would add a
        // column. Both are silent corruptions of everything below them.
        out.push_str(&escape(&text.replace('\n', " ")).replace('|', "\\|"));
        out.push_str(" |");
    }
    out.push('\n');
}

fn inlines_to_markdown(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for inline in inlines {
        match inline {
            Inline::Break => out.push_str("  \n"),
            Inline::Text { text, emphasis } => {
                let escaped = escape(text);
                out.push_str(&wrap(&escaped, *emphasis));
            }
        }
    }
    out
}

/// Wrap a run in its emphasis markers.
///
/// The markers have to sit against non-space characters or Markdown does not
/// apply them, so the run's own leading and trailing spaces are moved outside.
/// Without this a bold run that happens to end with a space renders as literal
/// asterisks, which is the most common way emphasis output goes wrong.
fn wrap(text: &str, emphasis: Emphasis) -> String {
    if emphasis.is_plain() || text.trim().is_empty() {
        return text.to_string();
    }
    let marker = match (emphasis.bold, emphasis.italic) {
        (true, true) => "***",
        (true, false) => "**",
        (false, true) => "*",
        (false, false) => "",
    };
    // Invisible text carries no marker of its own: it is a property of how the
    // page was painted, not of how the text reads, and Markdown has no way to
    // say it. It survives as text, which is the part that matters.
    if marker.is_empty() {
        return text.to_string();
    }

    let lead: String = text.chars().take_while(|c| c.is_whitespace()).collect();
    let tail: String = text
        .chars()
        .rev()
        .take_while(|c| c.is_whitespace())
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    let core = &text[lead.len()..text.len() - tail.len()];
    format!("{lead}{marker}{core}{marker}{tail}")
}

/// Escape the characters that would otherwise be Markdown syntax.
///
/// Deliberately narrow. Escaping every special character produces output full
/// of backslashes that reads worse than the risk it avoids; these are the ones
/// that actually change the rendering of ordinary prose extracted from a PDF.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}
