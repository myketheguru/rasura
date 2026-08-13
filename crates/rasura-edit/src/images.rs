//! Put a new image on a page. ISO 32000-1 §8.9.
//!
//! [`crate::blocks::replace_image`] swaps the bytes an XObject already holds:
//! the object exists, the page already names it, and the content stream already
//! draws it. Nothing has to be allocated or registered, which is why it is the
//! easier half and why it was the half that existed.
//!
//! Adding one needs all three: an object number, an entry in the page's
//! `/Resources /XObject` under a name nothing else uses, and a `Do` in the
//! content stream with a matrix that says where it goes.
//!
//! # No pixels are touched here
//!
//! The caller hands over encoded bytes and says what they are, exactly as for a
//! replacement. This library decodes no image format and does not intend to —
//! §3's non-goals put raster work outside it — so a caller with a JPEG passes a
//! JPEG and it is stored as one.
//!
//! # The content is appended, not rewritten
//!
//! `/Contents` may be a single stream or an array, and §7.8.2 says an array's
//! streams are concatenated as if they were one. So a new stream is appended to
//! the array rather than the existing bytes being edited, and **every object
//! that was already there keeps its bytes**. That is spec 2's first property
//! holding for an operation that could easily have broken it: adding a picture
//! to page 40 does not touch page 40's text.

use crate::blocks::BlockError;
use crate::blocks::Replacement;
use crate::numfmt::NumberStyle;
use crate::{Canvas, Fidelity};
use rasura_content::page::Page;
use rasura_cos::Document;
use rasura_cos::object::{Dictionary, Name, ObjId, Object, Stream};

/// Where an image goes on the page, in PDF page space: y upward from the
/// bottom-left of the media box, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Left edge.
    pub x: f64,
    /// **Bottom** edge. PDF space runs upward, and taking the top here would be
    /// a courtesy that silently disagrees with every other coordinate in the
    /// file.
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Placement {
    /// The image at its natural size, treating one sample as one point.
    pub fn natural(x: f64, y: f64, image: &Replacement) -> Placement {
        Placement { x, y, width: f64::from(image.width), height: f64::from(image.height) }
    }

    /// Scaled to fit inside `width` × `height` without distortion.
    pub fn fit(x: f64, y: f64, width: f64, height: f64, image: &Replacement) -> Placement {
        let (w, h) = (f64::from(image.width.max(1)), f64::from(image.height.max(1)));
        let scale = (width / w).min(height / h);
        Placement { x, y, width: w * scale, height: h * scale }
    }
}

/// What adding an image changed.
#[derive(Debug, Clone)]
pub struct ImageAddition {
    /// Ready for [`EditSession::set_objects`](crate::EditSession::set_objects).
    pub changes: Vec<(ObjId, Option<Object>)>,
    /// The XObject's id.
    pub id: ObjId,
    /// The name it was registered under, which is what the content stream says.
    pub name: Name,
    pub fidelity: Fidelity,
}

/// Draw `image` on `page` at `at`.
///
/// Object numbers come from the document's allocator; nothing is written until
/// the caller applies `changes`, so an added image is as undoable as any other
/// edit.
pub fn add_image(
    doc: &mut Document,
    page: &Page,
    image: &Replacement,
    at: &Placement,
) -> Result<ImageAddition, BlockError> {
    if image.width == 0 || image.height == 0 {
        return Err(BlockError::Degenerate);
    }
    // A zero-sized placement draws nothing at all, which is a caller mistake
    // worth refusing rather than writing a `0 0 0 0 x y cm` nobody can see.
    if at.width <= 0.0 || at.height <= 0.0 {
        return Err(BlockError::Degenerate);
    }

    let reserved = doc.reserve(2);
    let (xobject_id, content_id) = (reserved[0], reserved[1]);
    let mut changes: Vec<(ObjId, Option<Object>)> = Vec::new();

    // --- the image object ---------------------------------------------------
    let mut dict = Dictionary::new();
    dict.insert("Type", Object::name("XObject"));
    dict.insert("Subtype", Object::name("Image"));
    dict.insert("Width", Object::Integer(i64::from(image.width)));
    dict.insert("Height", Object::Integer(i64::from(image.height)));
    dict.insert("BitsPerComponent", Object::Integer(i64::from(image.bits_per_component)));
    dict.insert("ColorSpace", Object::name(image.colour_space));
    if let Some(filter) = image.filter {
        dict.insert("Filter", Object::name(filter));
    }
    // `Stream::new` takes raw bytes, which is what the caller supplied: they are
    // already in the filter the dictionary declares. Handing them to
    // `set_decoded` would ask the writer to encode them again and produce a
    // double-compressed JPEG.
    changes.push((xobject_id, Some(Object::Stream(Stream::new(dict, image.data.clone())))));

    // --- the resource entry -------------------------------------------------
    //
    // The effective resources, inherited ones included. A page with no
    // `/Resources` of its own inherits from an ancestor `/Pages` node, and
    // writing a fresh dictionary holding only this image would shadow that
    // inheritance -- taking every font on the page with it. Copying what was
    // in scope and adding to it is the only version that cannot lose anything.
    let mut resources =
        page.resources.as_ref().and_then(|r| r.as_dict().cloned()).unwrap_or_default();

    let mut xobjects = resources
        .get("XObject")
        .and_then(|x| doc.resolve(x).ok())
        .and_then(|x| x.as_dict().cloned())
        .unwrap_or_default();

    let name = fresh_name(&xobjects);
    xobjects.insert(name.clone(), Object::Reference(xobject_id));
    resources.insert("XObject", Object::Dictionary(xobjects));

    let mut page_dict = page.dict.clone();
    page_dict.insert("Resources", Object::Dictionary(resources));

    // --- the drawing --------------------------------------------------------
    //
    // §8.9.5.2: an image XObject is drawn in a unit square, so the matrix is
    // the size and the position at once.
    let mut canvas = Canvas::new(NumberStyle::default());
    canvas.save();
    canvas.concat(rasura_content::matrix::Matrix {
        a: at.width,
        b: 0.0,
        c: 0.0,
        d: at.height,
        e: at.x,
        f: at.y,
    });
    canvas.push_xobject(&name);
    canvas.restore();
    let drawing = canvas.finish().map_err(|_| BlockError::Degenerate)?;

    // A leading newline because §7.8.2 divides concatenated content streams
    // only between lexical tokens: a previous stream ending in `Q` with no
    // trailing whitespace would otherwise merge with our `q`.
    let mut bytes = Vec::with_capacity(drawing.len() + 1);
    bytes.push(b'\n');
    bytes.extend_from_slice(&drawing);
    let mut stream = Stream::new(Dictionary::new(), Vec::new());
    stream.set_decoded(bytes);
    changes.push((content_id, Some(Object::Stream(stream))));

    // Appended to the array rather than merged into the existing stream, so
    // every object already in the file keeps its bytes.
    let contents = match page_dict.get("Contents") {
        Some(Object::Array(existing)) => {
            let mut a = existing.clone();
            a.push(Object::Reference(content_id));
            a
        }
        Some(other) => vec![other.clone(), Object::Reference(content_id)],
        None => vec![Object::Reference(content_id)],
    };
    page_dict.insert("Contents", Object::Array(contents));
    changes.push((page.id, Some(Object::Dictionary(page_dict))));

    Ok(ImageAddition { changes, id: xobject_id, name, fidelity: Fidelity::Exact })
}

/// A resource name nothing in `existing` already uses.
///
/// Named rather than numbered from the count: a page whose `/XObject` holds
/// `Im0` and `Im2` has two entries, and `Im2` would collide.
fn fresh_name(existing: &Dictionary) -> Name {
    (0..)
        .map(|i| Name::new(format!("RasuraIm{i}")))
        .find(|n| existing.get_name(n).is_none())
        .expect("an unbounded range always yields an unused name")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditSession;
    use rasura_cos::testutil::ClassicBuilder;

    /// A 2×2 RGB image, uncompressed: twelve bytes of red, green, blue, white.
    fn swatch() -> Replacement {
        Replacement {
            data: vec![
                255, 0, 0, //
                0, 255, 0, //
                0, 0, 255, //
                255, 255, 255,
            ],
            filter: None,
            width: 2,
            height: 2,
            colour_space: "DeviceRGB",
            bits_per_component: 8,
        }
    }

    fn page_with_text() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .stream(4, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (existing text) Tj ET\n")
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .finish("/Root 1 0 R")
    }

    #[test]
    fn an_image_can_be_added_to_a_page_that_had_none() {
        let mut doc = Document::open(page_with_text()).unwrap();
        let pages = rasura_content::page::pages(&doc).unwrap();
        let page = pages.pages[0].clone();

        let added = add_image(
            &mut doc,
            &page,
            &swatch(),
            &Placement { x: 100.0, y: 500.0, width: 120.0, height: 120.0 },
        )
        .unwrap();
        {
            let mut session = EditSession::new(&mut doc);
            session.set_objects("add image", &added.changes, added.fidelity.clone()).unwrap();
        }

        // Through a save and a reopen, which is the only claim worth making.
        let saved = rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()).unwrap();
        let reopened = Document::open(saved.bytes).unwrap();
        assert_eq!(reopened.leniencies(), Vec::new());

        let pages = rasura_content::page::pages(&reopened).unwrap();
        let images = rasura_layout::graphics::collect(&reopened, &pages.pages[0]).images;
        assert_eq!(images.len(), 1, "the reader finds the image that was added");
        let placed = &images[0];
        assert!((placed.bbox.width() - 120.0).abs() < 0.5, "{:?}", placed.bbox);
        assert!((placed.bbox.x0 - 100.0).abs() < 0.5, "{:?}", placed.bbox);
    }

    #[test]
    fn adding_an_image_leaves_the_existing_content_stream_untouched() {
        // Spec 2's first property. The picture is appended as a second content
        // stream, so the bytes of the one that was there do not move -- which is
        // the difference between an edit and a rewrite.
        let original = page_with_text();
        let mut doc = Document::open(original.clone()).unwrap();
        let pages = rasura_content::page::pages(&doc).unwrap();
        let before = doc.decoded_stream(rasura_cos::ObjId::new(4, 0)).unwrap().to_vec();

        let added = add_image(
            &mut doc,
            &pages.pages[0],
            &swatch(),
            &Placement::natural(10.0, 10.0, &swatch()),
        )
        .unwrap();
        assert!(
            !added.changes.iter().any(|(id, _)| *id == rasura_cos::ObjId::new(4, 0)),
            "the existing content stream is not among the objects changed",
        );

        let mut session = EditSession::new(&mut doc);
        session.set_objects("add image", &added.changes, added.fidelity.clone()).unwrap();
        drop(session);

        let after = doc.decoded_stream(rasura_cos::ObjId::new(4, 0)).unwrap().to_vec();
        assert_eq!(before, after);
        // And the text still reads, which a merged stream could easily break.
        let pages = rasura_content::page::pages(&doc).unwrap();
        assert!(rasura_layout::page_text(&doc, &pages.pages[0]).contains("existing text"));
    }

    #[test]
    fn inherited_resources_survive() {
        // The page's font is on the page here; the case that matters is a font
        // inherited from the /Pages node, where writing a fresh /Resources
        // holding only the image would take the font with it.
        let inherited = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 \
                 /Resources << /Font << /F1 5 0 R >> >> >>",
            )
            .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>")
            .stream(4, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (inherited font) Tj ET\n")
            .object(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
            .finish("/Root 1 0 R");

        let mut doc = Document::open(inherited).unwrap();
        let pages = rasura_content::page::pages(&doc).unwrap();
        let added = add_image(
            &mut doc,
            &pages.pages[0],
            &swatch(),
            &Placement::natural(0.0, 0.0, &swatch()),
        )
        .unwrap();
        let mut session = EditSession::new(&mut doc);
        session.set_objects("add image", &added.changes, added.fidelity.clone()).unwrap();
        drop(session);

        let pages = rasura_content::page::pages(&doc).unwrap();
        assert!(
            rasura_layout::page_text(&doc, &pages.pages[0]).contains("inherited font"),
            "the inherited font was shadowed by the new /Resources",
        );
    }

    #[test]
    fn a_second_image_gets_its_own_name() {
        let mut doc = Document::open(page_with_text()).unwrap();
        let mut names = Vec::new();
        for _ in 0..3 {
            let pages = rasura_content::page::pages(&doc).unwrap();
            let added = add_image(
                &mut doc,
                &pages.pages[0],
                &swatch(),
                &Placement::natural(0.0, 0.0, &swatch()),
            )
            .unwrap();
            names.push(added.name.clone());
            let mut session = EditSession::new(&mut doc);
            session.set_objects("add image", &added.changes, added.fidelity.clone()).unwrap();
        }

        let unique: std::collections::BTreeSet<_> = names.iter().collect();
        assert_eq!(unique.len(), 3, "{names:?}");

        let pages = rasura_content::page::pages(&doc).unwrap();
        assert_eq!(rasura_layout::graphics::collect(&doc, &pages.pages[0]).images.len(), 3);
    }

    #[test]
    fn a_degenerate_image_or_placement_is_refused() {
        let mut doc = Document::open(page_with_text()).unwrap();
        let pages = rasura_content::page::pages(&doc).unwrap();
        let page = pages.pages[0].clone();

        let empty = Replacement { width: 0, ..swatch() };
        let at = Placement { x: 0.0, y: 0.0, width: 10.0, height: 10.0 };
        assert!(add_image(&mut doc, &page, &empty, &at).is_err());

        let nowhere = Placement { x: 0.0, y: 0.0, width: 0.0, height: 10.0 };
        assert!(add_image(&mut doc, &page, &swatch(), &nowhere).is_err());
    }

    #[test]
    fn fit_preserves_proportions() {
        let wide = Replacement { width: 400, height: 100, ..swatch() };
        let p = Placement::fit(0.0, 0.0, 200.0, 200.0, &wide);
        assert_eq!((p.width, p.height), (200.0, 50.0));
    }
}
