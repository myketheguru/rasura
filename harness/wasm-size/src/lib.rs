//! Size probe for spec §18 question Q6.
//!
//! > What is the smallest `core` chunk that can still parse and extract? If it
//! > exceeds 900 KB gzipped, the layout engine may need to become a third lazy
//! > chunk.
//!
//! `rasura-cos` is the *floor* for that chunk: `core` is cos + content +
//! layout, so whatever cos costs, core costs at least that. Measuring it now
//! says whether the budget is comfortable or whether the module split in §12.3
//! has to change before content and layout are written against it.
//!
//! # Why this is a `cdylib` with `extern "C"` and not wasm-bindgen
//!
//! wasm-bindgen adds its own glue, and the question here is what the *Rust*
//! side costs. This measures that; the real `rasura-wasm` crate adds bindgen
//! glue on top, which is a separate and much smaller number.
//!
//! `rasura-cos` uses `std` -- `HashMap`, `Arc`, `RefCell` -- so this links
//! `std` too, which is the honest measurement: the shipped build will link it
//! as well. `panic = "abort"` in the `wasm-release` profile keeps the unwinding
//! tables out.
//!
//! The exported functions exist to keep code reachable. Everything not reachable
//! from an export is stripped, so a probe that exported nothing would measure an
//! empty module and report a wonderfully small answer.

/// Hand the caller a buffer to write PDF bytes into.
#[unsafe(no_mangle)]
pub extern "C" fn alloc_input(len: usize) -> *mut u8 {
    let mut v: Vec<u8> = Vec::with_capacity(len);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// Open a document and report its object count. Reaches the lexer, the parser,
/// every cross-reference form, the recovery scan, and the security handler.
///
/// # Safety
/// `ptr` must point at `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn probe_open(ptr: *const u8, len: usize) -> usize {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match rasura_cos::Document::open(bytes.to_vec()) {
        Ok(doc) => doc.xref().len(),
        Err(_) => 0,
    }
}

/// Open and decode every stream. Reaches the filter chain and the predictors.
///
/// # Safety
/// `ptr` must point at `len` readable bytes.
#[cfg(any(feature = "full", feature = "read-only"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn probe_read(ptr: *const u8, len: usize) -> usize {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let Ok(doc) = rasura_cos::Document::open(bytes.to_vec()) else { return 0 };
    let mut total = 0usize;
    for number in doc.xref().live_objects() {
        let id = rasura_cos::ObjId::new(number, 0);
        if let Ok(obj) = doc.get(id)
            && obj.as_stream().is_some()
            && let Ok(data) = doc.decoded_stream(id)
        {
            total = total.wrapping_add(data.len());
        }
    }
    total
}

/// Open, edit, and save. Reaches the writer, the re-encoder, and the
/// incremental-append path.
///
/// # Safety
/// `ptr` must point at `len` readable bytes.
#[cfg(feature = "full")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn probe_save(ptr: *const u8, len: usize) -> usize {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let Ok(mut doc) = rasura_cos::Document::open(bytes.to_vec()) else { return 0 };
    let id = rasura_cos::ObjId::new(1, 0);
    if let Ok(obj) = doc.get(id)
        && let Some(dict) = obj.as_dict()
    {
        let mut d = dict.clone();
        d.insert(rasura_cos::Name::new("ProbeMarker"), rasura_cos::Object::Integer(1));
        doc.set(id, rasura_cos::Object::Dictionary(d));
    }
    match rasura_cos::save(&doc, &rasura_cos::SaveOptions::default()) {
        Ok(r) => r.bytes.len(),
        Err(_) => 0,
    }
}

/// The whole `core` chunk: open a document, extract a page, run the §7.2 chain,
/// reconstruct it, and shape a run.
///
/// This is the variant spec 12.3's 900 KB budget applies to. Q6 measured the
/// object layer alone at 122.7 KB gzipped; shaping brings `rustybuzz`,
/// `ttf-parser` and a Unicode script table into the same chunk, and whether
/// that still fits is the question the module split in §12.3 turns on.
///
/// # Safety
/// `ptr` must point at `len` readable bytes.
#[cfg(feature = "core")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn probe_core(ptr: *const u8, len: usize) -> usize {
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let Ok(doc) = rasura_cos::Document::open(bytes.to_vec()) else { return 0 };
    let Ok(tree) = rasura_content::page::pages(&doc) else { return 0 };
    let Some(page) = tree.pages.first() else { return 0 };

    let (runs, _) = rasura_layout::resolve_page(&doc, page);
    let rules = rasura_layout::rules::collect(&doc, page);
    let regions = rasura_layout::detect(rasura_layout::place(&runs), &rules);
    let graphics = rasura_layout::graphics::collect(&doc, page);

    let mut total = regions.iter().map(|r| r.lines.len()).sum::<usize>() + graphics.images.len();
    for region in &regions {
        total += rasura_layout::reconstruct(region, &runs).len();
    }
    total += rasura_layout::detect_page(&regions, &rules, &runs).len();

    // And the shaper, which is the point of measuring this variant at all.
    let request = rasura_font::shape::request_for(
        "shaping",
        false,
        rasura_font::KerningSource::None,
        true,
        None,
    );
    total += rasura_font::shape(bytes, &request).map(|g| g.len()).unwrap_or(0);
    total
}
