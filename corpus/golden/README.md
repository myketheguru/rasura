# Golden references for composition

`report.png` and `columns.png` are what the composed pages look like. Both are
every page of a fixed document, stacked, rendered by pdfium at 100 dpi.

They exist because pdfium confirming there is ink and pdf.js confirming the text
reads back say nothing about whether the typesetting is right. Three
keep-with-next bugs and a float comparison that cost a line per column were all
found by looking at a rendered page while the suite was green.

    cargo run --release -p rasura-pixeldiff --bin golden -- corpus/golden

If the output changed on purpose, look at the `.actual.png` the failure writes
before you bless anything:

    cargo run --release -p rasura-pixeldiff --bin golden -- corpus/golden --bless

The two cases are not redundant. Reintroducing the keep-with-next bug on purpose
leaves `report` matching and fails `columns` by 240,939 pixels: two columns
give twice as many boundaries for the fault to appear at, and a golden over the
single-column case alone would have passed.

The text is set in Roboto, Apache-2.0, which permits redistribution. The font
file itself is still fetched rather than committed.
