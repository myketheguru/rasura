//! Moving content on the page. Spec 9.2's block operations, Phase 6.
//!
//! > `move_block(block, point)`, `resize_block(block, rect)`
//!
//! # Why a wrapper and not a rewritten matrix
//!
//! The obvious implementation is to find the `cm` that positioned the content
//! and change its operands. It does not work, for two reasons that only show up
//! on real files.
//!
//! A CTM is *accumulated*. By the time an image is drawn, the transform in
//! force may be the product of the page's base matrix, a `cm` in an enclosing
//! `q`, another inside a form XObject's `/Matrix`, and one immediately before
//! the `Do`. There is no single "the `cm`" to edit, and the last one in the
//! chain is not privileged — changing it moves everything else drawn under the
//! same `q` too.
//!
//! So instead the drawing operator is **wrapped**:
//!
//! ```text
//! q  a b c d e f cm  <the original operator, byte for byte>  Q
//! ```
//!
//! `q` and `Q` bracket the change, so nothing outside is affected by
//! construction rather than by analysis, and the original operator's bytes are
//! carried through untouched. That last point matters more than it looks: an
//! inline image's payload is *inside* its operator, and re-emitting it would
//! mean re-encoding pixel data this library deliberately never decodes.
//!
//! # Preserving rotation
//!
//! 1,129 of the 3,095 images in the corpus — **36%** — are rotated or skewed.
//! Moving one by rewriting its bounding box would flatten every one of them, so
//! the translation is composed rather than assigned.
//!
//! `cm` concatenates on the left: inserting `M cm` makes the effective
//! transform `M × CTM`. A device-space translation of `(dx, dy)` is `CTM × T`,
//! so `M = CTM × T × CTM⁻¹` — but computing it that way round-trips the whole
//! matrix through an inversion, and the linear part comes back as identity plus
//! floating-point litter. On an unmoved image that prints as `1 0 0 1 -0 0 cm`.
//!
//! Since `M` is always a *pure translation*, only its vector is unknown, and it
//! solves directly:
//!
//! ```text
//! M = translate(v),  where  v = (dx, dy) × linear(CTM)⁻¹
//! ```
//!
//! The linear part is then exactly identity by construction rather than by
//! luck. Note `v` is expressed in the space *before* the CTM: moving an image
//! scaled 200× by 50 device points is a translation of 0.25 in its own
//! coordinates, which is the number that appears in the file.

use crate::locate::EditablePage;
use crate::numfmt::NumberStyle;
use crate::patch::Patch;
use crate::session::{Compromise, Fidelity};
use crate::text::Edit;
use rasura_content::matrix::{Matrix, Point};
use rasura_layout::graphics::ImageBlock;

/// Why a block could not be moved.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BlockError {
    /// The content is inside a form XObject.
    ///
    /// Declined rather than attempted. A form may be invoked many times from
    /// many places; editing its stream moves *every* instance, which is not
    /// what "move this image" means. Moving one instance requires editing the
    /// invocation instead, and deciding which invocation is the caller's.
    #[error(
        "the content is inside a form XObject at depth {depth}; moving it would move every instance"
    )]
    InsideForm { depth: usize },

    /// The transform cannot be inverted, so no device-space move is definable.
    ///
    /// A singular CTM means the content is collapsed to a line or a point. It
    /// is drawn, and it has no area, and "move it by ten points on the page"
    /// has no answer in its own coordinates.
    #[error("the transform is singular, so a device-space move is undefined")]
    Singular,

    /// The operator's bytes are not where the block said they were.
    #[error("the operator at {0}..{1} could not be read")]
    Unreadable(usize, usize),

    /// An inline image has no XObject to swap.
    ///
    /// Its bytes live in the content stream, so replacing one means rewriting
    /// the operator rather than an object. Spec 10.4 says inline images are
    /// "editable in place"; doing it through the object path would silently do
    /// nothing.
    #[error("this is an inline image; it has no XObject to replace")]
    InlineNotAnObject,

    /// A scale factor of zero or a non-finite one.
    ///
    /// Zero is refused rather than treated as "make it invisible": it produces
    /// a singular transform, which cannot be undone by scaling back and which
    /// `move_image` would then reject. Deleting is the operation that means
    /// "make it go away", and it is reversible.
    #[error("a scale factor must be finite and non-zero")]
    Degenerate,

    /// The block carries no recorded path geometry.
    ///
    /// Either it genuinely has none, or the page had more painted paths than
    /// the collector retains geometry for — see `Graphics::geometry_truncated`.
    /// A map of fifty thousand strokes is reported as a region so that an edit
    /// does not reflow text through it, and is not offered for moving.
    #[error("this vector block has no recorded path geometry to move")]
    VectorNotAddressable,

    /// A path's operators are interleaved with something else.
    ///
    /// Moving artwork wraps its operators in `q … Q` with a transform, which
    /// works only if the range holds nothing but the path. A `W` inside it
    /// would have its clip undone at the `Q`; a colour set inside it would stop
    /// applying to everything after. Both change operators the caller did not
    /// ask to touch, which is the one thing §2's first property forbids — so
    /// this declines instead.
    #[error("the path's operators are interleaved with other content; moving it is not local")]
    VectorNotSelfContained,
}

/// Move an image by `(dx, dy)`. Spec 9.2.
///
/// Rotation, flips and shear are preserved: the translation is composed with
/// the image's own transform rather than replacing it.
///
/// # Which coordinates
///
/// The delta is in the **same space as [`ImageBlock::bbox`]** — device space,
/// including the page's base flip, so `y` increases downward. A caller that
/// read `bbox.y0` and wants the image thirty points lower on the page passes
/// `dy = 30.0`, and the new `bbox.y0` is thirty larger. Naming any other space
/// here would mean the number a caller reads and the number it passes back
/// disagree, which is the kind of API that is wrong exactly half the time.
pub fn move_image(
    page: &EditablePage,
    image: &ImageBlock,
    dx: f64,
    dy: f64,
) -> Result<Edit, BlockError> {
    if image.depth > 0 {
        return Err(BlockError::InsideForm { depth: image.depth });
    }
    let inverse = image.ctm.invert().ok_or(BlockError::Singular)?;

    // The device-space delta, expressed in the image's own coordinates.
    // `apply_vector` uses only the linear part, so the CTM's own translation
    // does not contaminate a displacement.
    let v = inverse.apply_vector(Point { x: dx, y: dy });
    let shift = Matrix::translate(v.x, v.y);

    let original = page
        .content
        .data()
        .get(image.span.clone())
        .ok_or(BlockError::Unreadable(image.span.start, image.span.end))?;

    Ok(Edit {
        patches: vec![Patch::new(image.span.clone(), wrap(original, &shift, &page.style))],
        // Nothing was re-encoded and nothing was approximated: the operator's
        // own bytes are carried through and a transform is composed around
        // them. There is no fidelity to lose.
        fidelity: Fidelity::Exact,
        text_after: String::new(),
    })
}

/// Scale an image by `(sx, sy)` about its own origin. Spec 10.4.
///
/// > `delete_image`, `move_image`, `resize_image` — content-stream level, no
/// > pixel work.
///
/// Factors rather than a target rectangle, deliberately. `resize_block(block,
/// rect)` reads well and cannot express a rotated image: fitting a
/// parallelogram to an axis-aligned rectangle means discarding the rotation,
/// and 36% of the corpus's images have one. Scaling composes with the existing
/// transform and preserves it.
///
/// The anchor is the image's own local origin — the corner the transform's
/// translation places — so scaling and moving compose predictably. A caller
/// wanting to grow an image about its centre scales, then moves by half the
/// difference.
pub fn scale_image(
    page: &EditablePage,
    image: &ImageBlock,
    sx: f64,
    sy: f64,
) -> Result<Edit, BlockError> {
    if image.depth > 0 {
        return Err(BlockError::InsideForm { depth: image.depth });
    }
    if !(sx.is_finite() && sy.is_finite()) || sx == 0.0 || sy == 0.0 {
        return Err(BlockError::Degenerate);
    }

    // `S cm` before the drawing operator gives CTM' = S × CTM: the unit square
    // is scaled in its own space and then transformed as before, so rotation
    // and shear survive untouched.
    let shift = Matrix::new(sx, 0.0, 0.0, sy, 0.0, 0.0);
    let original = page
        .content
        .data()
        .get(image.span.clone())
        .ok_or(BlockError::Unreadable(image.span.start, image.span.end))?;

    Ok(Edit {
        patches: vec![Patch::new(image.span.clone(), wrap(original, &shift, &page.style))],
        fidelity: Fidelity::Exact,
        text_after: String::new(),
    })
}

/// Remove an image from the page. Spec 10.4.
///
/// The drawing operator is deleted and nothing else. Any `q`/`cm`/`Q` the
/// producer wrapped it in is left in place: without a `Do` between them those
/// operators paint nothing, and removing them would mean deciding whether they
/// also positioned something else — which, inside a `q`, they often did.
///
/// The image *object* is not touched. It may be drawn on other pages, and an
/// unreferenced XObject costs bytes rather than correctness. Reclaiming it
/// belongs to a compacting save, where the whole document is in view.
pub fn delete_image(page: &EditablePage, image: &ImageBlock) -> Result<Edit, BlockError> {
    if image.depth > 0 {
        return Err(BlockError::InsideForm { depth: image.depth });
    }
    if page.content.data().get(image.span.clone()).is_none() {
        return Err(BlockError::Unreadable(image.span.start, image.span.end));
    }
    Ok(Edit {
        patches: vec![Patch::delete(image.span.clone())],
        fidelity: Fidelity::Exact,
        text_after: String::new(),
    })
}

/// What to do when the replacement image has different proportions. Spec 10.4.
///
/// > If the new image has different dimensions, either preserve the placement
/// > rectangle (default, stretch) or preserve the aspect ratio (opt-in,
/// > letterbox).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Keep the rectangle the old image occupied; distort if the shape differs.
    #[default]
    Stretch,
    /// Keep the new image's proportions, fitting inside the old rectangle.
    Letterbox,
}

/// An image to put in another's place.
#[derive(Debug, Clone)]
pub struct Replacement {
    /// The encoded bytes, exactly as they will be stored.
    pub data: Vec<u8>,
    /// The filter the bytes are already in — `DCTDecode` for a JPEG, `None`
    /// for raw samples. Taken rather than sniffed: guessing the codec from
    /// magic bytes is how a mislabelled stream becomes a corrupt file.
    pub filter: Option<&'static str>,
    pub width: u32,
    pub height: u32,
    /// `/ColorSpace`, e.g. `DeviceRGB`. Ignored for an image mask.
    pub colour_space: &'static str,
    pub bits_per_component: u8,
}

/// Swap the image an XObject holds. Spec 10.4.
///
/// > `replace_image(image_block, bytes, format)` — swap an XObject's data.
///
/// The caller supplies encoded bytes, so **no pixel work happens here**. That
/// is deliberate and it is what separates this from `resample_image`: this
/// library decodes no image format, and a replace that had to re-encode would
/// need one. A caller that has a JPEG can hand it over as a JPEG.
///
/// Returns the object change *and* an optional content patch. The object change
/// is always needed; the patch appears only when [`Fit::Letterbox`] has to
/// adjust the placement, so a same-shaped replacement leaves every page that
/// draws the image untouched.
#[derive(Debug, Clone)]
pub struct ImageReplacement {
    pub object: (rasura_cos::ObjId, rasura_cos::object::Object),
    pub patch: Option<Patch>,
    pub fidelity: Fidelity,
}

pub fn replace_image(
    page: &EditablePage,
    image: &ImageBlock,
    new: &Replacement,
    fit: Fit,
) -> Result<ImageReplacement, BlockError> {
    use rasura_cos::object::{Dictionary, Name, Object, Stream};

    let id = image.id.ok_or(BlockError::InlineNotAnObject)?;
    if new.width == 0 || new.height == 0 {
        return Err(BlockError::Degenerate);
    }

    let mut dict = Dictionary::new();
    dict.insert(Name::new("Type"), Object::name("XObject"));
    dict.insert(Name::new("Subtype"), Object::name("Image"));
    dict.insert(Name::new("Width"), Object::Integer(new.width as i64));
    dict.insert(Name::new("Height"), Object::Integer(new.height as i64));
    dict.insert(Name::new("BitsPerComponent"), Object::Integer(new.bits_per_component as i64));
    if image.is_mask {
        dict.insert(Name::new("ImageMask"), Object::Bool(true));
    } else {
        dict.insert(Name::new("ColorSpace"), Object::name(new.colour_space));
    }
    if let Some(filter) = new.filter {
        dict.insert(Name::new("Filter"), Object::name(filter));
    }

    // `set_raw` rather than `set_decoded`: the bytes are already in the filter
    // the dictionary declares, and asking the writer to encode them again would
    // double-compress a JPEG.
    let stream = Stream::new(dict, new.data.clone());

    let mut compromises = Vec::new();
    let patch = match fit {
        Fit::Stretch => {
            // The old rectangle is kept, so a differently-proportioned image is
            // distorted. Reported, because "the picture looks squashed" is
            // exactly the kind of thing a caller wants told rather than
            // discovered.
            if differs_in_shape(image, new) {
                compromises.push(Compromise::ImageDistorted);
            }
            None
        }
        Fit::Letterbox => fit_inside(page, image, new)?,
    };

    Ok(ImageReplacement {
        object: (id, Object::Stream(stream)),
        patch,
        fidelity: if compromises.is_empty() {
            Fidelity::Exact
        } else {
            Fidelity::Degraded(compromises)
        },
    })
}

/// Whether the replacement's proportions differ from the space it goes into.
fn differs_in_shape(image: &ImageBlock, new: &Replacement) -> bool {
    let (w, h) = (image.bbox.width(), image.bbox.height());
    if w <= 0.0 || h <= 0.0 {
        return false;
    }
    let placed = w / h;
    let source = new.width as f64 / new.height as f64;
    (placed - source).abs() > 0.01 * placed.max(source)
}

/// Shrink one axis so the new image keeps its proportions inside the old box.
fn fit_inside(
    page: &EditablePage,
    image: &ImageBlock,
    new: &Replacement,
) -> Result<Option<Patch>, BlockError> {
    if image.depth > 0 {
        return Err(BlockError::InsideForm { depth: image.depth });
    }
    let (w, h) = (image.bbox.width(), image.bbox.height());
    if w <= 0.0 || h <= 0.0 {
        return Err(BlockError::Degenerate);
    }
    let placed = w / h;
    let source = new.width as f64 / new.height as f64;
    if (placed - source).abs() <= 1e-9 {
        // Already the right shape; no placement change, so no patch and no
        // page is touched.
        return Ok(None);
    }

    // Scale the axis that would otherwise overflow. The image keeps its own
    // origin, matching `scale_image`.
    let (sx, sy) = if source > placed { (1.0, placed / source) } else { (source / placed, 1.0) };

    let original = page
        .content
        .data()
        .get(image.span.clone())
        .ok_or(BlockError::Unreadable(image.span.start, image.span.end))?;
    let shift = Matrix::new(sx, 0.0, 0.0, sy, 0.0, 0.0);
    Ok(Some(Patch::new(image.span.clone(), wrap(original, &shift, &page.style))))
}

/// Move a vector block. Not possible as modelled; see [`BlockError`].
pub fn move_vector(
    page: &EditablePage,
    block: &rasura_layout::graphics::VectorBlock,
    dx: f64,
    dy: f64,
) -> Result<Edit, BlockError> {
    if block.paths.is_empty() {
        // Either the block has no paths, or the page had more than the
        // collector retains geometry for. Both mean there is nothing here to
        // address, and the second is why the message says "recorded" rather
        // than "exists".
        return Err(BlockError::VectorNotAddressable);
    }

    let mut patches = Vec::with_capacity(block.paths.len());
    for path in &block.paths {
        if path.depth > 0 {
            return Err(BlockError::InsideForm { depth: path.depth });
        }
        // `sh` paints the clip and has no geometry of its own: moving it means
        // moving the clip, which is a different operator somewhere else.
        if path.paint == rasura_layout::graphics::Paint::Shading {
            return Err(BlockError::VectorNotSelfContained);
        }
        if !path.self_contained {
            return Err(BlockError::VectorNotSelfContained);
        }

        let inverse = path.ctm.invert().ok_or(BlockError::Singular)?;
        let v = inverse.apply_vector(Point { x: dx, y: dy });
        let shift = Matrix::translate(v.x, v.y);

        let original = page
            .content
            .data()
            .get(path.path_span.clone())
            .ok_or(BlockError::Unreadable(path.path_span.start, path.path_span.end))?;

        patches.push(Patch::new(path.path_span.clone(), wrap(original, &shift, &page.style)));
    }

    Ok(Edit {
        // The same reasoning as `move_image`: the operators' own bytes are
        // carried through and a transform is composed around them. Nothing was
        // re-encoded and nothing was approximated.
        patches,
        fidelity: Fidelity::Exact,
        text_after: String::new(),
    })
}

/// `q <matrix> cm <original> Q`
fn wrap(original: &[u8], shift: &Matrix, style: &NumberStyle) -> Vec<u8> {
    use rasura_content::op::OpKind;
    use rasura_cos::object::Object;

    let mut out = Vec::with_capacity(original.len() + 64);
    crate::emit::write_op(&mut out, &crate::emit::op(OpKind::Save, []), style);
    out.push(b'\n');

    let cm = crate::emit::op(
        OpKind::Concat,
        [shift.a, shift.b, shift.c, shift.d, shift.e, shift.f].map(Object::Real),
    );
    crate::emit::write_op(&mut out, &cm, style);
    out.push(b'\n');

    // Verbatim. An inline image's pixel payload lives inside this span, and it
    // is passed through rather than re-encoded -- this library never decodes
    // DCT, JPX, JBIG2 or CCITT, so re-emitting one is not on the table.
    out.extend_from_slice(original);

    out.push(b'\n');
    crate::emit::write_op(&mut out, &crate::emit::op(OpKind::Restore, []), style);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_content::matrix::Point;
    use rasura_cos::object::Object;
    use rasura_cos::testutil::ClassicBuilder;
    use rasura_cos::{Document, ObjId};

    /// A page drawing one image XObject through `content`.
    fn page_with(content: &[u8]) -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /XObject << /Im1 5 0 R >> >> >>",
            )
            .stream(4, "", content)
            .stream(
                5,
                "/Type /XObject /Subtype /Image /Width 2 /Height 2 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &[0u8, 255, 128, 64],
            )
            .finish("/Root 1 0 R")
    }

    fn analysed(bytes: Vec<u8>) -> (Document, EditablePage, Vec<ImageBlock>) {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let page = EditablePage::analyse(&doc, &pages.pages[0]).expect("analyse");
        let graphics = rasura_layout::graphics::collect(&doc, &pages.pages[0]);
        (doc, page, graphics.images)
    }

    #[test]
    fn an_image_records_the_operator_that_drew_it() {
        let (_doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        assert_eq!(images.len(), 1);

        let drawn = &page.content.data()[images[0].span.clone()];
        assert_eq!(String::from_utf8_lossy(drawn).trim(), "/Im1 Do");
        assert_eq!(images[0].depth, 0);
    }

    #[test]
    fn moving_an_image_wraps_it_and_leaves_its_bytes_alone() {
        let (_doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        let edit = move_image(&page, &images[0], 50.0, -25.0).expect("move");

        let written = String::from_utf8_lossy(&edit.patches[0].bytes).to_string();
        assert!(written.starts_with('q'), "{written}");
        assert!(written.trim_end().ends_with('Q'), "{written}");
        assert!(written.contains("/Im1 Do"), "the original operator survives: {written}");
        assert!(written.contains("cm"), "{written}");
    }

    #[test]
    fn the_image_lands_where_it_was_asked_to() {
        // The arithmetic, checked by re-analysing the edited page rather than
        // by re-deriving the matrix here -- which would only prove the test
        // agrees with itself.
        let original = page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n");
        let (mut doc, page, images) = analysed(original);
        let before = images[0].bbox;

        let edit = move_image(&page, &images[0], 50.0, -25.0).expect("move");
        let content = page.content;
        let mut session = crate::EditSession::new(&mut doc);
        session.patch_content("move", &content, &edit.patches, edit.fidelity).expect("patch");
        let saved = session.commit(&rasura_cos::SaveOptions::default()).expect("commit").bytes;

        let after_doc = Document::open(saved).expect("reopen");
        let pages = rasura_content::page::pages(&after_doc).expect("pages");
        let moved = rasura_layout::graphics::collect(&after_doc, &pages.pages[0]);
        let after = moved.images[0].bbox;

        assert!((after.x0 - (before.x0 + 50.0)).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.y0 - (before.y0 - 25.0)).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.width() - before.width()).abs() < 1e-6, "the size is unchanged");
    }

    #[test]
    fn a_rotated_image_stays_rotated() {
        // 36% of the corpus's images are rotated or skewed. Rewriting a bbox
        // would flatten every one of them, so this is the case the composed
        // translation exists for.
        let rotated = page_with(b"q 0 150 -150 0 300 400 cm /Im1 Do Q\n");
        let (mut doc, page, images) = analysed(rotated);
        let before = images[0].ctm;
        assert!(before.b.abs() > 1e-9, "the fixture really is rotated");

        let edit = move_image(&page, &images[0], 30.0, 40.0).expect("move");
        let content = page.content;
        let mut session = crate::EditSession::new(&mut doc);
        session.patch_content("move", &content, &edit.patches, edit.fidelity).expect("patch");
        let saved = session.commit(&rasura_cos::SaveOptions::default()).expect("commit").bytes;

        let after_doc = Document::open(saved).expect("reopen");
        let pages = rasura_content::page::pages(&after_doc).expect("pages");
        let after = rasura_layout::graphics::collect(&after_doc, &pages.pages[0]).images[0].ctm;

        // The linear part is untouched; only the translation moved.
        assert!((after.a - before.a).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.b - before.b).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.c - before.c).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.d - before.d).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.e - (before.e + 30.0)).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.f - (before.f + 40.0)).abs() < 1e-6, "{before:?} -> {after:?}");
    }

    #[test]
    fn an_inline_images_payload_is_carried_through_untouched() {
        // The payload is inside the operator's span. Re-emitting it would mean
        // re-encoding pixel data this library never decodes, so the bytes are
        // copied rather than regenerated.
        let content: &[u8] =
            b"q 100 0 0 100 10 10 cm BI /W 2 /H 2 /CS /G /BPC 8 ID \x00\xff\x80\x40 EI Q\n";
        let (_doc, page, images) = analysed(page_with(content));
        assert_eq!(images.len(), 1);
        assert!(images[0].inline);

        let edit = move_image(&page, &images[0], 5.0, 5.0).expect("move");
        let written = &edit.patches[0].bytes;
        assert!(
            written.windows(4).any(|w| w == [0x00, 0xff, 0x80, 0x40]),
            "the pixel bytes survive verbatim"
        );
    }

    #[test]
    fn an_image_inside_a_form_is_declined_by_name() {
        let bytes = ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(
                3,
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
                 /Resources << /XObject << /Fm1 6 0 R >> >> >>",
            )
            .stream(4, "", b"q 1 0 0 1 0 0 cm /Fm1 Do Q\n")
            .stream(
                5,
                "/Type /XObject /Subtype /Image /Width 2 /Height 2 \
                 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &[0u8, 255, 128, 64],
            )
            .stream(
                6,
                "/Type /XObject /Subtype /Form /BBox [0 0 612 792] \
                 /Resources << /XObject << /Im1 5 0 R >> >>",
                b"q 200 0 0 100 72 600 cm /Im1 Do Q\n",
            )
            .finish("/Root 1 0 R");

        let (_doc, page, images) = analysed(bytes);
        assert_eq!(images.len(), 1, "the image inside the form was found");
        assert!(images[0].depth > 0, "and it knows it is nested");

        let err = move_image(&page, &images[0], 10.0, 10.0).expect_err("declined");
        assert!(matches!(err, BlockError::InsideForm { .. }), "{err:?}");
    }

    #[test]
    fn a_collapsed_image_cannot_be_moved_in_device_space() {
        let (_doc, page, images) = analysed(page_with(b"q 0 0 0 0 72 600 cm /Im1 Do Q\n"));
        let err = move_image(&page, &images[0], 10.0, 0.0).expect_err("singular");
        assert!(matches!(err, BlockError::Singular), "{err:?}");
    }

    /// The vector blocks of a page, with their geometry.
    fn vectors_of(bytes: Vec<u8>) -> (Document, EditablePage, Vec<rasura_layout::VectorBlock>) {
        let doc = Document::open(bytes).expect("open");
        let pages = rasura_content::page::pages(&doc).expect("pages");
        let page = EditablePage::analyse(&doc, &pages.pages[0]).expect("analyse");
        let graphics = rasura_layout::graphics::collect(&doc, &pages.pages[0]);
        (doc, page, graphics.vectors)
    }

    #[test]
    fn a_drawing_moves_by_wrapping_its_own_operators() {
        // The whole path has to move, not just the painting operator: the
        // coordinates live in the `re` and `f` carries none of them.
        let (mut doc, page, vectors) = vectors_of(page_with(b"1 0 0 rg 10 10 100 50 re f\n"));
        let block = &vectors[0];
        let before = block.bbox;

        let edit = move_vector(&page, block, 20.0, -30.0).expect("moved");
        assert_eq!(edit.fidelity, Fidelity::Exact);
        assert_eq!(edit.patches.len(), 1);

        let mut session = crate::EditSession::new(&mut doc);
        session.patch_content("move", &page.content, &edit.patches, edit.fidelity).expect("apply");
        let saved = rasura_cos::save(session.document(), &rasura_cos::SaveOptions::default())
            .expect("save")
            .bytes;

        let (_, _, after) = vectors_of(saved);
        let moved = after.first().expect("still one drawing");
        assert!(
            (moved.bbox.x0 - (before.x0 + 20.0)).abs() < 0.01,
            "{before:?} -> {:?}",
            moved.bbox
        );
        // Device space flips y, so a -30 device move is +30 in the box.
        assert!(
            (moved.bbox.y0 - (before.y0 - 30.0)).abs() < 0.01,
            "{before:?} -> {:?}",
            moved.bbox
        );
        assert!((moved.bbox.width() - before.width()).abs() < 0.01, "moving does not resize it");
    }

    #[test]
    fn a_path_with_a_clip_inside_it_declines() {
        // `re W n` builds a path and clips with it. Wrapping that range in
        // `q … Q` would undo the clip at the `Q`, silently changing every
        // operator after it — which is exactly the non-locality §2 forbids.
        let (_doc, page, vectors) = vectors_of(page_with(b"10 10 100 50 re W f\n"));
        let block = vectors.first().expect("the filled path is artwork");
        let err = move_vector(&page, block, 1.0, 1.0).expect_err("declined");
        assert!(matches!(err, BlockError::VectorNotSelfContained), "{err:?}");
    }

    #[test]
    fn a_colour_change_inside_a_path_declines_too() {
        // Legal, rare, and the same hazard: the `rg` would stop applying to
        // everything after the `Q`.
        let (_doc, page, vectors) = vectors_of(page_with(b"10 10 m 1 0 0 rg 90 90 l 90 10 l f\n"));
        let block = vectors.first().expect("artwork");
        assert!(matches!(
            move_vector(&page, block, 1.0, 1.0),
            Err(BlockError::VectorNotSelfContained)
        ));
    }

    #[test]
    fn a_page_too_dense_to_retain_geometry_declines_rather_than_guessing() {
        // Above the collector's cap the page's artwork is reported as one
        // region — so an edit will not reflow text through it — with no
        // geometry, so there is nothing to move. Both halves matter: the region
        // has to exist, and moving it has to fail rather than move the wrong
        // thing.
        let mut content = String::new();
        for i in 0..4100 {
            let x = (i % 500) as f64 * 1.2;
            let y = (i / 500) as f64 * 1.2;
            content.push_str(&format!("{x} {y} 1 1 re f\n"));
        }
        let (_doc, page, vectors) = vectors_of(page_with(content.as_bytes()));

        let block = vectors.first().expect("the artwork is still reported");
        assert!(block.count > 4000);
        assert!(block.paths.is_empty(), "geometry is not retained above the cap");

        let err = move_vector(&page, block, 1.0, 1.0).expect_err("declined");
        assert!(matches!(err, BlockError::VectorNotAddressable), "{err:?}");
    }

    #[test]
    fn scaling_an_image_changes_its_size_and_keeps_its_corner() {
        let original = page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n");
        let (mut doc, page, images) = analysed(original);
        let before = images[0].bbox;

        let edit = scale_image(&page, &images[0], 2.0, 0.5).expect("scale");
        let content = page.content;
        let mut session = crate::EditSession::new(&mut doc);
        session.patch_content("scale", &content, &edit.patches, edit.fidelity).expect("patch");
        let saved = session.commit(&rasura_cos::SaveOptions::default()).expect("commit").bytes;

        let after_doc = Document::open(saved).expect("reopen");
        let pages = rasura_content::page::pages(&after_doc).expect("pages");
        let after = rasura_layout::graphics::collect(&after_doc, &pages.pages[0]).images[0].bbox;

        assert!((after.width() - before.width() * 2.0).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.height() - before.height() * 0.5).abs() < 1e-6, "{before:?} -> {after:?}");
        // The local origin is the anchor. With this transform the page flip
        // puts it at the box's top edge, so that edge is what holds still.
        assert!((after.x0 - before.x0).abs() < 1e-6, "the anchored corner did not move");
    }

    #[test]
    fn scaling_preserves_rotation() {
        let (mut doc, page, images) = analysed(page_with(b"q 0 150 -150 0 300 400 cm /Im1 Do Q\n"));
        let before = images[0].ctm;

        let edit = scale_image(&page, &images[0], 2.0, 2.0).expect("scale");
        let content = page.content;
        let mut session = crate::EditSession::new(&mut doc);
        session.patch_content("scale", &content, &edit.patches, edit.fidelity).expect("patch");
        let saved = session.commit(&rasura_cos::SaveOptions::default()).expect("commit").bytes;

        let after_doc = Document::open(saved).expect("reopen");
        let pages = rasura_content::page::pages(&after_doc).expect("pages");
        let after = rasura_layout::graphics::collect(&after_doc, &pages.pages[0]).images[0].ctm;

        // Every linear term doubled; the rotation angle is unchanged because
        // both axes scaled together.
        assert!((after.a - before.a * 2.0).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.b - before.b * 2.0).abs() < 1e-6, "{before:?} -> {after:?}");
        assert!((after.e - before.e).abs() < 1e-6, "the anchor did not move");
        assert!((after.f - before.f).abs() < 1e-6, "the anchor did not move");
    }

    #[test]
    fn a_zero_scale_is_refused_rather_than_making_it_invisible() {
        // It produces a singular transform, which cannot be scaled back and
        // which `move_image` would then reject. `delete_image` is the
        // operation that means "make it go away", and it is reversible.
        let (_doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        for (sx, sy) in [(0.0, 1.0), (1.0, 0.0), (f64::NAN, 1.0), (1.0, f64::INFINITY)] {
            let err = scale_image(&page, &images[0], sx, sy).expect_err("refused");
            assert!(matches!(err, BlockError::Degenerate), "{sx} {sy}: {err:?}");
        }
    }

    #[test]
    fn deleting_an_image_removes_it_and_leaves_its_wrapper() {
        // The `q`/`cm`/`Q` stay. Without a `Do` between them they paint
        // nothing, and removing them would mean deciding whether they also
        // positioned something else -- which inside a `q` they often did.
        let (mut doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        let edit = delete_image(&page, &images[0]).expect("delete");
        assert!(edit.patches[0].bytes.is_empty());

        let content = page.content;
        let mut session = crate::EditSession::new(&mut doc);
        session.patch_content("delete", &content, &edit.patches, edit.fidelity).expect("patch");
        let saved = session.commit(&rasura_cos::SaveOptions::default()).expect("commit").bytes;

        let after_doc = Document::open(saved).expect("reopen");
        let pages = rasura_content::page::pages(&after_doc).expect("pages");
        let after = rasura_layout::graphics::collect(&after_doc, &pages.pages[0]);
        assert!(after.images.is_empty(), "the image is gone");

        let stream = after_doc.decoded_stream(rasura_cos::ObjId::new(4, 0)).expect("stream");
        let text = String::from_utf8_lossy(&stream);
        assert!(text.contains("cm"), "the wrapper survives: {text:?}");
        assert!(!text.contains("Do"), "the drawing operator does not: {text:?}");
    }

    #[test]
    fn deleting_an_image_leaves_its_object_alone() {
        // It may be drawn on other pages. An unreferenced XObject costs bytes,
        // not correctness, and reclaiming it needs the whole document in view.
        let (mut doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        let edit = delete_image(&page, &images[0]).expect("delete");
        let content = page.content;
        let mut session = crate::EditSession::new(&mut doc);
        session.patch_content("delete", &content, &edit.patches, edit.fidelity).expect("patch");
        let saved = session.commit(&rasura_cos::SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        assert!(after.get(rasura_cos::ObjId::new(5, 0)).is_ok(), "the XObject is still there");
    }

    fn jpeg_like(width: u32, height: u32) -> Replacement {
        Replacement {
            data: b"\xff\xd8\xff\xe0not really a jpeg".to_vec(),
            filter: Some("DCTDecode"),
            width,
            height,
            colour_space: "DeviceRGB",
            bits_per_component: 8,
        }
    }

    #[test]
    fn replacing_an_image_swaps_its_object_and_keeps_the_bytes_encoded() {
        // The caller hands over encoded bytes and they are stored as-is. This
        // library decodes no image format, so a replace that re-encoded would
        // need one -- and would double-compress a JPEG.
        let (mut doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        let new = jpeg_like(400, 200);
        let out = replace_image(&page, &images[0], &new, Fit::Stretch).expect("replace");

        assert_eq!(out.object.0, ObjId::new(5, 0));
        assert!(out.patch.is_none(), "the same shape needs no placement change");

        let mut session = crate::EditSession::new(&mut doc);
        session
            .set_objects("replace", &[(out.object.0, Some(out.object.1))], out.fidelity)
            .expect("set");
        let saved = session.commit(&rasura_cos::SaveOptions::default()).expect("commit").bytes;

        let after = Document::open(saved).expect("reopen");
        let object = after.get(ObjId::new(5, 0)).expect("image");
        let stream = object.as_stream().expect("stream");
        assert_eq!(stream.raw(), &new.data[..], "stored byte for byte");
        assert_eq!(
            stream.dict.get("Filter").and_then(Object::as_name).and_then(|n| n.as_str()),
            Some("DCTDecode")
        );
        assert_eq!(stream.dict.get("Width").and_then(Object::as_i64), Some(400));
    }

    #[test]
    fn stretching_a_differently_shaped_image_says_it_distorted() {
        // "The picture looks squashed" is exactly what a caller wants told
        // rather than discovered.
        let (_doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        let out =
            replace_image(&page, &images[0], &jpeg_like(100, 400), Fit::Stretch).expect("replace");
        match &out.fidelity {
            Fidelity::Degraded(list) => assert!(list.contains(&Compromise::ImageDistorted)),
            other => panic!("expected a distortion report, got {other:?}"),
        }
    }

    #[test]
    fn letterboxing_shrinks_the_axis_that_would_overflow() {
        let (mut doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        let before = images[0].bbox;
        // A tall image into a wide box: the width must come in.
        let out = replace_image(&page, &images[0], &jpeg_like(100, 400), Fit::Letterbox)
            .expect("replace");
        let patch = out.patch.expect("a placement change is needed");

        let content = page.content;
        let mut session = crate::EditSession::new(&mut doc);
        session
            .set_objects("replace", &[(out.object.0, Some(out.object.1))], Fidelity::Exact)
            .expect("set");
        session.patch_content("refit", &content, &[patch], out.fidelity).expect("patch");
        let saved = session.commit(&rasura_cos::SaveOptions::default()).expect("commit").bytes;

        let after_doc = Document::open(saved).expect("reopen");
        let pages = rasura_content::page::pages(&after_doc).expect("pages");
        let after = rasura_layout::graphics::collect(&after_doc, &pages.pages[0]).images[0].bbox;

        assert!(after.width() < before.width(), "{before:?} -> {after:?}");
        assert!((after.height() - before.height()).abs() < 1e-6, "the tall axis is unchanged");
        // And it now has the source's proportions.
        assert!((after.width() / after.height() - 0.25).abs() < 1e-6, "{after:?}");
    }

    #[test]
    fn letterboxing_a_same_shaped_image_touches_no_page() {
        let (_doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        let out = replace_image(&page, &images[0], &jpeg_like(400, 200), Fit::Letterbox)
            .expect("replace");
        assert!(out.patch.is_none(), "no placement change, so no page is rewritten");
        assert!(out.fidelity.is_exact());
    }

    #[test]
    fn an_inline_image_cannot_be_replaced_through_the_object_path() {
        // Its bytes are in the content stream. Doing this through the object
        // path would silently do nothing.
        let content: &[u8] =
            b"q 100 0 0 100 10 10 cm BI /W 2 /H 2 /CS /G /BPC 8 ID \x00\xff\x80\x40 EI Q\n";
        let (_doc, page, images) = analysed(page_with(content));
        let err =
            replace_image(&page, &images[0], &jpeg_like(4, 4), Fit::Stretch).expect_err("declined");
        assert!(matches!(err, BlockError::InlineNotAnObject), "{err:?}");
    }

    #[test]
    fn a_zero_sized_replacement_is_refused() {
        let (_doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        let err =
            replace_image(&page, &images[0], &jpeg_like(0, 10), Fit::Stretch).expect_err("refused");
        assert!(matches!(err, BlockError::Degenerate), "{err:?}");
    }

    #[test]
    fn a_zero_move_is_still_a_valid_wrap() {
        // Degenerate but reachable: a caller dragging an image and dropping it
        // where it started. The identity transform is composed and the bytes
        // survive; nothing special-cases it into a no-op, because a no-op that
        // silently produced no patch would leave the session's log wrong.
        let (_doc, page, images) = analysed(page_with(b"q 200 0 0 100 72 600 cm /Im1 Do Q\n"));
        let edit = move_image(&page, &images[0], 0.0, 0.0).expect("move");
        let written = String::from_utf8_lossy(&edit.patches[0].bytes).to_string();
        assert!(written.contains("1 0 0 1 0 0 cm"), "an identity concat: {written}");
    }

    #[test]
    fn the_move_vector_is_in_the_images_own_coordinates_not_the_pages() {
        // The correction this test exists for: `M` sits *before* the CTM, so a
        // 50-point move of an image scaled 200x is a translation of 0.25 in the
        // file. Asserting 50 here would be asserting the device delta in the
        // wrong space -- and the bounding-box tests would still pass, because
        // they measure the outcome rather than the operand.
        let ctm = Matrix::new(200.0, 0.0, 0.0, 100.0, 72.0, 600.0);
        let v = ctm.invert().unwrap().apply_vector(Point { x: 50.0, y: -25.0 });
        assert!((v.x - 0.25).abs() < 1e-12, "{v:?}");
        assert!((v.y + 0.25).abs() < 1e-12, "{v:?}");

        // And composing it really does land the unit square 50 across, 25 down.
        let moved = Matrix::translate(v.x, v.y).then(&ctm);
        let p = moved.apply(Point { x: 0.0, y: 0.0 });
        assert!((p.x - 122.0).abs() < 1e-9 && (p.y - 575.0).abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn an_unmoved_image_gets_an_exact_identity_with_no_float_litter() {
        // Solving for the translation directly, rather than round-tripping the
        // whole matrix through an inversion, is what keeps the linear part
        // exactly identity. The round-trip version printed `1 0 0 1 -0 0 cm`,
        // which is valid PDF and reads in a diff as though something happened.
        let ctm = Matrix::new(200.0, 0.0, 0.0, 100.0, 72.0, 600.0);
        let v = ctm.invert().unwrap().apply_vector(Point { x: 0.0, y: 0.0 });
        assert_eq!(v.x, 0.0);
        assert_eq!(v.y, 0.0);
        assert!(!v.x.is_sign_negative() && !v.y.is_sign_negative(), "not negative zero");
    }
}
