//! The loaded document: object access, lazy decoding, and the dirty set.
//!
//! A `Document` holds the original bytes untouched for its whole life. Objects
//! are parsed on demand and cached; decoded stream contents are cached
//! separately, so a stream nobody looks at is never decoded and therefore never
//! re-encoded. Spec 12.5 targets peak memory at 3x file size for read-and-edit,
//! and this shape is how that is reached.
//!
//! Every object also remembers the byte span it occupied in the source file.
//! The writer replays those spans verbatim for anything unmodified, which is the
//! mechanism behind invariant I1: open, save with no edits, get the same bytes.

use crate::crypt::{Cipher, Decryptor, Permissions};
use crate::error::{CosError, Leniency, LeniencyKind, Result};
use crate::filters::{self, FilterChain};
use crate::object::{Dictionary, Name, ObjId, Object, PdfString};
use crate::parser::{FnResolver, NoResolve, Parser};
use crate::recovery;
use crate::xref::{self, RevisionInfo, XrefEntry, XrefStyle, XrefTable};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

/// Reference chains longer than this are a malformed file, not a deep document.
const MAX_RESOLVE_DEPTH: usize = 32;

/// Object number to `(offset, generation)`, built by scanning the whole file.
type HeaderIndex = HashMap<u32, (usize, u16)>;

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Tried as both user and owner password. The empty password is always
    /// attempted regardless.
    pub password: String,
    /// Whether to fall back to a full-file scan when the cross-reference table
    /// cannot be followed. Spec 11.2 defaults this to on.
    pub recovery: RecoveryPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecoveryPolicy {
    #[default]
    Auto,
    Never,
}

/// How the document was loaded. Drives what the writer is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMode {
    /// The cross-reference chain was followed successfully.
    Xref,
    /// The table was rebuilt by scanning. Forces `SaveMode::FullRewrite`.
    Reconstructed,
    /// There was no file: the document was built by [`Document::new`]. Forces
    /// `SaveMode::FullRewrite`, for the plainest of reasons — an incremental
    /// save appends to original bytes, and there are none.
    Created,
}

pub struct Document {
    /// The original file bytes. Never mutated.
    buf: Vec<u8>,
    /// Where `%PDF-` sits, which is not always byte 0.
    header_offset: usize,
    version: String,
    xref: XrefTable,
    decryptor: Option<Decryptor>,
    load_mode: LoadMode,
    /// Interior-mutable because leniencies are also discovered lazily, when an
    /// object is first loaded and its cross-reference entry turns out to be
    /// wrong. A file's leniency list is therefore only complete once everything
    /// in it has been read.
    leniencies: RefCell<Vec<Leniency>>,

    /// Parsed objects, keyed by id.
    cache: RefCell<HashMap<ObjId, Arc<Object>>>,
    /// Source byte spans of top-level indirect objects, for verbatim re-emission.
    spans: RefCell<HashMap<ObjId, Range<usize>>>,
    /// Decoded stream contents.
    decoded: RefCell<HashMap<ObjId, Arc<Vec<u8>>>>,
    /// Object-stream contents, decoded once and reused for every member.
    objstm: RefCell<HashMap<u32, Arc<ObjStmCache>>>,
    /// Lazily built map of object number to true file offset, used to repair
    /// cross-reference entries that point at the wrong place.
    header_index: RefCell<Option<Arc<HeaderIndex>>>,

    /// Objects the caller replaced or created. Ordered so output is stable.
    dirty: BTreeMap<ObjId, Object>,
    /// Object numbers deleted by the caller; written as free entries.
    deleted: BTreeMap<ObjId, ()>,
    /// Next object number to hand out.
    next_number: u32,
    /// Whether content has been redacted, which forces a full rewrite.
    ///
    /// Spec 10.6 step 7. Kept on the document rather than passed at save time
    /// so that no code path can save this document incrementally, whatever
    /// options it is handed.
    redacted: bool,
    /// What the next save should do about protection. Spec 5.5, Phase 8.
    ///
    /// Deliberately *not* stored by replacing `decryptor`: that field is the
    /// handler for the bytes in `buf`, and the writer still needs it to read
    /// them. A protection change means the input and the output have different
    /// keys, so both have to exist at once.
    protection: ProtectionChange,
}

/// What the next save should do about the document's protection.
///
/// Spec 5.5's Phase 8 work. See [`crate::protect`] for how one is made and why
/// any change forces a full rewrite.
#[derive(Debug, Clone, Default)]
pub enum ProtectionChange {
    /// Save with whatever the input had, re-encrypting new content with the
    /// existing file key.
    #[default]
    Unchanged,
    /// Drop `/Encrypt` and write everything in the clear.
    Removed,
    /// Write `/Encrypt` and encrypt everything with this handler's key.
    Replaced {
        /// Where the `/Encrypt` dictionary will be written. Allocated when the
        /// change is made rather than at save time, because the handler has to
        /// know its own object number: `/Encrypt` is the one object that is
        /// never itself encrypted.
        id: ObjId,
        dict: Dictionary,
        decryptor: Decryptor,
    },
}

impl ProtectionChange {
    pub fn is_change(&self) -> bool {
        !matches!(self, ProtectionChange::Unchanged)
    }
}

struct ObjStmCache {
    data: Arc<Vec<u8>>,
    /// `(object number, offset from /First)`.
    pairs: Vec<(u32, usize)>,
    first: usize,
}

impl Default for Document {
    /// An empty document, as [`Document::new`].
    fn default() -> Document {
        Document::new()
    }
}

impl Document {
    // -----------------------------------------------------------------------
    // Loading
    // -----------------------------------------------------------------------

    /// A new, empty document: a catalog and a page tree with no pages in it.
    ///
    /// The catalog exists from the first moment on purpose. Every layer above
    /// this one assumes `catalog()` resolves — `open_with` refuses to return a
    /// document where it does not (see the `/Root` check below), and the writer
    /// walks reachability from `/Root`. Handing back a document that had to be
    /// made valid by its caller would put that invariant in the caller's hands,
    /// where sooner or later it would be dropped.
    ///
    /// Object 1 is the catalog and object 2 the root `/Pages` node. Nothing
    /// depends on those numbers; they are simply what an empty allocator hands
    /// out first.
    ///
    /// Saving is always a full rewrite ([`LoadMode::Created`]): an incremental
    /// save appends to the original bytes, and a created document has none.
    pub fn new() -> Document {
        Document::with_version("1.7")
    }

    /// As [`Document::new`], at a stated version.
    ///
    /// 1.7 is the default because it is ISO 32000-1, which is what the rest of
    /// this library implements. Ask for 2.0 only if you mean it.
    pub fn with_version(version: &str) -> Document {
        let mut doc = Document {
            buf: Vec::new(),
            header_offset: 0,
            version: version.to_string(),
            xref: XrefTable::default(),
            decryptor: None,
            load_mode: LoadMode::Created,
            leniencies: RefCell::new(Vec::new()),
            header_index: RefCell::new(None),
            cache: RefCell::new(HashMap::new()),
            spans: RefCell::new(HashMap::new()),
            decoded: RefCell::new(HashMap::new()),
            objstm: RefCell::new(HashMap::new()),
            dirty: BTreeMap::new(),
            deleted: BTreeMap::new(),
            next_number: 1,
            redacted: false,
            protection: ProtectionChange::Unchanged,
        };

        let catalog = doc.reserve(1)[0];
        let pages = doc.reserve(1)[0];

        let mut catalog_dict = Dictionary::new();
        catalog_dict.insert("Type", Object::name("Catalog"));
        catalog_dict.insert("Pages", Object::Reference(pages));
        doc.set(catalog, Object::Dictionary(catalog_dict));

        let mut pages_dict = Dictionary::new();
        pages_dict.insert("Type", Object::name("Pages"));
        pages_dict.insert("Kids", Object::Array(Vec::new()));
        pages_dict.insert("Count", Object::Integer(0));
        doc.set(pages, Object::Dictionary(pages_dict));

        doc.xref.trailer.insert("Root", Object::Reference(catalog));
        doc.xref.trailer.insert("Size", Object::Integer(doc.next_number as i64));
        doc
    }

    pub fn open(bytes: Vec<u8>) -> Result<Document> {
        Document::open_with(bytes, &OpenOptions::default())
    }

    pub fn open_with(bytes: Vec<u8>, opts: &OpenOptions) -> Result<Document> {
        let mut leniencies = Vec::new();

        let header_offset = match xref::find_header(&bytes) {
            Some(0) => 0,
            Some(at) => {
                leniencies.push(Leniency::new(
                    LeniencyKind::HeaderNotAtStart,
                    0,
                    format!("{at} bytes precede the %PDF- header"),
                ));
                at
            }
            // No header anywhere. If the buffer contains PDF objects it is a
            // PDF that lost its first line -- a truncated download, a mangled
            // mail attachment. Viewers open these, so recovery gets a chance
            // before this is called a failure.
            None => {
                if crate::parser::find_bytes(&bytes, b" obj").is_none() {
                    return Err(CosError::NotAPdf);
                }
                leniencies.push(Leniency::new(
                    LeniencyKind::HeaderNotAtStart,
                    0,
                    "no %PDF- header anywhere in the file",
                ));
                0
            }
        };
        let version = read_version(&bytes, header_offset);

        // Try the cross-reference chain; fall back to scanning.
        let (xref_table, load_mode, objstm_to_expand) = match xref::load(&bytes, header_offset) {
            Ok(loaded) => {
                leniencies.extend(loaded.leniencies);
                (loaded.table, LoadMode::Xref, Vec::new())
            }
            Err(e) if opts.recovery == RecoveryPolicy::Auto => {
                leniencies.push(Leniency::new(
                    LeniencyKind::BadStartxref,
                    0,
                    format!("cross-reference chain unusable ({e}); rebuilding"),
                ));
                let r = recovery::reconstruct(&bytes)?;
                leniencies.extend(r.leniencies);
                (r.table, LoadMode::Reconstructed, r.object_streams)
            }
            Err(e) => return Err(e),
        };

        let next_number = xref_table.next_free_number().max(xref_table.trailer_size());

        let mut doc = Document {
            buf: bytes,
            header_offset,
            version,
            xref: xref_table,
            decryptor: None,
            load_mode,
            leniencies: RefCell::new(leniencies),
            header_index: RefCell::new(None),
            cache: RefCell::new(HashMap::new()),
            spans: RefCell::new(HashMap::new()),
            decoded: RefCell::new(HashMap::new()),
            objstm: RefCell::new(HashMap::new()),
            dirty: BTreeMap::new(),
            deleted: BTreeMap::new(),
            next_number,
            redacted: false,
            protection: ProtectionChange::Unchanged,
        };

        doc.setup_encryption(&opts.password)?;

        // Recovery cannot expand object streams until decryption is ready.
        for container in objstm_to_expand {
            doc.register_objstm_members(container);
        }

        // A document whose /Root does not resolve is not usable. Try recovery
        // before giving up -- a plausible-looking xref that points at the wrong
        // objects is a real failure mode.
        //
        // The requirement is uniform: a reconstructed document has to produce a
        // catalog too. Accepting one without would mean `open` succeeds and
        // every subsequent operation fails, and a save would emit a file with
        // no document in it.
        if doc.catalog().is_err() && doc.load_mode == LoadMode::Xref {
            if opts.recovery == RecoveryPolicy::Never {
                return Err(CosError::malformed(0, "/Root does not resolve to a catalog"));
            }
            doc.leniencies.borrow_mut().push(Leniency::new(
                LeniencyKind::XrefReconstructed,
                0,
                "/Root did not resolve through the cross-reference table; rebuilding",
            ));
            let r = recovery::reconstruct(&doc.buf)?;
            doc.xref = r.table;
            doc.load_mode = LoadMode::Reconstructed;
            doc.leniencies.borrow_mut().extend(r.leniencies);
            doc.cache.borrow_mut().clear();
            doc.spans.borrow_mut().clear();
            doc.decoded.borrow_mut().clear();
            doc.objstm.borrow_mut().clear();
            doc.setup_encryption(&opts.password)?;
            for container in r.object_streams {
                doc.register_objstm_members(container);
            }
            doc.next_number = doc.xref.next_free_number().max(doc.xref.trailer_size());
        }
        if doc.catalog().is_err() {
            return Err(CosError::malformed(
                0,
                "no usable /Root: the file has no catalog, even after rebuilding",
            ));
        }

        Ok(doc)
    }

    fn setup_encryption(&mut self, password: &str) -> Result<()> {
        let Some(encrypt_obj) = self.xref.trailer.get("Encrypt").cloned() else {
            return Ok(());
        };
        let encrypt_ref = encrypt_obj.as_reference();

        // The /Encrypt dictionary is never itself encrypted, so it is loaded
        // before the decryptor exists.
        let encrypt_dict = match &encrypt_obj {
            Object::Dictionary(d) => d.clone(),
            Object::Reference(id) => {
                // An /Encrypt that will not resolve to a dictionary leaves no
                // way to know whether the content is protected. Guessing "not
                // encrypted" would hand back ciphertext dressed as content,
                // which spec 2 forbids: degradation must be reported, never
                // assumed.
                let obj = self.load_raw(*id).map_err(|e| {
                    CosError::UnsupportedEncryption(format!("/Encrypt {id} is unreadable: {e}"))
                })?;
                obj.as_dict()
                    .ok_or_else(|| {
                        CosError::UnsupportedEncryption(format!(
                            "/Encrypt {id} is not a dictionary"
                        ))
                    })?
                    .clone()
            }
            _ => {
                return Err(CosError::UnsupportedEncryption("/Encrypt is not a dictionary".into()));
            }
        };

        // /ID strings are likewise never encrypted.
        let id0 = self
            .xref
            .trailer
            .get("ID")
            .and_then(Object::as_array)
            .and_then(|a| a.first())
            .and_then(Object::as_string)
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default();

        self.decryptor = Some(Decryptor::new(&encrypt_dict, &id0, password, encrypt_ref)?);
        // Anything cached during bootstrap was read without decryption.
        self.cache.borrow_mut().clear();
        self.spans.borrow_mut().clear();
        Ok(())
    }

    /// After recovery, register the objects packed inside a container so they
    /// can be resolved.
    fn register_objstm_members(&mut self, container: u32) {
        let Ok(cache) = self.objstm_contents(container) else { return };
        let members: Vec<(u32, usize)> = cache.pairs.clone();
        for (index, (num, _)) in members.iter().enumerate() {
            // A top-level definition of the same object wins: it is either a
            // later revision or the reason the object stream is stale.
            if !matches!(self.xref.get(*num), Some(XrefEntry::InFile { .. })) {
                self.xref.insert(*num, XrefEntry::InObjStm { container, index: index as u32 });
            }
        }
    }

    // -----------------------------------------------------------------------
    // Metadata
    // -----------------------------------------------------------------------

    /// The original bytes, exactly as opened.
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn header_offset(&self) -> usize {
        self.header_offset
    }

    /// `"1.7"`, from the header, overridden by the catalog's `/Version` when
    /// that is higher (ISO 32000-1 §7.5.5).
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn trailer(&self) -> &Dictionary {
        &self.xref.trailer
    }

    pub fn xref(&self) -> &XrefTable {
        &self.xref
    }

    pub fn xref_style(&self) -> XrefStyle {
        self.xref.style
    }

    pub fn load_mode(&self) -> LoadMode {
        self.load_mode
    }

    /// Newest revision first. Spec 5.3: the redaction path needs to know old
    /// bytes are still present.
    pub fn revisions(&self) -> &[RevisionInfo] {
        &self.xref.revisions
    }

    /// Every spec deviation tolerated while reading this file.
    pub fn leniencies(&self) -> Vec<Leniency> {
        self.leniencies.borrow().clone()
    }

    fn note(&self, kind: LeniencyKind, offset: usize, detail: impl Into<String>) {
        self.leniencies.borrow_mut().push(Leniency::new(kind, offset, detail));
    }

    /// Find where an object really is, when its cross-reference entry lies.
    ///
    /// The whole-file index is built once, on the first entry that turns out to
    /// be wrong, and reused thereafter -- a file with a hundred bad offsets
    /// costs one pass, not a hundred.
    fn repair_offset(&self, number: u32) -> Option<usize> {
        if self.header_index.borrow().is_none() {
            let index = Arc::new(recovery::index_object_headers(&self.buf));
            *self.header_index.borrow_mut() = Some(index);
        }
        let borrowed = self.header_index.borrow();
        let index = borrowed.as_ref()?;
        index.get(&number).map(|&(offset, _)| offset)
    }

    pub fn is_encrypted(&self) -> bool {
        self.decryptor.is_some()
    }

    pub(crate) fn decryptor(&self) -> Option<&Decryptor> {
        self.decryptor.as_ref()
    }

    /// Advisory only; Rasura reports and does not enforce (spec 5.5).
    pub fn permissions(&self) -> Permissions {
        self.decryptor.as_ref().map_or_else(Permissions::all, |d| d.permissions)
    }

    /// True when the original file declared `/Linearized`. Appending a revision
    /// breaks it, which the writer reports as a warning.
    pub fn is_linearized(&self) -> bool {
        // The linearisation dictionary is the first object in the file.
        let mut parser = Parser::at(&self.buf, self.header_offset);
        // Skip the header comment line.
        if let Ok(io) = parser.parse_indirect_object(&NoResolve)
            && let Some(d) = io.object.as_dict()
        {
            return d.contains_key("Linearized");
        }
        false
    }

    /// The catalog's own object id.
    ///
    /// `catalog()` returns the resolved dictionary, which is enough to read and
    /// not enough to write: an edit that changes something the catalog holds
    /// inline has to write the catalog back, and needs to know which object
    /// that is. `None` for a trailer whose `/Root` is a direct dictionary,
    /// which is malformed but occurs.
    pub fn catalog_id(&self) -> Option<ObjId> {
        self.xref.trailer.get("Root")?.as_reference()
    }

    pub fn catalog(&self) -> Result<Arc<Object>> {
        let root = self
            .xref
            .trailer
            .get("Root")
            .ok_or_else(|| CosError::malformed(0, "trailer has no /Root"))?;
        let obj = self.resolve(root)?;
        match obj.as_dict() {
            Some(d) if d.type_name().is_none_or(|t| t.as_bytes() == b"Catalog") => Ok(obj),
            _ => Err(CosError::malformed(0, "/Root does not point at a catalog dictionary")),
        }
    }

    // -----------------------------------------------------------------------
    // Object access
    // -----------------------------------------------------------------------

    /// Fetch an indirect object, parsing and caching on first use.
    pub fn get(&self, id: ObjId) -> Result<Arc<Object>> {
        if let Some(replacement) = self.dirty.get(&id) {
            return Ok(Arc::new(replacement.clone()));
        }
        if let Some(hit) = self.cache.borrow().get(&id) {
            return Ok(Arc::clone(hit));
        }
        let obj = self.load(id)?;
        self.cache.borrow_mut().insert(id, Arc::clone(&obj));
        Ok(obj)
    }

    /// Like `get`, but returns `Null` rather than an error for an object the
    /// table does not list. ISO 32000-1 §7.3.10: a reference to a nonexistent
    /// object *is* null, and treating it as an error breaks legitimate files.
    pub fn get_or_null(&self, id: ObjId) -> Arc<Object> {
        self.get(id).unwrap_or_else(|_| Arc::new(Object::Null))
    }

    /// Follow references until a direct object is reached.
    pub fn resolve(&self, object: &Object) -> Result<Arc<Object>> {
        let mut current = object.clone();
        let mut seen: HashSet<ObjId> = HashSet::new();
        for _ in 0..MAX_RESOLVE_DEPTH {
            match current {
                Object::Reference(id) => {
                    if !seen.insert(id) {
                        return Err(CosError::ReferenceCycle { id });
                    }
                    let next = match self.get(id) {
                        Ok(o) => o,
                        // ISO 32000-1 §7.3.10: "An indirect reference to an
                        // undefined object shall be considered a reference to
                        // the null object." Erroring here would reject files
                        // that every viewer opens, and would make a full
                        // rewrite -- which legitimately drops unreachable
                        // objects -- produce a file this library refuses.
                        Err(CosError::MissingObject { .. }) => {
                            return Ok(Arc::new(Object::Null));
                        }
                        Err(e) => return Err(e),
                    };
                    match &*next {
                        Object::Reference(_) => current = (*next).clone(),
                        _ => return Ok(next),
                    }
                }
                other => return Ok(Arc::new(other)),
            }
        }
        Err(CosError::malformed(0, "reference chain exceeded the depth limit"))
    }

    /// Look up a dictionary key and resolve it in one step.
    pub fn get_entry(&self, dict: &Dictionary, key: &str) -> Result<Option<Arc<Object>>> {
        match dict.get(key) {
            None => Ok(None),
            Some(v) => Ok(Some(self.resolve(v)?)),
        }
    }

    fn load(&self, id: ObjId) -> Result<Arc<Object>> {
        let entry = self.xref.get(id.number).ok_or(CosError::MissingObject { id })?;
        match entry {
            XrefEntry::Free { .. } => Ok(Arc::new(Object::Null)),
            XrefEntry::InFile { .. } => self.load_raw(id),
            XrefEntry::InObjStm { container, index } => self.load_from_objstm(id, container, index),
        }
    }

    /// Parse a top-level `N G obj` and decrypt its strings.
    fn load_raw(&self, id: ObjId) -> Result<Arc<Object>> {
        let Some(XrefEntry::InFile { offset, generation }) = self.xref.get(id.number) else {
            return Err(CosError::MissingObject { id });
        };
        let offset = self.adjust_offset(offset, id.number);
        if offset >= self.buf.len() {
            return Err(CosError::MissingObject { id });
        }

        // /Length may itself be indirect. Resolving it re-enters the parser on a
        // different object, which is safe: a /Length object is an integer and
        // cannot itself be a stream.
        let resolver =
            FnResolver(|len_id: ObjId| self.load_raw_shallow(len_id).ok().and_then(|o| o.as_i64()));

        let mut parser = Parser::at(&self.buf, offset);
        let parsed = parser.parse_indirect_object(&resolver);

        // A cross-reference entry pointing at the wrong place is one of the
        // commonest real-world corruptions -- a file edited by a tool that did
        // not fix up its offsets, or bytes inserted by a transfer that mangled
        // line endings. Every viewer recovers by finding the object elsewhere,
        // and a reader that gives up here rejects files the whole world opens.
        let io = match parsed {
            Ok(io) if io.id.number == id.number => io,
            other => {
                let Some(repaired) = self.repair_offset(id.number) else {
                    return Err(CosError::MissingObject { id });
                };
                let found = other.map(|io| io.id.number);
                let mut parser = Parser::at(&self.buf, repaired);
                let io = parser.parse_indirect_object(&resolver)?;
                if io.id.number != id.number {
                    return Err(CosError::MissingObject { id });
                }
                self.note(
                    LeniencyKind::XrefOffsetMismatch,
                    offset,
                    match found {
                        Ok(n) => format!(
                            "xref says object {} is at {offset}, but object {n} is; \
                             found {} at {repaired} by scanning",
                            id.number, id.number
                        ),
                        Err(_) => format!(
                            "xref offset {offset} for object {} is unparseable; \
                             found it at {repaired} by scanning",
                            id.number
                        ),
                    },
                );
                io
            }
        };

        let mut object = io.object;
        if io.id.generation != generation {
            // Tolerated: the object is there, the bookkeeping disagrees. The
            // object's own header is the more trustworthy of the two.
            self.note(
                LeniencyKind::GenerationMismatch,
                offset,
                format!(
                    "xref says object {} is generation {generation}, the object says {}",
                    id.number, io.id.generation
                ),
            );
        }

        // Spec 5.5: strings are decrypted at load; stream bodies are decrypted
        // lazily when decoded. The /Encrypt dictionary is exempt.
        if let Some(dec) = &self.decryptor
            && dec.encrypt_ref != Some(id)
            && dec.string_cipher() != Cipher::None
        {
            dec.decrypt_strings_in(id, &mut object)?;
        }

        self.spans.borrow_mut().insert(id, io.span);
        Ok(Arc::new(object))
    }

    /// Parse an object without decryption or caching. Used for `/Length`
    /// resolution and for the `/Encrypt` dictionary during bootstrap.
    fn load_raw_shallow(&self, id: ObjId) -> Result<Object> {
        let Some(XrefEntry::InFile { offset, .. }) = self.xref.get(id.number) else {
            return Err(CosError::MissingObject { id });
        };
        let offset = self.adjust_offset(offset, id.number);
        if offset >= self.buf.len() {
            return Err(CosError::MissingObject { id });
        }
        let mut parser = Parser::at(&self.buf, offset);
        let io = parser.parse_indirect_object(&NoResolve)?;
        if io.id.number != id.number {
            return Err(CosError::MissingObject { id });
        }
        Ok(io.object)
    }

    fn load_from_objstm(&self, id: ObjId, container: u32, index: u32) -> Result<Arc<Object>> {
        let cache = self.objstm_contents(container)?;

        // The index is a hint. What the entry actually means is "object N lives
        // in this container", so a wrong or out-of-range index is repaired by
        // looking the object number up in the container's own header rather
        // than by giving up.
        let by_index = cache.pairs.get(index as usize).copied();
        if let Some((num, rel)) = by_index
            && num == id.number
        {
            return self.parse_objstm_member(&cache, rel);
        }
        let Some(&(_, rel)) = cache.pairs.iter().find(|(n, _)| *n == id.number) else {
            return Err(CosError::MissingObject { id });
        };
        self.note(
            LeniencyKind::ObjStmTruncated,
            0,
            match by_index {
                Some((num, _)) => format!(
                    "object stream {container} entry {index} is object {num}, not {}; \
                     found it by searching the header",
                    id.number
                ),
                None => format!(
                    "object stream {container} has no entry {index}; found object {} \
                     by searching the header",
                    id.number
                ),
            },
        );
        self.parse_objstm_member(&cache, rel)
    }

    fn parse_objstm_member(&self, cache: &ObjStmCache, rel: usize) -> Result<Arc<Object>> {
        let at = cache.first.saturating_add(rel);
        if at >= cache.data.len() {
            return Err(CosError::malformed(at, "object stream offset is past the end"));
        }
        let mut parser = Parser::at(&cache.data, at);
        // Objects inside an object stream carry no strings needing separate
        // decryption: the container was decrypted whole.
        parser.parse_object().map(Arc::new)
    }

    fn objstm_contents(&self, container: u32) -> Result<Arc<ObjStmCache>> {
        if let Some(hit) = self.objstm.borrow().get(&container) {
            return Ok(Arc::clone(hit));
        }
        let id = ObjId::new(container, 0);
        let obj = self.load_raw(id)?;
        let stream =
            obj.as_stream().ok_or(CosError::TypeMismatch { id, expected: "object stream" })?;
        let data = self.decoded_stream(id)?;
        let n = stream.dict.get("N").and_then(Object::as_i64).unwrap_or(0).max(0) as usize;
        let first = stream.dict.get("First").and_then(Object::as_usize).unwrap_or(0);
        let pairs = recovery::objstm_pairs(&data, n, first);

        let cache = Arc::new(ObjStmCache { data, pairs, first });
        self.objstm.borrow_mut().insert(container, Arc::clone(&cache));
        Ok(cache)
    }

    // -----------------------------------------------------------------------
    // Stream contents
    // -----------------------------------------------------------------------

    /// Decrypt (if needed) and run the filter chain, caching the result.
    ///
    /// For a chain ending in an image codec this returns the codec's bytes, not
    /// pixels -- see the module docs in `filters`.
    pub fn decoded_stream(&self, id: ObjId) -> Result<Arc<Vec<u8>>> {
        if let Some(hit) = self.decoded.borrow().get(&id) {
            return Ok(Arc::clone(hit));
        }
        let obj = self.get(id)?;
        let stream = obj.as_stream().ok_or(CosError::TypeMismatch { id, expected: "stream" })?;

        if let Some(pending) = stream.pending_decoded() {
            return Ok(Arc::new(pending.to_vec()));
        }

        let raw = self.decrypted_stream_bytes(id, stream)?;
        let chain = self.filter_chain(&stream.dict)?;
        let data = Arc::new(filters::decode(&chain, &raw)?.data);
        self.decoded.borrow_mut().insert(id, Arc::clone(&data));
        Ok(data)
    }

    /// The stream's stored bytes with encryption removed but filters still
    /// applied.
    pub fn decrypted_stream_bytes(
        &self,
        id: ObjId,
        stream: &crate::object::Stream,
    ) -> Result<Vec<u8>> {
        let Some(dec) = &self.decryptor else {
            return Ok(stream.raw().to_vec());
        };
        if dec.stream_cipher() == Cipher::None || !self.stream_is_encrypted(id, &stream.dict) {
            return Ok(stream.raw().to_vec());
        }
        dec.decrypt_stream(id, stream.raw())
    }

    /// Which streams the security handler skips. ISO 32000-1 §7.6.
    pub(crate) fn stream_is_encrypted(&self, id: ObjId, dict: &Dictionary) -> bool {
        let Some(dec) = &self.decryptor else { return false };
        if dec.encrypt_ref == Some(id) {
            return false;
        }
        match dict.type_name().map(|t| t.as_bytes().to_vec()).as_deref() {
            // Cross-reference streams must be readable before the key exists.
            Some(b"XRef") => false,
            // With /EncryptMetadata false the XMP stream is left in the clear so
            // indexers can read it.
            Some(b"Metadata") if !dec.encrypt_metadata => false,
            _ => {
                // An explicit /Crypt /Identity filter opts a stream out.
                !matches!(dict.get("Filter"), Some(Object::Name(n)) if n.as_bytes() == b"Crypt")
            }
        }
    }

    /// Build the filter chain for a stream, resolving indirect `/Filter` and
    /// `/DecodeParms`.
    pub fn filter_chain(&self, dict: &Dictionary) -> Result<FilterChain> {
        let filter = match dict.get("Filter") {
            Some(f) => Some(self.resolve(f)?),
            None => None,
        };
        let parms = match dict.get("DecodeParms").or_else(|| dict.get("DP")) {
            Some(p) => Some(self.resolve(p)?),
            None => None,
        };
        // Entries inside the arrays may themselves be references.
        let filter = filter.map(|f| self.resolve_shallow_array(&f)).transpose()?;
        let parms = parms.map(|p| self.resolve_shallow_array(&p)).transpose()?;
        Ok(FilterChain::build(filter.as_deref(), parms.as_deref()))
    }

    fn resolve_shallow_array(&self, object: &Object) -> Result<Box<Object>> {
        Ok(Box::new(match object {
            Object::Array(items) => Object::Array(
                items
                    .iter()
                    .map(|i| self.resolve(i).map(|a| (*a).clone()))
                    .collect::<Result<Vec<_>>>()?,
            ),
            other => other.clone(),
        }))
    }

    /// The byte span an object occupied in the source file, if it was loaded
    /// from one. `None` for objects that live in an object stream or were
    /// created after opening.
    pub fn source_span(&self, id: ObjId) -> Option<Range<usize>> {
        if self.spans.borrow().contains_key(&id) {
            return self.spans.borrow().get(&id).cloned();
        }
        // Force a load so the span is recorded.
        if matches!(self.xref.get(id.number), Some(XrefEntry::InFile { .. })) {
            let _ = self.get(id);
        }
        self.spans.borrow().get(&id).cloned()
    }

    /// Offsets are measured from the `%PDF-` header, which is not always at byte
    /// zero.
    ///
    /// The candidate must be checked against the object number we are *looking
    /// for*, not merely against "is there an object here". In a file with an
    /// N-byte preamble, an unshifted offset frequently lands squarely on some
    /// other perfectly valid object, and a plausibility check happily accepts
    /// it and hands back the wrong object.
    fn adjust_offset(&self, offset: usize, expect: u32) -> usize {
        if self.header_offset == 0 {
            return offset;
        }
        if object_number_at(&self.buf, offset) == Some(expect) {
            offset
        } else if object_number_at(&self.buf, offset + self.header_offset) == Some(expect) {
            offset + self.header_offset
        } else {
            offset
        }
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Replace an object. The original bytes stay in `buf`; only the writer
    /// decides what reaches the output.
    pub fn set(&mut self, id: ObjId, object: Object) {
        self.cache.borrow_mut().remove(&id);
        self.decoded.borrow_mut().remove(&id);
        self.objstm.borrow_mut().remove(&id.number);
        self.deleted.remove(&id);
        self.dirty.insert(id, object);
    }

    /// Add a new indirect object, returning its freshly allocated id.
    /// The next object number [`add`](Self::add) or [`reserve`](Self::reserve)
    /// would hand out.
    ///
    /// For an operation that must *plan* new objects while holding the document
    /// immutably — so a caller can inspect the plan before anything is written
    /// — and then have them created by the session along with everything else.
    pub fn next_object_number(&self) -> u32 {
        self.next_number
    }

    /// Claim object numbers without creating anything.
    ///
    /// `add` allocates *and* writes, which is convenient and wrong for an edit
    /// that must stay inside a transaction: the object lands in the dirty set
    /// before the session has recorded that it did not previously exist, so
    /// undo restores it rather than removing it. Reserving separates the two —
    /// the caller gets ids it can reference, and the session creates the
    /// objects along with everything else in the operation.
    ///
    /// Numbers are never reused, so a reservation that is then abandoned costs
    /// a gap in the numbering and nothing else.
    pub fn reserve(&mut self, count: usize) -> Vec<ObjId> {
        (0..count)
            .map(|_| {
                let id = ObjId::new(self.next_number, 0);
                self.next_number += 1;
                id
            })
            .collect()
    }

    pub fn add(&mut self, object: Object) -> ObjId {
        let id = ObjId::new(self.next_number, 0);
        self.next_number += 1;
        self.dirty.insert(id, object);
        id
    }

    /// Mark an object deleted. The writer emits a free entry for it.
    pub fn delete(&mut self, id: ObjId) {
        self.dirty.remove(&id);
        self.cache.borrow_mut().remove(&id);
        self.decoded.borrow_mut().remove(&id);
        self.deleted.insert(id, ());
    }

    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty() || !self.deleted.is_empty()
    }

    /// Record that this document has had content redacted. Spec 10.6.
    ///
    /// > **Forces `SaveMode::FullRewrite`.** Incremental append leaves the
    /// > original bytes in the file, which would make the redaction cosmetic.
    /// > This is non-negotiable and must be enforced in code, not
    /// > documentation.
    ///
    /// So it is enforced here rather than asked of the caller: once set, the
    /// writer ignores any request for an incremental save, exactly as it does
    /// for a document opened in recovery mode. A full rewrite also emits a
    /// single revision with no `/Prev`, which is step 8 of the same list —
    /// prior revisions cannot survive a save that does not reference them.
    ///
    /// There is deliberately no way to unset it. A document that has had
    /// something removed cannot become one that has not, and an API that let a
    /// caller clear the flag would be an API for shipping a cosmetic redaction.
    pub fn mark_redacted(&mut self) {
        self.redacted = true;
    }

    /// Whether [`mark_redacted`](Self::mark_redacted) has been called.
    pub fn is_redacted(&self) -> bool {
        self.redacted
    }

    // -----------------------------------------------------------------------
    // Protection. Spec 5.5, Phase 8; built through `crate::protect`.
    // -----------------------------------------------------------------------

    /// What the next save will do about protection.
    pub fn protection_change(&self) -> &ProtectionChange {
        &self.protection
    }

    /// Install new protection, to take effect on the next save.
    ///
    /// The `/Encrypt` dictionary is given an object number here, and the
    /// handler is told what it is: `/Encrypt` is the one object a security
    /// handler must never encrypt, and it cannot know that about itself until
    /// it has a number. The number comes from [`reserve`](Self::reserve) rather
    /// than [`add`](Self::add) so the dictionary does not enter the dirty set —
    /// it is not part of the document's object graph, and an edit session
    /// undoing its way back past this point should not resurrect it.
    pub(crate) fn set_protection(&mut self, dict: Dictionary, mut decryptor: Decryptor) {
        let id = self.reserve(1)[0];
        decryptor.set_encrypt_ref(id);
        self.protection = ProtectionChange::Replaced { id, dict, decryptor };
    }

    /// Drop protection on the next save.
    pub(crate) fn clear_protection(&mut self) {
        self.protection = ProtectionChange::Removed;
    }

    /// The handler the *writer* should encrypt with, which is not always the
    /// one the document was read with.
    pub(crate) fn output_decryptor(&self) -> Option<&Decryptor> {
        match &self.protection {
            ProtectionChange::Unchanged => self.decryptor.as_ref(),
            ProtectionChange::Removed => None,
            ProtectionChange::Replaced { decryptor, .. } => Some(decryptor),
        }
    }

    /// Set `/ID`, which `/R` 4's key derivation depends on.
    ///
    /// Needed before protecting a document that arrived without one: the writer
    /// synthesises `/ID` from the bytes it has written, which do not exist at
    /// the moment the key is derived. Deriving the key from an identifier the
    /// file will not carry produces a document that rejects its own password.
    pub(crate) fn set_trailer_id(&mut self, id0: &[u8], id1: &[u8]) {
        self.xref.trailer.insert(
            Name::new("ID"),
            Object::Array(vec![
                Object::String(PdfString::new_hex(id0)),
                Object::String(PdfString::new_hex(id1)),
            ]),
        );
    }

    pub(crate) fn dirty_objects(&self) -> &BTreeMap<ObjId, Object> {
        &self.dirty
    }

    pub(crate) fn deleted_objects(&self) -> impl Iterator<Item = ObjId> + '_ {
        self.deleted.keys().copied()
    }

    pub(crate) fn next_number(&self) -> u32 {
        self.next_number
    }

    /// Forget every accumulated change, returning the document to its
    /// as-opened state.
    pub fn discard_changes(&mut self) {
        self.dirty.clear();
        self.deleted.clear();
        self.cache.borrow_mut().clear();
        self.decoded.borrow_mut().clear();
    }

    /// Rough resident size: the file plus what has been parsed from it.
    pub fn memory_usage(&self) -> usize {
        let decoded: usize = self.decoded.borrow().values().map(|v| v.len()).sum();
        let objstm: usize = self.objstm.borrow().values().map(|c| c.data.len()).sum();
        self.buf.len() + decoded + objstm
    }
}

/// The object number of the `N G obj` header at `at`, if there is one.
fn object_number_at(buf: &[u8], at: usize) -> Option<u32> {
    if at >= buf.len() {
        return None;
    }
    // Only the header is read; parsing the whole object would be wasteful and
    // could fail for reasons that have nothing to do with the offset.
    let mut lx = crate::lexer::Lexer::at(buf, at);
    let number = match lx.next_token().token {
        crate::lexer::Token::Integer(v) if (0..=u32::MAX as i64).contains(&v) => v as u32,
        _ => return None,
    };
    match lx.next_token().token {
        crate::lexer::Token::Integer(v) if (0..=u16::MAX as i64).contains(&v) => {}
        _ => return None,
    }
    match lx.next_token().token {
        crate::lexer::Token::Keyword(kw) if &*kw == b"obj" => Some(number),
        _ => None,
    }
}

fn read_version(buf: &[u8], header_offset: usize) -> String {
    let tail = &buf[header_offset..];
    let after = &tail[b"%PDF-".len().min(tail.len())..];
    let n = after.iter().take(8).take_while(|b| b.is_ascii_digit() || **b == b'.').count();
    let v = String::from_utf8_lossy(&after[..n]).into_owned();
    if v.is_empty() { "1.4".to_string() } else { v }
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("version", &self.version)
            .field("bytes", &self.buf.len())
            .field("objects", &self.xref.len())
            .field("encrypted", &self.decryptor.is_some())
            .field("load_mode", &self.load_mode)
            .field("leniencies", &self.leniencies.borrow().len())
            .finish()
    }
}

/// Helper so `Name` is in scope for callers building dictionaries.
pub fn name(s: &str) -> Name {
    Name::new(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    #[test]
    fn a_created_document_is_valid_the_moment_it_exists() {
        let doc = Document::new();
        assert_eq!(doc.load_mode(), LoadMode::Created);
        assert!(doc.bytes().is_empty(), "a created document has no original bytes");

        // The invariant every layer above this one relies on, and the reason
        // `new` builds the catalog itself rather than leaving it to the caller.
        let catalog = doc.catalog().expect("a created document has a catalog");
        let catalog = catalog.as_dict().expect("the catalog is a dictionary");
        assert_eq!(catalog.get("Type").and_then(Object::as_name), Some(&Name::new("Catalog")));

        let pages = doc.resolve(catalog.get("Pages").expect("/Pages")).expect("it resolves");
        let pages = pages.as_dict().expect("the page tree is a dictionary");
        assert_eq!(pages.get("Count").and_then(Object::as_i64), Some(0));
        assert_eq!(pages.get("Kids").and_then(Object::as_array).map(|k| k.len()), Some(0));
    }

    #[test]
    fn a_created_document_saves_and_reopens() {
        let doc = Document::new();
        let saved = crate::writer::save(&doc, &crate::writer::SaveOptions::default()).unwrap();

        // Never incremental: there is nothing to append to.
        assert_eq!(saved.mode, crate::writer::SaveMode::FullRewrite);
        assert!(
            saved.bytes.starts_with(b"%PDF-1.7"),
            "{:?}",
            &saved.bytes[..16.min(saved.bytes.len())]
        );
        assert!(saved.bytes.ends_with(b"%%EOF\n"));

        // The round trip is the claim: bytes this library wrote, parsed by the
        // same reader it uses on everyone else's files, with no leniencies --
        // a created document that only opens by recovery would be a failure
        // dressed as a pass.
        let reopened = Document::open(saved.bytes).expect("what was written can be read");
        assert_eq!(reopened.load_mode(), LoadMode::Xref);
        assert!(reopened.catalog().is_ok());
        assert_eq!(reopened.leniencies(), Vec::new());
    }

    #[test]
    fn opens_a_minimal_classic_file() {
        let doc = Document::open(testutil::minimal_classic()).unwrap();
        assert_eq!(doc.version(), "1.4");
        assert_eq!(doc.load_mode(), LoadMode::Xref);
        assert!(!doc.is_encrypted());
        assert!(doc.catalog().is_ok());
        assert!(doc.leniencies().is_empty(), "{:?}", doc.leniencies());
    }

    #[test]
    fn resolves_through_references() {
        let doc = Document::open(testutil::minimal_classic()).unwrap();
        let catalog = doc.catalog().unwrap();
        let pages_ref = catalog.as_dict().unwrap().get("Pages").unwrap();
        let pages = doc.resolve(pages_ref).unwrap();
        assert_eq!(pages.as_dict().unwrap().type_name().unwrap().as_bytes(), b"Pages");
    }

    #[test]
    fn decodes_a_flate_content_stream() {
        let doc = Document::open(testutil::classic_with_flate_content()).unwrap();
        let content = doc.decoded_stream(ObjId::new(4, 0)).unwrap();
        assert!(String::from_utf8_lossy(&content).contains("Hello"));
        // The cache means a second call does not redo the work.
        assert!(Arc::ptr_eq(&content, &doc.decoded_stream(ObjId::new(4, 0)).unwrap()));
    }

    #[test]
    fn reads_objects_out_of_an_object_stream() {
        let doc = Document::open(testutil::xref_stream_with_objstm()).unwrap();
        assert_eq!(doc.xref_style(), XrefStyle::Stream);
        let catalog = doc.catalog().unwrap();
        assert_eq!(catalog.as_dict().unwrap().type_name().unwrap().as_bytes(), b"Catalog");
        let pages = doc.get(ObjId::new(2, 0)).unwrap();
        assert_eq!(pages.as_dict().unwrap().get("Count").unwrap().as_i64(), Some(1));
    }

    #[test]
    fn falls_back_to_recovery_on_a_broken_startxref() {
        let mut bytes = testutil::minimal_classic();
        let s = String::from_utf8_lossy(&bytes).replace("startxref\n9", "startxref\n999999999\n%");
        bytes = s.into_bytes();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.load_mode(), LoadMode::Reconstructed);
        assert!(doc.catalog().is_ok());
        assert!(
            doc.leniencies().iter().any(|l| l.kind == LeniencyKind::XrefReconstructed),
            "recovery must be reported, never silent"
        );
    }

    #[test]
    fn recovery_can_be_refused() {
        let mut bytes = testutil::minimal_classic();
        let s = String::from_utf8_lossy(&bytes).replace("startxref\n9", "startxref\n999999999\n%");
        bytes = s.into_bytes();
        let opts = OpenOptions { recovery: RecoveryPolicy::Never, ..Default::default() };
        assert!(Document::open_with(bytes, &opts).is_err());
    }

    #[test]
    fn a_reference_to_a_missing_object_is_null_not_an_error() {
        let doc = Document::open(testutil::minimal_classic()).unwrap();
        assert!(doc.get_or_null(ObjId::new(999, 0)).is_null());
    }

    #[test]
    fn detects_a_reference_cycle() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (n, body) in [
            (1u32, "<< /Type /Catalog /Pages 2 0 R >>".to_string()),
            (2, "<< /Type /Pages /Kids [] /Count 0 >>".to_string()),
            (3, "4 0 R".to_string()),
            (4, "3 0 R".to_string()),
        ] {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let xref_at = bytes.len();
        bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        for off in &offsets {
            bytes.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").as_bytes(),
        );

        let doc = Document::open(bytes).unwrap();
        let err = doc.resolve(&Object::Reference(ObjId::new(3, 0))).unwrap_err();
        assert!(matches!(err, CosError::ReferenceCycle { .. }));
    }

    #[test]
    fn tolerates_bytes_before_the_header() {
        let mut bytes = b"GARBAGE PREAMBLE\n".to_vec();
        let shift = bytes.len();
        bytes.extend_from_slice(&testutil::minimal_classic());
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.header_offset(), shift);
        assert!(doc.catalog().is_ok());
        assert!(doc.leniencies().iter().any(|l| l.kind == LeniencyKind::HeaderNotAtStart));
    }

    #[test]
    fn a_shifted_offset_must_match_the_object_it_claims_to_be() {
        // Regression. With an N-byte preamble every recorded offset is short by
        // N, and the unshifted value frequently lands squarely on some *other*
        // valid object. A check that only asks "is there an object here" accepts
        // it and silently returns the wrong object, which is a corruption the
        // caller has no way to detect.
        let preamble = b"HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\n\r\n";
        let mut bytes = preamble.to_vec();
        bytes.extend_from_slice(&testutil::minimal_classic());

        let doc = Document::open(bytes).unwrap();
        for (number, expected_type) in [(1u32, "Catalog"), (2, "Pages"), (3, "Page")] {
            let obj = doc.get(ObjId::new(number, 0)).unwrap();
            assert_eq!(
                obj.as_dict().unwrap().type_name().unwrap().as_bytes(),
                expected_type.as_bytes(),
                "object {number} resolved to the wrong object"
            );
        }
    }

    #[test]
    fn dirty_tracking_survives_reads() {
        let mut doc = Document::open(testutil::minimal_classic()).unwrap();
        assert!(!doc.is_dirty());
        let mut d = Dictionary::new();
        d.insert(Name::new("Type"), Object::name("Page"));
        doc.set(ObjId::new(3, 0), Object::Dictionary(d));
        assert!(doc.is_dirty());
        assert_eq!(
            doc.get(ObjId::new(3, 0)).unwrap().as_dict().unwrap().type_name().unwrap().as_bytes(),
            b"Page"
        );
        doc.discard_changes();
        assert!(!doc.is_dirty());
    }

    #[test]
    fn new_objects_get_fresh_numbers() {
        let mut doc = Document::open(testutil::minimal_classic()).unwrap();
        let a = doc.add(Object::Integer(1));
        let b = doc.add(Object::Integer(2));
        assert_ne!(a, b);
        assert!(doc.xref().get(a.number).is_none(), "must not collide with an existing object");
    }
}
