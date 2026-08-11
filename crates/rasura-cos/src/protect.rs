//! Creating protection, and changing the password. Spec 5.5, Phase 8.
//!
//! > Encrypted documents saved incrementally must re-encrypt new strings and
//! > streams with the existing file key. **Changing the password is Phase 8.**
//!
//! [`crate::crypt`] recovers a key from a password someone else chose. This
//! module chooses one. The two directions look symmetric and are not: reading
//! is best-effort against whatever a producer wrote, while writing decides what
//! a document's protection *is*, and a mistake here is not a file that fails to
//! open — it is a file that opens when it should not.
//!
//! # Three refusals, stated up front
//!
//! **RC4 is not offered.** [`crate::crypt`] reads it, because legacy files
//! exist and refusing them helps nobody. Writing it would be creating new
//! protection with a cipher that is broken, and the read path already says so:
//! "a broken cipher we support only to *read* legacy files, never to write new
//! protection." There is no [`Strength`] variant for it and no option to force
//! one.
//!
//! **`/R` 5 is not offered either.** It is Adobe's deprecated extension — a
//! single SHA-256 over the password, with none of `/R` 6's hardening loop.
//! Files using it are read; new ones are not written.
//!
//! **Permissions remain advisory.** [`Policy::permissions`] is written into
//! `/P` and signed into `/Perms`, and this library still does not enforce it on
//! read. A caller who sets "printing not allowed" should understand they have
//! recorded a request, not built a control.
//!
//! # Where the randomness comes from
//!
//! Nowhere in this crate: it has no RNG, and `wasm32-unknown-unknown` provides
//! none by default. That is not an accident to be patched over — the object
//! layer needing no filesystem, no clock and no randomness is what lets it run
//! unchanged in a Worker, and pulling in `getrandom` to generate a salt would
//! spend that property on four bytes.
//!
//! So the caller supplies 32 bytes through [`Entropy`], and everything else is
//! expanded from them with a counter-mode KDF. The security of the result is
//! bounded by the quality of those 32 bytes, which is stated plainly rather
//! than hidden behind an API that appears to generate its own.
//!
//! # A protection change forces a full rewrite
//!
//! For the same reason redaction does, and it is worth spelling out because the
//! failure is different. An incremental save appends: the objects in prior
//! revisions stay exactly as they were, encrypted under the *old* key or under
//! no key at all. A reader uses one file key for the whole document, so half of
//! it would be undecryptable. Adding protection incrementally does not produce
//! a weakly-protected file; it produces a broken one.
//!
//! [`Document::save`](crate::Document) cannot be reached from here, so the rule
//! lives in [`crate::writer::effective_mode`], which checks
//! [`Document::protection_change`] before the caller's requested mode.

use crate::crypt::{
    Cipher, Decryptor, NewHandler, Permissions, aes_cbc_no_iv_encrypt, algorithm_2, compute_o,
    compute_u_r3, hash_2b,
};
use crate::document::Document;
use crate::error::{CosError, Result};
use crate::object::{Dictionary, Name, Object, PdfString};
use sha2::{Digest, Sha256};

/// Which algorithm the new protection uses.
///
/// Two, not a menu. Every other combination ISO 32000 permits is either broken
/// (RC4), deprecated (`/R` 5), or a way of writing one of these two badly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strength {
    /// AES-256, `/V` 5 `/R` 6. ISO 32000-2's algorithm, with the
    /// SHA-256/384/512 hardening loop over the password.
    ///
    /// The default, and what should be used unless a specific old reader has to
    /// open the file. Acrobat 9 and later, and every current viewer.
    #[default]
    Aes256,

    /// AES-128, `/V` 4 `/R` 4. Acrobat 7 and later.
    ///
    /// The cipher is sound; the *password hashing* is not — `/R` 4 derives the
    /// key with one MD5 and fifty more over 16 bytes, which a modern machine
    /// runs at an enormous rate. Offered for compatibility with readers that
    /// predate AES-256, and reported as [`Weakness::LegacyKeyDerivation`] so a
    /// caller cannot choose it without being told.
    Aes128,
}

/// Something true about the protection that the caller should know.
///
/// Returned rather than logged: a caller who has just protected a document is
/// about to tell a user it is protected, and this is what qualifies that
/// sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weakness {
    /// `/R` 4's key derivation is one MD5 plus fifty over a 16-byte digest.
    LegacyKeyDerivation,
    /// The user password is empty, so the document opens without one. The
    /// protection is real — the bytes are encrypted — but it restricts nobody.
    EmptyUserPassword,
    /// The user and owner passwords are the same, so `/P` cannot be enforced
    /// even by a reader that wanted to: anyone who can open the document holds
    /// the owner password.
    OwnerPasswordEqualsUser,
}

/// 32 bytes of caller-supplied randomness.
///
/// A newtype rather than a `[u8; 32]` parameter so that the one thing this
/// module cannot check for itself is at least impossible to pass by accident:
/// an array of zeros is a compile-time-valid argument and a catastrophic one.
#[derive(Clone)]
pub struct Entropy([u8; 32]);

impl std::fmt::Debug for Entropy {
    /// Deliberately opaque. Entropy that ends up in a log is not entropy.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Entropy(<32 bytes>)")
    }
}

impl Entropy {
    /// Wrap 32 random bytes.
    ///
    /// Rejects input that is obviously not random — all bytes equal, or a
    /// counting sequence. This catches the two mistakes that actually happen
    /// (a zeroed buffer, and `0..32` written while testing) and cannot catch a
    /// caller who passes a poor-quality RNG's output. It is a guard against
    /// accident, not against a determined mistake.
    pub fn new(bytes: [u8; 32]) -> Result<Self> {
        if bytes.iter().all(|&b| b == bytes[0]) {
            return Err(CosError::Internal("entropy of 32 identical bytes is not entropy".into()));
        }
        let counting = bytes.windows(2).all(|w| w[1] == w[0].wrapping_add(1));
        if counting {
            return Err(CosError::Internal("entropy that counts upward is not entropy".into()));
        }
        Ok(Entropy(bytes))
    }

    /// Derive `n` bytes for one purpose.
    ///
    /// Counter mode over SHA-256 with a per-purpose label, so the file key and
    /// the validation salts are independent: reusing one buffer by slicing it
    /// would make a leak of any part a leak of the rest.
    fn expand(&self, label: &[u8], n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n + 32);
        let mut counter = 0u32;
        while out.len() < n {
            let mut h = Sha256::new();
            h.update(self.0);
            h.update(label);
            h.update(counter.to_be_bytes());
            out.extend_from_slice(&h.finalize());
            counter += 1;
        }
        out.truncate(n);
        out
    }
}

/// What the new protection should say.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Opens the document. Empty means it opens without a password, which is
    /// legal, common, and reported as [`Weakness::EmptyUserPassword`].
    pub user_password: String,
    /// Lifts the advisory `/P` restrictions. Should differ from the user
    /// password or it grants nothing.
    ///
    /// **Empty means "the same as the user password", not "no owner
    /// password".** A reader checks the two entries independently and is in if
    /// either is satisfied, so an empty owner password would open a document
    /// that has a user password — one that asks for a password and opens
    /// without one. The substitution is reported as
    /// [`Weakness::OwnerPasswordEqualsUser`].
    ///
    /// Leaving *both* empty is the ordinary "encrypted but opens for anyone"
    /// case, which is legal and common; it is reported too.
    pub owner_password: String,
    /// `/P`. Advisory — see the module note.
    pub permissions: Permissions,
    /// `/EncryptMetadata`. False leaves the XMP packet readable without the
    /// password, which is what indexers want and what a caller redacting
    /// metadata does not.
    pub encrypt_metadata: bool,
    pub strength: Strength,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            user_password: String::new(),
            owner_password: String::new(),
            permissions: Permissions::all(),
            encrypt_metadata: true,
            strength: Strength::default(),
        }
    }
}

/// What protecting a document did.
#[derive(Debug, Clone)]
pub struct Protection {
    pub strength: Strength,
    /// Everything the caller should be told before saying "this is protected".
    pub weaknesses: Vec<Weakness>,
    /// Whether an existing protection was replaced rather than added.
    pub replaced_existing: bool,
}

/// Protect a document, or change the password of one already protected.
///
/// The same function for both because they are the same operation: every object
/// is re-encrypted under a key derived from the new passwords, and the old key
/// ceases to open anything. "Change the password" is not a cheaper path — a
/// document whose `/O` and `/U` were rewritten while its objects kept the old
/// key would still be readable by anyone holding the old password, which is the
/// one thing a password change is for.
///
/// The document must be open — that is, already decrypted, which
/// [`Document::open`](crate::Document::open) does with the password it was
/// given. Protecting a document whose password was never supplied is not
/// possible and not attempted.
///
/// Nothing is written here. The change takes effect on the next save, which is
/// forced to [`SaveMode::FullRewrite`](crate::SaveMode::FullRewrite).
pub fn protect(doc: &mut Document, policy: &Policy, entropy: &Entropy) -> Result<Protection> {
    let replaced_existing = doc.is_encrypted();

    // An empty owner password opens the document, exactly as an empty user
    // password does — the two entries are checked independently, and a reader
    // that finds either satisfied is in. So setting a user password and leaving
    // the owner password blank produces a file that asks for a password and
    // opens without one, which is the worst possible outcome: it looks
    // protected to the person who made it.
    //
    // Acrobat resolves this by making the owner password default to the user
    // password, and so does this. The substitution is real and is reported, not
    // assumed to be understood.
    let owner_is_borrowed = policy.owner_password.is_empty() && !policy.user_password.is_empty();
    let owner_password =
        if owner_is_borrowed { &policy.user_password } else { &policy.owner_password };

    let mut weaknesses = Vec::new();
    if policy.strength == Strength::Aes128 {
        weaknesses.push(Weakness::LegacyKeyDerivation);
    }
    if policy.user_password.is_empty() {
        weaknesses.push(Weakness::EmptyUserPassword);
    }
    if &policy.user_password == owner_password {
        weaknesses.push(Weakness::OwnerPasswordEqualsUser);
    }

    // `/ID[0]` is an input to `/R` 4's key derivation, so it has to be settled
    // *before* anything is encrypted and preserved verbatim by the writer. A
    // document with no `/ID` gets one here rather than at save time, where the
    // writer derives it from the bytes written so far -- a value that does not
    // exist yet when the key is needed.
    let id0 = match existing_id0(doc) {
        Some(id0) => id0,
        None => {
            let fresh = entropy.expand(b"rasura/id0", 16);
            doc.set_trailer_id(&fresh, &fresh);
            fresh
        }
    };

    let p = policy.permissions.to_bits();
    let (dict, decryptor) = match policy.strength {
        Strength::Aes256 => build_aes256(policy, owner_password, p, entropy)?,
        Strength::Aes128 => build_aes128(policy, owner_password, p, &id0, entropy)?,
    };

    doc.set_protection(dict, decryptor);
    Ok(Protection { strength: policy.strength, weaknesses, replaced_existing })
}

/// Remove protection, leaving a document anyone can open.
///
/// A real operation with a real use — a caller who holds the password and wants
/// an unprotected copy — and one worth naming rather than expressing as
/// "protect with no cipher", which would be a way of writing a file that claims
/// protection and has none.
///
/// Like [`protect`], this forces a full rewrite: the streams on disk are
/// ciphertext, and an incremental append that merely dropped `/Encrypt` would
/// produce a file whose every page is garbage.
pub fn unprotect(doc: &mut Document) -> Result<()> {
    if !doc.is_encrypted() {
        return Err(CosError::Internal("the document is not protected".into()));
    }
    doc.clear_protection();
    Ok(())
}

fn existing_id0(doc: &Document) -> Option<Vec<u8>> {
    doc.trailer()
        .get("ID")
        .and_then(Object::as_array)
        .and_then(|a| a.first())
        .and_then(Object::as_string)
        .map(|s| s.as_bytes().to_vec())
        .filter(|v| !v.is_empty())
}

/// ISO 32000-2 Algorithms 8, 9 and 10: `/V` 5 `/R` 6.
fn build_aes256(
    policy: &Policy,
    owner_password: &str,
    p: i32,
    entropy: &Entropy,
) -> Result<(Dictionary, Decryptor)> {
    const R: i64 = 6;

    // The file key is random, not derived: at `/R` 6 the passwords unlock a
    // wrapped copy of it rather than producing it. That is what makes changing
    // the password cheap in principle -- and it is still not done cheaply here,
    // for the reason given on `protect`.
    let file_key = entropy.expand(b"rasura/aes256/filekey", 32);
    let salts = entropy.expand(b"rasura/aes256/salts", 32);
    let (u_validation, u_key) = (&salts[0..8], &salts[8..16]);
    let (o_validation, o_key) = (&salts[16..24], &salts[24..32]);

    let user = truncate_password(&policy.user_password);
    let owner = truncate_password(owner_password);

    // Algorithm 8: /U is hash || validation salt || key salt, and /UE wraps the
    // file key under a hash of the *other* salt. Two salts rather than one so
    // that checking a password does not hand over the key-wrapping key.
    let mut u = hash_2b(&user, u_validation, &[], R);
    u.extend_from_slice(u_validation);
    u.extend_from_slice(u_key);
    let ue = aes_cbc_no_iv_encrypt(&hash_2b(&user, u_key, &[], R), &file_key)?;

    // Algorithm 9: the same shape, with the whole of /U mixed in, which is what
    // binds the owner entry to this document's user entry.
    let mut o = hash_2b(&owner, o_validation, &u[..48], R);
    o.extend_from_slice(o_validation);
    o.extend_from_slice(o_key);
    let oe = aes_cbc_no_iv_encrypt(&hash_2b(&owner, o_key, &u[..48], R), &file_key)?;

    // Algorithm 10: /Perms is the permission bits encrypted under the file key,
    // so a reader can tell whether they were tampered with after the fact. It
    // is a tamper *check*, not enforcement -- see the module note.
    let mut perms = Vec::with_capacity(16);
    perms.extend_from_slice(&(p as u32).to_le_bytes());
    perms.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
    perms.push(if policy.encrypt_metadata { b'T' } else { b'F' });
    perms.extend_from_slice(b"adb");
    perms.extend_from_slice(&entropy.expand(b"rasura/aes256/perms", 4));
    let perms = aes_cbc_no_iv_encrypt(&file_key, &perms)?;

    let mut dict = base_dictionary(5, R, 256, p, policy.encrypt_metadata);
    dict.insert(Name::new("O"), Object::String(PdfString::new_hex(&o)));
    dict.insert(Name::new("U"), Object::String(PdfString::new_hex(&u)));
    dict.insert(Name::new("OE"), Object::String(PdfString::new_hex(&oe)));
    dict.insert(Name::new("UE"), Object::String(PdfString::new_hex(&ue)));
    dict.insert(Name::new("Perms"), Object::String(PdfString::new_hex(&perms)));
    insert_crypt_filter(&mut dict, "AESV3", 32);

    let decryptor = Decryptor::for_new_protection(NewHandler {
        file_key,
        cipher: Cipher::Aes256,
        revision: R,
        permissions: p,
        encrypt_metadata: policy.encrypt_metadata,
        iv_seed: entropy.expand(b"rasura/iv", 16),
    });
    Ok((dict, decryptor))
}

/// ISO 32000-1 Algorithms 2, 3 and 5: `/V` 4 `/R` 4, AES-128.
fn build_aes128(
    policy: &Policy,
    owner_password: &str,
    p: i32,
    id0: &[u8],
    entropy: &Entropy,
) -> Result<(Dictionary, Decryptor)> {
    const R: i64 = 4;
    const KEY_LEN: usize = 16;

    let user = policy.user_password.as_bytes();
    let owner = owner_password.as_bytes();

    // Note the order, which is not optional: `/O` is an input to the file key,
    // and the file key is an input to `/U`. Computing them in any other order
    // produces a document that rejects its own password.
    let o = compute_o(owner, user, R, KEY_LEN);
    let file_key = algorithm_2(user, &o, p, id0, R, KEY_LEN, policy.encrypt_metadata);
    let u = compute_u_r3(&file_key, id0);

    let mut dict = base_dictionary(4, R, 128, p, policy.encrypt_metadata);
    dict.insert(Name::new("O"), Object::String(PdfString::new_hex(&o)));
    dict.insert(Name::new("U"), Object::String(PdfString::new_hex(&u)));
    insert_crypt_filter(&mut dict, "AESV2", 16);

    let decryptor = Decryptor::for_new_protection(NewHandler {
        file_key,
        cipher: Cipher::Aes128,
        revision: R,
        permissions: p,
        encrypt_metadata: policy.encrypt_metadata,
        iv_seed: entropy.expand(b"rasura/iv", 16),
    });
    Ok((dict, decryptor))
}

fn base_dictionary(v: i64, r: i64, length_bits: i64, p: i32, encrypt_metadata: bool) -> Dictionary {
    let mut d = Dictionary::new();
    d.insert(Name::new("Filter"), Object::name("Standard"));
    d.insert(Name::new("V"), Object::Integer(v));
    d.insert(Name::new("R"), Object::Integer(r));
    d.insert(Name::new("Length"), Object::Integer(length_bits));
    d.insert(Name::new("P"), Object::Integer(p as i64));
    if !encrypt_metadata {
        d.insert(Name::new("EncryptMetadata"), Object::Bool(false));
    }
    d
}

fn insert_crypt_filter(dict: &mut Dictionary, cfm: &str, key_bytes: i64) {
    let mut stdcf = Dictionary::new();
    stdcf.insert(Name::new("CFM"), Object::name(cfm));
    stdcf.insert(Name::new("AuthEvent"), Object::name("DocOpen"));
    stdcf.insert(Name::new("Length"), Object::Integer(key_bytes));

    let mut cf = Dictionary::new();
    cf.insert(Name::new("StdCF"), Object::Dictionary(stdcf));

    dict.insert(Name::new("CF"), Object::Dictionary(cf));
    dict.insert(Name::new("StmF"), Object::name("StdCF"));
    dict.insert(Name::new("StrF"), Object::name("StdCF"));
}

/// ISO 32000-2 §7.6.4.3.3: a `/R` 6 password is used as at most 127 bytes.
///
/// The specification also calls for SASLprep. It is not applied: every password
/// that exists in practice is ASCII, where SASLprep is the identity, and a
/// wrong normalisation would produce a document that rejects the password it
/// was given. Truncation *is* applied because it changes the bytes hashed and
/// omitting it would silently disagree with every other implementation.
fn truncate_password(password: &str) -> Vec<u8> {
    let bytes = password.as_bytes();
    // Never split a character in half; a truncated UTF-8 sequence is a
    // different string to everyone who reads it.
    let mut end = bytes.len().min(127);
    while end > 0 && !password.is_char_boundary(end) {
        end -= 1;
    }
    bytes[..end].to_vec()
}

/// Rebuild `/P` from decoded flags.
///
/// An extension trait rather than an inherent method because it belongs to
/// *writing* protection, and [`crate::crypt`] is the reading side: a reader
/// never needs to turn flags back into bits, and putting it there would suggest
/// the round trip is lossless. It is not — the reserved bits are normalised, so
/// `from_bits(0).to_bits()` is not zero.
pub trait PermissionBits {
    /// The `/P` value these flags describe. ISO 32000-1 Table 22.
    fn to_bits(&self) -> i32;
}

impl PermissionBits for Permissions {
    fn to_bits(&self) -> i32 {
        // Bits 1-2 are reserved and shall be 0. Bits 7-8 and 13-32 are reserved
        // and shall be 1, which is why the base is all-ones with the low two
        // cleared rather than zero with bits set.
        let mut raw: u32 = 0xffff_fffc;
        for (bit, allowed) in [
            (3, self.print),
            (4, self.modify),
            (5, self.copy),
            (6, self.annotate),
            (9, self.fill_forms),
            (10, self.extract_for_accessibility),
            (11, self.assemble),
            (12, self.print_high_quality),
        ] {
            if !allowed {
                raw &= !(1u32 << (bit - 1));
            }
        }
        raw as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::ClassicBuilder;
    use crate::{OpenOptions, SaveMode, SaveOptions};

    fn entropy() -> Entropy {
        // Fixed so the tests are reproducible; the bytes are arbitrary and are
        // deliberately neither constant nor counting, which `Entropy::new`
        // rejects.
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        Entropy::new(bytes).expect("entropy")
    }

    fn plain_document() -> Vec<u8> {
        ClassicBuilder::new()
            .object(1, "<< /Type /Catalog /Pages 2 0 R >>")
            .object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
            .object(3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>")
            .stream(4, "", b"BT /F1 12 Tf 1 0 0 1 72 700 Tm (secret memo) Tj ET\n")
            .object(5, "<< /Title (The secret memo) >>")
            .finish("/Root 1 0 R /Info 5 0 R")
    }

    fn protect_and_save(policy: &Policy) -> (Vec<u8>, Protection) {
        let mut doc = Document::open(plain_document()).expect("open");
        let report = protect(&mut doc, policy, &entropy()).expect("protect");
        let saved = crate::save(&doc, &SaveOptions::default()).expect("save");
        (saved.bytes, report)
    }

    #[test]
    fn a_protected_document_opens_with_its_password_and_not_without() {
        let policy = Policy { user_password: "hunter2".into(), ..Policy::default() };
        let (bytes, _) = protect_and_save(&policy);

        let opts = OpenOptions { password: "hunter2".into(), ..OpenOptions::default() };
        let reopened = Document::open_with(bytes.clone(), &opts).expect("opens with the password");
        assert!(reopened.is_encrypted());

        let err = Document::open(bytes).expect_err("must not open without one");
        assert!(matches!(err, CosError::PasswordRequired), "{err:?}");
    }

    #[test]
    fn the_owner_password_also_opens_it() {
        let policy = Policy {
            user_password: "user-pw".into(),
            owner_password: "owner-pw".into(),
            ..Policy::default()
        };
        let (bytes, _) = protect_and_save(&policy);

        let opts = OpenOptions { password: "owner-pw".into(), ..OpenOptions::default() };
        let doc = Document::open_with(bytes, &opts).expect("owner opens it");
        assert_eq!(doc.permissions().raw, Permissions::all().to_bits());
    }

    #[test]
    fn the_content_is_actually_encrypted() {
        // The claim is about the bytes, not about the dictionary: a file that
        // says /Encrypt and carries plaintext is the failure this whole module
        // exists to avoid, and it passes every check that only reads the
        // dictionary back.
        let policy = Policy { user_password: "pw".into(), ..Policy::default() };
        let (bytes, _) = protect_and_save(&policy);

        let window = bytes.windows(11);
        assert!(
            !window.clone().any(|w| w == b"secret memo"),
            "the page content survives in plaintext"
        );
        assert!(
            !bytes.windows(15).any(|w| w == b"The secret memo"),
            "the /Info title survives in plaintext"
        );

        // ...and it comes back when the password is supplied.
        let opts = OpenOptions { password: "pw".into(), ..OpenOptions::default() };
        let doc = Document::open_with(bytes, &opts).expect("open");
        let content = doc.decoded_stream(crate::ObjId::new(4, 0)).expect("content");
        assert!(String::from_utf8_lossy(&content).contains("secret memo"));
    }

    #[test]
    fn both_strengths_round_trip() {
        for strength in [Strength::Aes256, Strength::Aes128] {
            let policy = Policy { user_password: "pw".into(), strength, ..Policy::default() };
            let (bytes, report) = protect_and_save(&policy);
            assert_eq!(report.strength, strength);

            let opts = OpenOptions { password: "pw".into(), ..OpenOptions::default() };
            let doc =
                Document::open_with(bytes, &opts).unwrap_or_else(|e| panic!("{strength:?}: {e}"));
            let content = doc.decoded_stream(crate::ObjId::new(4, 0)).expect("content");
            assert!(String::from_utf8_lossy(&content).contains("secret memo"), "{strength:?}");
        }
    }

    #[test]
    fn aes128_is_reported_as_the_weaker_choice() {
        let policy = Policy {
            user_password: "pw".into(),
            owner_password: "other".into(),
            strength: Strength::Aes128,
            ..Policy::default()
        };
        let (_, report) = protect_and_save(&policy);
        assert!(report.weaknesses.contains(&Weakness::LegacyKeyDerivation), "{report:?}");

        let policy = Policy { strength: Strength::Aes256, ..policy };
        let (_, report) = protect_and_save(&policy);
        assert!(!report.weaknesses.contains(&Weakness::LegacyKeyDerivation), "{report:?}");
    }

    #[test]
    fn an_empty_owner_password_does_not_leave_the_door_open() {
        // The bug this pins: `/O` and `/U` are checked independently, so
        // leaving the owner password blank while setting a user password
        // produces a document that prompts for a password and then opens
        // without one. It looks protected to whoever made it, which is the
        // worst way for this to be wrong.
        for strength in [Strength::Aes256, Strength::Aes128] {
            let policy = Policy {
                user_password: "hunter2".into(),
                owner_password: String::new(),
                strength,
                ..Policy::default()
            };
            let (bytes, report) = protect_and_save(&policy);
            assert!(
                Document::open(bytes.clone()).is_err(),
                "{strength:?}: opened with no password at all"
            );
            // And the caller is told the owner password is not a second secret.
            assert!(report.weaknesses.contains(&Weakness::OwnerPasswordEqualsUser), "{report:?}");

            let opts = OpenOptions { password: "hunter2".into(), ..OpenOptions::default() };
            Document::open_with(bytes, &opts).expect("the user password still opens it");
        }
    }

    #[test]
    fn an_empty_or_shared_password_is_reported() {
        let (_, report) = protect_and_save(&Policy::default());
        assert!(report.weaknesses.contains(&Weakness::EmptyUserPassword), "{report:?}");
        assert!(report.weaknesses.contains(&Weakness::OwnerPasswordEqualsUser), "{report:?}");
    }

    #[test]
    fn changing_the_password_stops_the_old_one_working() {
        // The property that makes this a password *change* rather than a
        // rewrite of two dictionary entries: the old password must stop
        // opening the file, which is only true if every object was
        // re-encrypted under the new key.
        let first = Policy { user_password: "first".into(), ..Policy::default() };
        let (bytes, _) = protect_and_save(&first);

        let opts = OpenOptions { password: "first".into(), ..OpenOptions::default() };
        let mut doc = Document::open_with(bytes, &opts).expect("open with the first password");
        let second = Policy { user_password: "second".into(), ..Policy::default() };
        let report = protect(&mut doc, &second, &entropy()).expect("re-protect");
        assert!(report.replaced_existing);
        let bytes = crate::save(&doc, &SaveOptions::default()).expect("save").bytes;

        let opts = OpenOptions { password: "second".into(), ..OpenOptions::default() };
        Document::open_with(bytes.clone(), &opts).expect("the new password opens it");

        let opts = OpenOptions { password: "first".into(), ..OpenOptions::default() };
        let err = Document::open_with(bytes, &opts).expect_err("the old password must not");
        assert!(matches!(err, CosError::PasswordRequired), "{err:?}");
    }

    #[test]
    fn unprotecting_leaves_a_readable_document() {
        let policy = Policy { user_password: "pw".into(), ..Policy::default() };
        let (bytes, _) = protect_and_save(&policy);

        let opts = OpenOptions { password: "pw".into(), ..OpenOptions::default() };
        let mut doc = Document::open_with(bytes, &opts).expect("open");
        unprotect(&mut doc).expect("unprotect");
        let bytes = crate::save(&doc, &SaveOptions::default()).expect("save").bytes;

        let doc = Document::open(bytes).expect("opens with no password at all");
        assert!(!doc.is_encrypted());
        let content = doc.decoded_stream(crate::ObjId::new(4, 0)).expect("content");
        assert!(String::from_utf8_lossy(&content).contains("secret memo"));
    }

    #[test]
    fn protection_forces_a_full_rewrite_even_when_incremental_is_asked_for() {
        // An incremental append would leave every prior object under the old
        // key -- or under none -- and a reader has only one file key. This is
        // not weaker protection, it is a broken file.
        let mut doc = Document::open(plain_document()).expect("open");
        protect(&mut doc, &Policy::default(), &entropy()).expect("protect");

        let opts = SaveOptions { mode: Some(SaveMode::Incremental), ..SaveOptions::default() };
        let saved = crate::save(&doc, &opts).expect("save");
        assert_eq!(saved.mode, SaveMode::FullRewrite);
    }

    #[test]
    fn entropy_rejects_what_is_obviously_not_random() {
        assert!(Entropy::new([0u8; 32]).is_err(), "all zeros");
        assert!(Entropy::new([7u8; 32]).is_err(), "all the same");
        assert!(Entropy::new(std::array::from_fn(|i| i as u8)).is_err(), "counting");
        assert!(Entropy::new(std::array::from_fn(|i| (i as u8).wrapping_mul(37))).is_ok());
    }

    #[test]
    fn the_permission_bits_round_trip_through_the_reserved_ones() {
        let mut p = Permissions::all();
        p.print = false;
        p.copy = false;
        let bits = p.to_bits();
        let back = Permissions::from_bits(bits);
        assert!(!back.print && !back.copy);
        assert!(back.modify && back.annotate && back.assemble);
        // Bits 7-8 and 13-32 are reserved as 1, so a document with everything
        // denied is still not zero.
        let none = Permissions::from_bits(0);
        assert_ne!(none.to_bits(), 0);
    }

    #[test]
    fn a_long_password_is_truncated_on_a_character_boundary() {
        // 127 bytes is the /R 6 limit, and cutting a multi-byte character in
        // half would produce a different string to every other implementation.
        let long = "é".repeat(100);
        let cut = truncate_password(&long);
        assert!(cut.len() <= 127);
        assert!(std::str::from_utf8(&cut).is_ok(), "the truncation split a character");
    }

    #[test]
    fn unprotecting_something_unprotected_is_an_error() {
        let mut doc = Document::open(plain_document()).expect("open");
        assert!(unprotect(&mut doc).is_err());
    }
}
