//! `/Info` and XMP, and where they disagree. Spec 10.3.
//!
//! > Dual-surface: the `/Info` dictionary and the XMP `/Metadata` stream. They
//! > routinely disagree. **Expose both, expose the disagreement**, and write
//! > both on change.
//!
//! Two places to record a title is one place too many, and PDF has had both
//! since XMP arrived in 2001. Producers update one and forget the other; tools
//! rewrite one and leave the other; a file edited by three programs can easily
//! carry three answers.
//!
//! The instinct is to pick a winner — XMP is newer, so prefer it — and that is
//! exactly what the specification tells us not to do. A caller showing a
//! document's title wants the title. A caller auditing a document wants to know
//! that it claims two. Collapsing them serves the first and blinds the second,
//! so [`Metadata::title`] answers the first and [`Metadata::disagreements`]
//! answers the second.
//!
//! # XMP is matched, not parsed
//!
//! Reading XMP properly means an RDF/XML parser, which is a dependency this
//! crate does not want for four fields. What it does instead is find
//! `<dc:title>` and its neighbours by scanning — enough to compare the two
//! surfaces, and honest about being a scan rather than a parse. A caller who
//! needs real XMP has [`Metadata::xmp`], the raw packet.

use rasura_cos::Document;
use rasura_cos::object::Object;

/// A document's descriptive metadata, from both surfaces. Spec 10.3.
#[derive(Debug, Clone, Default)]
pub struct Metadata {
    /// From `/Info`.
    pub info: Fields,
    /// From the XMP packet, where one exists and the field could be found.
    pub xmp_fields: Fields,
    /// The raw XMP packet, for a caller who needs real RDF.
    pub xmp: Option<Vec<u8>>,
}

/// The four fields both surfaces carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fields {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
}

/// One field on which the two surfaces disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disagreement {
    pub field: &'static str,
    pub info: String,
    pub xmp: String,
}

impl Metadata {
    /// The document's title, preferring `/Info`.
    ///
    /// `/Info` rather than XMP, which is the opposite of what "newer is better"
    /// suggests, and is what viewers do: Acrobat's document properties panel
    /// reads `/Info`, so `/Info` is what a user has seen and expects. Being
    /// consistent with the thing the user already looked at beats being
    /// consistent with the more modern standard.
    pub fn title(&self) -> Option<&str> {
        self.info.title.as_deref().or(self.xmp_fields.title.as_deref())
    }

    pub fn author(&self) -> Option<&str> {
        self.info.author.as_deref().or(self.xmp_fields.author.as_deref())
    }

    pub fn producer(&self) -> Option<&str> {
        self.info.producer.as_deref().or(self.xmp_fields.producer.as_deref())
    }

    /// Every field where the two surfaces both have a value and the values
    /// differ. Spec 10.3's "expose the disagreement".
    ///
    /// A field present in one and absent in the other is *not* a disagreement:
    /// that is the normal state of a document whose producer wrote only one
    /// surface, and reporting it would bury the real conflicts in noise.
    pub fn disagreements(&self) -> Vec<Disagreement> {
        let pairs: [(&'static str, &Option<String>, &Option<String>); 5] = [
            ("title", &self.info.title, &self.xmp_fields.title),
            ("author", &self.info.author, &self.xmp_fields.author),
            ("subject", &self.info.subject, &self.xmp_fields.subject),
            ("creator", &self.info.creator, &self.xmp_fields.creator),
            ("producer", &self.info.producer, &self.xmp_fields.producer),
        ];
        pairs
            .into_iter()
            .filter_map(|(field, a, b)| match (a, b) {
                (Some(a), Some(b)) if a != b => {
                    Some(Disagreement { field, info: a.clone(), xmp: b.clone() })
                }
                _ => None,
            })
            .collect()
    }

    pub fn has_xmp(&self) -> bool {
        self.xmp.is_some()
    }
}

/// Read both surfaces.
pub fn read(doc: &Document) -> Metadata {
    Metadata { info: read_info(doc), xmp: read_xmp(doc), xmp_fields: Fields::default() }
        .with_xmp_fields()
}

impl Metadata {
    fn with_xmp_fields(mut self) -> Metadata {
        if let Some(packet) = &self.xmp {
            let text = String::from_utf8_lossy(packet);
            self.xmp_fields = Fields {
                title: xmp_field(&text, "dc:title"),
                author: xmp_field(&text, "dc:creator"),
                subject: xmp_field(&text, "dc:description"),
                // The names cross over, and it is not a mistake. XMP's
                // `xmp:CreatorTool` is the application, which `/Info` calls
                // `Creator`; XMP's `dc:creator` is the *person*, which `/Info`
                // calls `Author`. Mapping them by name rather than by meaning
                // reports every document as disagreeing with itself.
                creator: xmp_field(&text, "xmp:CreatorTool"),
                producer: xmp_field(&text, "pdf:Producer"),
            };
        }
        self
    }
}

fn read_info(doc: &Document) -> Fields {
    let Some(info) = doc.trailer().get("Info") else { return Fields::default() };
    let Ok(object) = doc.resolve(info) else { return Fields::default() };
    let Some(dict) = object.as_dict() else { return Fields::default() };

    let field = |key: &str| {
        dict.get(key)
            .and_then(|v| doc.resolve(v).ok())
            .as_deref()
            .and_then(Object::as_string)
            .map(|s| s.as_text())
            .filter(|s| !s.is_empty())
    };

    Fields {
        title: field("Title"),
        author: field("Author"),
        subject: field("Subject"),
        creator: field("Creator"),
        producer: field("Producer"),
    }
}

fn read_xmp(doc: &Document) -> Option<Vec<u8>> {
    let catalog = doc.catalog().ok()?;
    let id = catalog.as_dict()?.get("Metadata")?.as_reference()?;
    let data = doc.decoded_stream(id).ok()?;
    Some(data.to_vec())
}

/// Find `<tag>…</tag>`, unwrapping the RDF containers XMP wraps text in.
///
/// A scan, not a parse — see the module note. It handles the two shapes that
/// exist in practice: the bare element, and the `rdf:Alt`/`rdf:Seq` list whose
/// first `rdf:li` carries the value.
fn xmp_field(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let start = text.find(&open)?;
    let after_open = text[start..].find('>')? + start + 1;
    let end = text[after_open..].find(&close)? + after_open;
    let inner = &text[after_open..end];

    // `<rdf:Alt><rdf:li xml:lang="x-default">Title</rdf:li></rdf:Alt>`
    let value = match inner.find("<rdf:li") {
        Some(li) => {
            let value_start = inner[li..].find('>')? + li + 1;
            let value_end = inner[value_start..].find("</rdf:li>")? + value_start;
            &inner[value_start..value_end]
        }
        None if inner.contains('<') => return None,
        None => inner,
    };

    let value = value.trim();
    if value.is_empty() { None } else { Some(unescape(value)) }
}

/// The five XML entities. Anything else is left alone rather than guessed at.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        // Last, or an escaped ampersand in an entity would be double-decoded.
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasura_cos::testutil::ClassicBuilder;

    fn doc_with(info: &str, xmp: Option<&str>) -> Document {
        let mut b = ClassicBuilder::new()
            .object(
                1,
                &match xmp {
                    Some(_) => "<< /Type /Catalog /Pages 2 0 R /Metadata 7 0 R >>".to_string(),
                    None => "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
                },
            )
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>")
            .object(6, info);
        if let Some(packet) = xmp {
            b = b.stream(7, " /Type /Metadata /Subtype /XML", packet.as_bytes());
        }
        Document::open(b.finish("/Root 1 0 R /Info 6 0 R")).expect("open")
    }

    const XMP: &str = "<?xpacket begin=\"\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
        <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
        <rdf:Description>\
        <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">The XMP title</rdf:li></rdf:Alt></dc:title>\
        <pdf:Producer>Producer From XMP</pdf:Producer>\
        </rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end=\"w\"?>";

    #[test]
    fn info_fields_are_read() {
        let doc =
            doc_with("<< /Title (A Title) /Author (An Author) /Producer (A Producer) >>", None);
        let meta = read(&doc);
        assert_eq!(meta.info.title.as_deref(), Some("A Title"));
        assert_eq!(meta.info.author.as_deref(), Some("An Author"));
        assert_eq!(meta.title(), Some("A Title"));
        assert!(!meta.has_xmp());
    }

    #[test]
    fn xmp_fields_are_found_inside_their_rdf_containers() {
        let doc = doc_with("<< >>", Some(XMP));
        let meta = read(&doc);
        assert!(meta.has_xmp());
        assert_eq!(meta.xmp_fields.title.as_deref(), Some("The XMP title"));
        assert_eq!(meta.xmp_fields.producer.as_deref(), Some("Producer From XMP"));
        // With no /Info title, XMP answers.
        assert_eq!(meta.title(), Some("The XMP title"));
    }

    #[test]
    fn a_disagreement_between_the_two_surfaces_is_reported() {
        // Spec 10.3's whole point. Collapsing them would serve a caller showing
        // a title and blind a caller auditing a document.
        let doc = doc_with("<< /Title (The Info title) >>", Some(XMP));
        let meta = read(&doc);
        let clashes = meta.disagreements();
        assert_eq!(clashes.len(), 1, "{clashes:?}");
        assert_eq!(clashes[0].field, "title");
        assert_eq!(clashes[0].info, "The Info title");
        assert_eq!(clashes[0].xmp, "The XMP title");
        // ...and the answer a caller gets is the one a viewer shows.
        assert_eq!(meta.title(), Some("The Info title"));
    }

    #[test]
    fn a_field_present_in_only_one_surface_is_not_a_disagreement() {
        // The normal state of a document whose producer wrote one surface.
        // Reporting it would bury the real conflicts in noise.
        let doc = doc_with("<< /Author (Only In Info) >>", Some(XMP));
        let meta = read(&doc);
        assert!(
            meta.disagreements().iter().all(|d| d.field != "author"),
            "{:?}",
            meta.disagreements()
        );
    }

    #[test]
    fn creator_and_author_are_mapped_by_meaning_not_by_name() {
        // XMP's `dc:creator` is the person, which /Info calls Author; XMP's
        // `xmp:CreatorTool` is the application, which /Info calls Creator.
        // Mapping by name reports every document as disagreeing with itself.
        let packet = "<rdf:Description>\
            <dc:creator><rdf:Seq><rdf:li>A Person</rdf:li></rdf:Seq></dc:creator>\
            <xmp:CreatorTool>An Application</xmp:CreatorTool></rdf:Description>";
        let doc = doc_with("<< /Author (A Person) /Creator (An Application) >>", Some(packet));
        let meta = read(&doc);
        assert_eq!(meta.xmp_fields.author.as_deref(), Some("A Person"));
        assert_eq!(meta.xmp_fields.creator.as_deref(), Some("An Application"));
        assert!(meta.disagreements().is_empty(), "{:?}", meta.disagreements());
    }

    #[test]
    fn entities_are_unescaped_and_ampersand_last() {
        // `&amp;lt;` must come back as `&lt;`, not as `<`.
        assert_eq!(unescape("a &amp;lt; b"), "a &lt; b");
        assert_eq!(unescape("Smith &amp; Sons"), "Smith & Sons");
    }

    #[test]
    fn a_document_with_no_metadata_at_all_reads_empty() {
        let doc = doc_with("<< >>", None);
        let meta = read(&doc);
        assert_eq!(meta.info, Fields::default());
        assert!(meta.title().is_none());
        assert!(meta.disagreements().is_empty());
    }
}
