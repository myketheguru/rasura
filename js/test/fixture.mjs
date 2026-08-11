// A document with more than text in it.
//
// The corpus sample every other test uses is a page of prose, which is the
// right fixture for the text path and useless for the rest of the catalogue:
// an image cannot be moved on a page that has none, and a field cannot be
// filled on a document with no form. So this builds one — two pages, an image
// XObject, and a text widget — rather than adding a binary to the corpus, which
// would be a fixture nobody could read a diff of.
//
// Written by hand rather than through the library, deliberately. A fixture the
// writer produced would agree with the reader by construction, and a test of
// `moveImage` that only ever sees content this codebase emitted has not seen a
// PDF.

/**
 * A classic cross-reference PDF, assembled from numbered objects.
 *
 * Offsets are tracked as the bytes are appended, because a table that disagrees
 * with the file is the one error that makes every downstream test meaningless
 * in a way that looks like a library bug.
 */
class Builder {
  constructor() {
    this.chunks = [];
    this.length = 0;
    this.offsets = new Map();
    this.push("%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
  }

  /** @param {string | Uint8Array} data */
  push(data) {
    const buf = typeof data === "string" ? Buffer.from(data, "latin1") : Buffer.from(data);
    this.chunks.push(buf);
    this.length += buf.length;
  }

  /** @param {number} n @param {string} body */
  object(n, body) {
    this.offsets.set(n, this.length);
    this.push(`${n} 0 obj\n${body}\nendobj\n`);
    return this;
  }

  /** @param {number} n @param {string} dict @param {Uint8Array} data */
  stream(n, dict, data) {
    this.offsets.set(n, this.length);
    this.push(`${n} 0 obj\n<< ${dict} /Length ${data.length} >>\nstream\n`);
    this.push(data);
    this.push("\nendstream\nendobj\n");
    return this;
  }

  /** @param {string} trailer */
  finish(trailer) {
    const count = Math.max(...this.offsets.keys()) + 1;
    const start = this.length;
    let table = `xref\n0 ${count}\n0000000000 65535 f \n`;
    for (let n = 1; n < count; n += 1) {
      const at = this.offsets.get(n) ?? 0;
      table += `${String(at).padStart(10, "0")} 00000 n \n`;
    }
    this.push(table);
    this.push(`trailer\n<< /Size ${count} ${trailer} >>\nstartxref\n${start}\n%%EOF\n`);
    return new Uint8Array(Buffer.concat(this.chunks));
  }
}

const latin1 = (s) => new Uint8Array(Buffer.from(s, "latin1"));

/**
 * Two pages, one image, one form field.
 *
 * Page 1 carries a paragraph, a 2×2 greyscale image drawn through `cm`/`Do`,
 * and a `/Widget` for the text field. Page 2 carries a paragraph, so deleting
 * one page leaves something to check.
 */
export function richer() {
  return new Builder()
    .object(
      1,
      "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [8 0 R] " +
        "/DA (/Helv 0 Tf 0 g) /DR << /Font << /Helv 5 0 R >> >> >> >>",
    )
    .object(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>")
    .object(
      3,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R " +
        "/Resources << /Font << /F1 5 0 R >> /XObject << /Im0 7 0 R >> >> " +
        "/Annots [8 0 R] >>",
    )
    .object(
      4,
      "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 9 0 R " +
        "/Resources << /Font << /F1 5 0 R >> >> >>",
    )
    .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
    .stream(
      6,
      "",
      latin1(
        "BT /F1 12 Tf 1 0 0 1 72 700 Tm (Quarterly revenue summary) Tj ET\n" +
          "q 120 0 0 60 72 560 cm /Im0 Do Q\n",
      ),
    )
    // A real image, not a stub: four grey pixels. `/Width` and `/Height` are
    // what `page.images()[n].pixels` reports, so a stub would make that check
    // agree with nothing.
    .stream(
      7,
      "/Type /XObject /Subtype /Image /Width 2 /Height 2 " +
        "/ColorSpace /DeviceGray /BitsPerComponent 8",
      new Uint8Array([0x00, 0x55, 0xaa, 0xff]),
    )
    .object(
      8,
      "<< /Type /Annot /Subtype /Widget /FT /Tx /T (signatory) " +
        "/Rect [72 400 372 424] /F 4 /DA (/Helv 12 Tf 0 g) /V () /P 3 0 R >>",
    )
    .stream(9, "", latin1("BT /F1 12 Tf 1 0 0 1 72 700 Tm (Notes and assumptions) Tj ET\n"))
    .finish("/Root 1 0 R");
}
