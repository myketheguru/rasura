//! The standard security handler. ISO 32000-1 §7.6, ISO 32000-2 §7.6.4.7 for
//! `/R` 6. Spec 5.5.
//!
//! Supported: `/V` 1, 2, 4, 5 and `/R` 2, 3, 4, 5, 6; RC4 40- and 128-bit;
//! AES-128 (`AESV2`); AES-256 (`AESV3`) with the SHA-256/384/512 hardening loop;
//! crypt filters; the empty user password attempted automatically; the owner
//! password path; `/EncryptMetadata false`.
//!
//! # Permissions are advisory
//!
//! `/P` is reported through `Permissions` and **not enforced**. Whether to
//! honour a bit that says "printing not allowed" is the consuming application's
//! legal and product decision, not the parser's. Enforcing it here would also be
//! trivially bypassable, which makes enforcement theatre rather than security.
//!
//! # A note on RC4
//!
//! RC4 is implemented inline rather than pulled from a crate. PDF needs keys of
//! every length from 5 to 16 bytes, and the RustCrypto `rc4` crate types the key
//! length as a compile-time parameter, which turns "decrypt with an n-byte key"
//! into a twelve-arm dispatch. The algorithm is twenty lines with no
//! constant-time or side-channel requirements that matter here -- this is a
//! broken cipher we support only to *read* legacy files, never to write new
//! protection.

use crate::error::{CosError, Result};
use crate::object::{Dictionary, ObjId, Object};
use aes::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::NoPadding};
use md5::Md5;
use sha2::{Digest, Sha256, Sha384, Sha512};

type Aes128CbcEnc = cbc::Encryptor<aes::Aes128>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

/// ISO 32000-1 Table 20: the 32-byte padding string.
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// The cipher a crypt filter selects. ISO 32000-1 Table 25.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cipher {
    /// `/Identity`: no encryption.
    None,
    /// `/V2`: RC4 with a per-object key.
    Rc4,
    /// `/AESV2`: AES-128-CBC with a per-object key.
    Aes128,
    /// `/AESV3`: AES-256-CBC with the file key used directly.
    Aes256,
}

/// `/P`, decoded. ISO 32000-1 Table 22. Reported, never enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    pub raw: i32,
    pub print: bool,
    pub modify: bool,
    pub copy: bool,
    pub annotate: bool,
    pub fill_forms: bool,
    pub extract_for_accessibility: bool,
    pub assemble: bool,
    pub print_high_quality: bool,
}

impl Permissions {
    pub fn from_bits(raw: i32) -> Self {
        // Bit positions in the table are 1-based.
        let bit = |n: u32| raw & (1 << (n - 1)) != 0;
        Permissions {
            raw,
            print: bit(3),
            modify: bit(4),
            copy: bit(5),
            annotate: bit(6),
            fill_forms: bit(9),
            extract_for_accessibility: bit(10),
            assemble: bit(11),
            print_high_quality: bit(12),
        }
    }

    /// What an unencrypted document reports: everything allowed.
    pub fn all() -> Self {
        Permissions::from_bits(-1)
    }
}

/// Which password satisfied the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordKind {
    /// The user password (very often the empty one).
    User,
    /// The owner password, which also lifts the advisory `/P` restrictions.
    Owner,
}

/// The inputs to a handler for protection this library is creating.
///
/// A struct rather than eight parameters because the call site is
/// [`crate::protect`], where the two ciphers, the revision and the key length
/// all have to agree with each other and with the `/Encrypt` dictionary being
/// written alongside — a positional argument list is exactly the wrong shape
/// for a set of values that must be consistent.
pub(crate) struct NewHandler {
    pub file_key: Vec<u8>,
    pub cipher: Cipher,
    pub revision: i64,
    pub permissions: i32,
    pub encrypt_metadata: bool,
    pub iv_seed: Vec<u8>,
}

/// A prepared standard security handler.
#[derive(Debug, Clone)]
pub struct Decryptor {
    file_key: Vec<u8>,
    stream_cipher: Cipher,
    string_cipher: Cipher,
    /// `/R`. Above 4 the file key is used directly with no per-object salt.
    revision: i64,
    pub permissions: Permissions,
    pub password_kind: PasswordKind,
    pub encrypt_metadata: bool,
    /// The object number of the `/Encrypt` dictionary, which is never itself
    /// encrypted.
    pub encrypt_ref: Option<ObjId>,
    /// Extra input to the CBC initialisation vector, set only when *this*
    /// library created the protection. See [`Decryptor::for_new_protection`].
    iv_seed: Vec<u8>,
}

impl Decryptor {
    /// Build from the trailer's `/Encrypt` dictionary and `/ID`.
    ///
    /// `password` is tried as both user and owner password. The empty password
    /// is attempted automatically -- it is the overwhelmingly common case, and
    /// requiring callers to pass `Some("")` would be a papercut on every
    /// encrypted file in existence.
    pub fn new(
        encrypt: &Dictionary,
        id0: &[u8],
        password: &str,
        encrypt_ref: Option<ObjId>,
    ) -> Result<Self> {
        let filter = encrypt.get("Filter").and_then(Object::as_name);
        if let Some(f) = filter
            && f.as_bytes() != b"Standard"
        {
            return Err(CosError::UnsupportedEncryption(format!(
                "/Filter /{} is not the standard security handler",
                String::from_utf8_lossy(f.as_bytes())
            )));
        }

        let v = encrypt.get("V").and_then(Object::as_i64).unwrap_or(0);
        let r = encrypt.get("R").and_then(Object::as_i64).unwrap_or(0);
        if !matches!(v, 1 | 2 | 4 | 5) {
            return Err(CosError::UnsupportedEncryption(format!("/V {v}")));
        }
        if !(2..=6).contains(&r) {
            return Err(CosError::UnsupportedEncryption(format!("/R {r}")));
        }

        let length_bits = encrypt.get("Length").and_then(Object::as_i64).unwrap_or(40);
        let o = string_bytes(encrypt.get("O"));
        let u = string_bytes(encrypt.get("U"));
        let p = encrypt.get("P").and_then(Object::as_i64).unwrap_or(-1) as i32;
        let encrypt_metadata =
            encrypt.get("EncryptMetadata").and_then(Object::as_bool).unwrap_or(true);

        let (stream_cipher, string_cipher, key_len) = crypt_filters(encrypt, v, length_bits)?;

        let (file_key, password_kind) = if r >= 5 {
            derive_key_r5_r6(encrypt, &o, &u, password, r)?
        } else {
            derive_key_r2_r4(&o, &u, p, id0, password, r, key_len, encrypt_metadata)?
        };

        Ok(Decryptor {
            file_key,
            stream_cipher,
            string_cipher,
            revision: r,
            permissions: Permissions::from_bits(p),
            password_kind,
            encrypt_metadata,
            encrypt_ref,
            // Reading someone else's file: the IV of every existing ciphertext
            // is already in the file, and the only bytes this handler will
            // write are re-encryptions under the same key it just read.
            iv_seed: Vec::new(),
        })
    }

    /// A handler for protection this library is *creating*. Spec 5.5, Phase 8.
    ///
    /// Separate from [`Decryptor::new`] because the inputs are different in
    /// kind: `new` recovers a key from a password and a stored `/O`/`/U`, while
    /// this is handed the key it will use. Sharing one constructor would mean a
    /// function that sometimes derives and sometimes trusts, and the difference
    /// between those is the whole security property.
    ///
    /// `iv_seed` is mixed into the CBC initialisation vector. The IV is derived
    /// from key and plaintext rather than drawn from an RNG — this crate has
    /// none, and `wasm32-unknown-unknown` provides none — which is unique per
    /// distinct content but *predictable*, and at `/R` 6 the file key is shared
    /// by every object, so two objects with identical plaintext would otherwise
    /// encrypt identically. Seeding from the caller's entropy removes that
    /// across documents. It does not make the IV unpredictable to someone
    /// holding the password, which is a property CBC wants and this does not
    /// provide; it is an improvement on the alternative, not a claim of more.
    pub(crate) fn for_new_protection(new: NewHandler) -> Self {
        Decryptor {
            file_key: new.file_key,
            // One cipher for both. `/StmF` and `/StrF` may legally differ, and
            // reading honours that because files in the wild do it; there is no
            // reason to *write* a document whose strings and streams are
            // protected differently.
            stream_cipher: new.cipher,
            string_cipher: new.cipher,
            revision: new.revision,
            permissions: Permissions::from_bits(new.permissions),
            // The creator holds both passwords, so it is the owner by
            // construction. Nothing downstream enforces this; it is reported.
            password_kind: PasswordKind::Owner,
            encrypt_metadata: new.encrypt_metadata,
            // Filled in by `set_encrypt_ref` once the document allocates a
            // number for the dictionary.
            encrypt_ref: None,
            iv_seed: new.iv_seed,
        }
    }

    /// Where the `/Encrypt` dictionary lives, once it has been written.
    ///
    /// Set after construction because the object number is allocated by the
    /// document, which cannot happen until the dictionary the handler produced
    /// exists — a circularity the type system would otherwise force into an
    /// `Option` field the caller has to remember to fill.
    pub(crate) fn set_encrypt_ref(&mut self, id: ObjId) {
        self.encrypt_ref = Some(id);
    }

    pub fn stream_cipher(&self) -> Cipher {
        self.stream_cipher
    }

    pub fn string_cipher(&self) -> Cipher {
        self.string_cipher
    }

    /// ISO 32000-1 Algorithm 1: mix the object number and generation into the
    /// file key. Not done at `/R` 5 and above, where the file key is used
    /// directly.
    fn object_key(&self, id: ObjId, cipher: Cipher) -> Vec<u8> {
        if self.revision >= 5 || cipher == Cipher::Aes256 {
            return self.file_key.clone();
        }
        let mut h = Md5::new();
        h.update(&self.file_key);
        h.update(&id.number.to_le_bytes()[..3]);
        h.update(&id.generation.to_le_bytes()[..2]);
        if cipher == Cipher::Aes128 {
            h.update(b"sAlT");
        }
        let digest = h.finalize();
        let n = (self.file_key.len() + 5).min(16);
        digest[..n].to_vec()
    }

    pub fn decrypt_stream(&self, id: ObjId, data: &[u8]) -> Result<Vec<u8>> {
        self.transform(id, data, self.stream_cipher, false)
    }

    pub fn encrypt_stream(&self, id: ObjId, data: &[u8]) -> Result<Vec<u8>> {
        self.transform(id, data, self.stream_cipher, true)
    }

    pub fn decrypt_string(&self, id: ObjId, data: &[u8]) -> Result<Vec<u8>> {
        self.transform(id, data, self.string_cipher, false)
    }

    pub fn encrypt_string(&self, id: ObjId, data: &[u8]) -> Result<Vec<u8>> {
        self.transform(id, data, self.string_cipher, true)
    }

    fn transform(
        &self,
        id: ObjId,
        data: &[u8],
        cipher: Cipher,
        encrypting: bool,
    ) -> Result<Vec<u8>> {
        match cipher {
            Cipher::None => Ok(data.to_vec()),
            Cipher::Rc4 => {
                // RC4 is its own inverse.
                Ok(rc4(&self.object_key(id, cipher), data))
            }
            Cipher::Aes128 | Cipher::Aes256 => {
                let key = self.object_key(id, cipher);
                if encrypting {
                    aes_cbc_encrypt(&key, data, &self.iv_seed)
                } else {
                    aes_cbc_decrypt(&key, data)
                }
            }
        }
    }

    /// Decrypt every string inside an object, in place.
    ///
    /// Streams are handled separately because their raw bytes are decrypted
    /// lazily. Strings inside an object stream are *not* passed here: the
    /// container was already decrypted as a whole, and decrypting them again
    /// would corrupt them.
    pub fn decrypt_strings_in(&self, id: ObjId, object: &mut Object) -> Result<()> {
        match object {
            Object::String(s) => {
                let plain = self.decrypt_string(id, s.as_bytes())?;
                s.replace_decoded(plain);
            }
            Object::Array(items) => {
                for item in items {
                    self.decrypt_strings_in(id, item)?;
                }
            }
            Object::Dictionary(d) => {
                for (_, v) in d.iter_mut() {
                    self.decrypt_strings_in(id, v)?;
                }
            }
            Object::Stream(s) => {
                for (_, v) in s.dict.iter_mut() {
                    self.decrypt_strings_in(id, v)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Read `/CF`, `/StmF` and `/StrF`. For `/V` below 4 there are no crypt filters
/// and everything is RC4.
fn crypt_filters(
    encrypt: &Dictionary,
    v: i64,
    length_bits: i64,
) -> Result<(Cipher, Cipher, usize)> {
    if v < 4 {
        let key_len = if v == 1 { 5 } else { (length_bits / 8).clamp(5, 16) as usize };
        return Ok((Cipher::Rc4, Cipher::Rc4, key_len));
    }

    let cf = encrypt.get("CF").and_then(Object::as_dict);
    let lookup = |which: &str, default_identity: bool| -> Result<(Cipher, usize)> {
        let name = encrypt.get(which).and_then(Object::as_name);
        let Some(name) = name.cloned() else {
            // ISO 32000-1 Table 20: /StmF and /StrF both default to /Identity.
            let _ = default_identity;
            return Ok((Cipher::None, 0));
        };
        if name.as_bytes() == b"Identity" {
            return Ok((Cipher::None, 0));
        }
        let Some(entry) = cf.and_then(|d| d.get_name(&name)).and_then(Object::as_dict) else {
            // A dangling /StmF or /StrF is a producer bug, not a reason to
            // refuse the file. ISO 32000-1 Table 20 makes AESV2 the only
            // sensible reading at /V 4 and AESV3 at /V 5, and that is what
            // every viewer assumes. Guessing wrong yields garbage the caller
            // can see; refusing yields nothing at all.
            let cipher = if v >= 5 { Cipher::Aes256 } else { Cipher::Aes128 };
            let key_len = if v >= 5 { 32 } else { 16 };
            return Ok((cipher, key_len));
        };
        let cfm = entry.get("CFM").and_then(Object::as_name);
        let bits = entry
            .get("Length")
            .and_then(Object::as_i64)
            // Some producers write /Length in bytes here rather than bits.
            .map(|l| if l <= 64 { l * 8 } else { l })
            .unwrap_or(length_bits);
        let cipher = match cfm.map(|n| n.as_bytes().to_vec()).as_deref() {
            Some(b"V2") => Cipher::Rc4,
            Some(b"AESV2") => Cipher::Aes128,
            Some(b"AESV3") => Cipher::Aes256,
            Some(b"None") | None => Cipher::None,
            Some(other) => {
                return Err(CosError::UnsupportedEncryption(format!(
                    "/CFM /{}",
                    String::from_utf8_lossy(other)
                )));
            }
        };
        let key_len = match cipher {
            Cipher::Aes256 => 32,
            Cipher::Aes128 => 16,
            _ => (bits / 8).clamp(5, 16) as usize,
        };
        Ok((cipher, key_len))
    };

    let (stream_cipher, stream_len) = lookup("StmF", true)?;
    let (string_cipher, string_len) = lookup("StrF", true)?;
    let key_len = stream_len.max(string_len).max(5);
    Ok((stream_cipher, string_cipher, key_len))
}

/// ISO 32000-1 Algorithms 2, 4, 5, 6 and 7.
#[allow(clippy::too_many_arguments)]
fn derive_key_r2_r4(
    o: &[u8],
    u: &[u8],
    p: i32,
    id0: &[u8],
    password: &str,
    r: i64,
    key_len: usize,
    encrypt_metadata: bool,
) -> Result<(Vec<u8>, PasswordKind)> {
    // Try the supplied password as a user password, then the empty one, then
    // the supplied password as an owner password.
    for candidate in dedup([password.as_bytes(), b""]) {
        let key = algorithm_2(candidate, o, p, id0, r, key_len, encrypt_metadata);
        if user_password_matches(&key, u, id0, r) {
            return Ok((key, PasswordKind::User));
        }
    }
    for candidate in dedup([password.as_bytes(), b""]) {
        if let Some(user_pw) = recover_user_password_from_owner(candidate, o, r, key_len) {
            let key = algorithm_2(&user_pw, o, p, id0, r, key_len, encrypt_metadata);
            if user_password_matches(&key, u, id0, r) {
                return Ok((key, PasswordKind::Owner));
            }
        }
    }
    Err(CosError::PasswordRequired)
}

fn dedup<const N: usize>(items: [&[u8]; N]) -> Vec<&[u8]> {
    let mut out: Vec<&[u8]> = Vec::with_capacity(N);
    for it in items {
        if !out.contains(&it) {
            out.push(it);
        }
    }
    out
}

pub(crate) fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let n = password.len().min(32);
    out[..n].copy_from_slice(&password[..n]);
    out[n..].copy_from_slice(&PAD[..32 - n]);
    out
}

/// Algorithm 2: compute the file encryption key.
pub(crate) fn algorithm_2(
    password: &[u8],
    o: &[u8],
    p: i32,
    id0: &[u8],
    r: i64,
    key_len: usize,
    encrypt_metadata: bool,
) -> Vec<u8> {
    let mut h = Md5::new();
    h.update(pad_password(password));
    // /O is always 32 bytes; short values are a damaged file, so use what there
    // is rather than refusing.
    h.update(&o[..o.len().min(32)]);
    h.update(p.to_le_bytes());
    h.update(id0);
    if r >= 4 && !encrypt_metadata {
        h.update([0xff, 0xff, 0xff, 0xff]);
    }
    let mut digest = h.finalize().to_vec();

    let n = if r == 2 { 5 } else { key_len.clamp(5, 16) };
    if r >= 3 {
        // Algorithm 2 step (h): 50 further MD5 passes over the first n bytes.
        for _ in 0..50 {
            let mut h = Md5::new();
            h.update(&digest[..n]);
            digest = h.finalize().to_vec();
        }
    }
    digest.truncate(n);
    digest
}

/// Algorithms 4 (R2) and 5 (R3+): does this key produce the stored `/U`?
fn user_password_matches(key: &[u8], u: &[u8], id0: &[u8], r: i64) -> bool {
    if r == 2 {
        let expected = rc4(key, &PAD);
        return u.len() >= 32 && expected[..] == u[..32];
    }
    let mut h = Md5::new();
    h.update(PAD);
    h.update(id0);
    let mut block = rc4(key, &h.finalize());
    for i in 1..=19u8 {
        let salted: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        block = rc4(&salted, &block);
    }
    // Only the first 16 bytes are meaningful; the rest is arbitrary padding.
    u.len() >= 16 && block[..16] == u[..16]
}

/// Algorithm 7 run backwards: decrypt `/O` to recover the user password.
fn recover_user_password_from_owner(
    owner_password: &[u8],
    o: &[u8],
    r: i64,
    key_len: usize,
) -> Option<Vec<u8>> {
    if o.len() < 32 {
        return None;
    }
    let mut digest = {
        let mut h = Md5::new();
        h.update(pad_password(owner_password));
        h.finalize().to_vec()
    };
    if r >= 3 {
        for _ in 0..50 {
            let mut h = Md5::new();
            h.update(&digest);
            digest = h.finalize().to_vec();
        }
    }
    let n = if r == 2 { 5 } else { key_len.clamp(5, 16) };
    let rc4_key = &digest[..n];

    let mut data = o[..32].to_vec();
    if r == 2 {
        data = rc4(rc4_key, &data);
    } else {
        for i in (0..=19u8).rev() {
            let salted: Vec<u8> = rc4_key.iter().map(|b| b ^ i).collect();
            data = rc4(&salted, &data);
        }
    }
    Some(data)
}

/// ISO 32000-2 Algorithms 2.A/2.B/8-10: AES-256, `/R` 5 and 6.
fn derive_key_r5_r6(
    encrypt: &Dictionary,
    o: &[u8],
    u: &[u8],
    password: &str,
    r: i64,
) -> Result<(Vec<u8>, PasswordKind)> {
    if u.len() < 48 {
        return Err(CosError::UnsupportedEncryption("/U is shorter than 48 bytes".into()));
    }
    let ue = string_bytes(encrypt.get("UE"));
    let oe = string_bytes(encrypt.get("OE"));
    let u48 = &u[..48];

    for candidate in dedup([password.as_bytes(), b""]) {
        // Passwords are SASLprep'd in the spec; the bytes are used as given
        // here, which is right for the ASCII passwords that exist in practice.
        let pw = &candidate[..candidate.len().min(127)];

        // User password: hash against the validation salt, U[32..40].
        let hash = hash_2b(pw, &u[32..40], &[], r);
        if hash == u[..32] {
            let intermediate = hash_2b(pw, &u[40..48], &[], r);
            let key = aes_cbc_no_iv_decrypt(&intermediate, &ue)?;
            return Ok((key, PasswordKind::User));
        }

        // Owner password: the same, but with U[0..48] mixed in.
        if o.len() >= 48 {
            let hash = hash_2b(pw, &o[32..40], u48, r);
            if hash == o[..32] {
                let intermediate = hash_2b(pw, &o[40..48], u48, r);
                let key = aes_cbc_no_iv_decrypt(&intermediate, &oe)?;
                return Ok((key, PasswordKind::Owner));
            }
        }
    }
    Err(CosError::PasswordRequired)
}

/// ISO 32000-2 Algorithm 2.B. At `/R` 5 (Adobe's deprecated extension) this is
/// a single SHA-256; at `/R` 6 it is the hardening loop.
pub(crate) fn hash_2b(password: &[u8], salt: &[u8], udata: &[u8], r: i64) -> Vec<u8> {
    let mut k = {
        let mut h = Sha256::new();
        h.update(password);
        h.update(salt);
        h.update(udata);
        h.finalize().to_vec()
    };
    if r == 5 {
        return k;
    }

    let mut round = 0usize;
    loop {
        // K1 = (password || K || udata) repeated 64 times.
        let unit_len = password.len() + k.len() + udata.len();
        let mut k1 = Vec::with_capacity(unit_len * 64);
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }

        // E = AES-128-CBC-NoPadding(key = K[0..16], iv = K[16..32], K1).
        let mut buf = k1;
        let len = buf.len();
        let e = match Aes128CbcEnc::new_from_slices(&k[..16], &k[16..32]) {
            Ok(enc) => match enc.encrypt_padded_mut::<NoPadding>(&mut buf, len) {
                Ok(ct) => ct.to_vec(),
                Err(_) => return k,
            },
            Err(_) => return k,
        };

        let modulo = e[..16].iter().map(|&b| b as u32).sum::<u32>() % 3;
        k = match modulo {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };

        let last_e_byte = *e.last().unwrap_or(&0);
        round += 1;
        if round >= 64 && (last_e_byte as usize) <= round - 32 {
            break;
        }
        // Defensive bound: the loop provably terminates, but an adversarial
        // file should not be able to spin the parser regardless.
        if round > 4096 {
            break;
        }
    }
    k.truncate(32);
    k
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

/// RC4. Symmetric: the same call encrypts and decrypts.
fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    if key.is_empty() {
        return data.to_vec();
    }
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let mut out = Vec::with_capacity(data.len());
    let (mut i, mut j) = (0u8, 0u8);
    for &b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        out.push(b ^ k);
    }
    out
}

/// AES-CBC where the IV is the first 16 bytes of the data, per ISO 32000-1
/// §7.6.2. Padding is PKCS#5, stripped manually so a file with bad padding
/// yields the plaintext it has rather than an error.
fn aes_cbc_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 16 {
        // Too short to carry an IV. An empty stream is legitimate.
        return Ok(Vec::new());
    }
    let (iv, body) = data.split_at(16);
    if body.is_empty() {
        return Ok(Vec::new());
    }
    // A body that is not a whole number of blocks is damaged; decrypt the
    // blocks that are there.
    let usable = body.len() - body.len() % 16;
    let mut buf = body[..usable].to_vec();

    let plain: Vec<u8> = match key.len() {
        16 => {
            let dec = Aes128CbcDec::new_from_slices(key, iv)
                .map_err(|_| CosError::UnsupportedEncryption("bad AES-128 key length".into()))?;
            dec.decrypt_padded_mut::<NoPadding>(&mut buf)
                .map_err(|_| CosError::UnsupportedEncryption("AES-128 decrypt failed".into()))?
                .to_vec()
        }
        32 => {
            let dec = Aes256CbcDec::new_from_slices(key, iv)
                .map_err(|_| CosError::UnsupportedEncryption("bad AES-256 key length".into()))?;
            dec.decrypt_padded_mut::<NoPadding>(&mut buf)
                .map_err(|_| CosError::UnsupportedEncryption("AES-256 decrypt failed".into()))?
                .to_vec()
        }
        other => {
            return Err(CosError::UnsupportedEncryption(format!(
                "AES key length {other} is neither 16 nor 32 bytes"
            )));
        }
    };
    Ok(strip_pkcs5(plain))
}

fn strip_pkcs5(mut plain: Vec<u8>) -> Vec<u8> {
    let Some(&pad) = plain.last() else { return plain };
    let n = pad as usize;
    if (1..=16).contains(&n)
        && n <= plain.len()
        && plain[plain.len() - n..].iter().all(|&b| b == pad)
    {
        plain.truncate(plain.len() - n);
    }
    plain
}

fn aes_cbc_encrypt(key: &[u8], data: &[u8], iv_seed: &[u8]) -> Result<Vec<u8>> {
    // A deterministic IV would leak equality between identically-encrypted
    // objects. There is no RNG in this crate and none in a `wasm32-unknown`
    // target by default, so the IV is derived from the plaintext and key: it is
    // unique per distinct content, which is what CBC actually requires.
    //
    // `iv_seed` is empty when re-encrypting a file this library only read, and
    // is the caller's entropy when it created the protection -- see
    // `Decryptor::for_new_protection` for what that does and does not buy.
    let mut h = Sha256::new();
    h.update(key);
    h.update(iv_seed);
    h.update(data);
    let iv: Vec<u8> = h.finalize()[..16].to_vec();

    let pad = 16 - (data.len() % 16);
    let mut buf = Vec::with_capacity(data.len() + pad + 16);
    buf.extend_from_slice(data);
    buf.extend(std::iter::repeat_n(pad as u8, pad));
    let len = buf.len();

    let ct = match key.len() {
        16 => Aes128CbcEnc::new_from_slices(key, &iv)
            .map_err(|_| CosError::UnsupportedEncryption("bad AES-128 key length".into()))?
            .encrypt_padded_mut::<NoPadding>(&mut buf, len)
            .map_err(|_| CosError::UnsupportedEncryption("AES-128 encrypt failed".into()))?
            .to_vec(),
        32 => Aes256CbcEnc::new_from_slices(key, &iv)
            .map_err(|_| CosError::UnsupportedEncryption("bad AES-256 key length".into()))?
            .encrypt_padded_mut::<NoPadding>(&mut buf, len)
            .map_err(|_| CosError::UnsupportedEncryption("AES-256 encrypt failed".into()))?
            .to_vec(),
        other => {
            return Err(CosError::UnsupportedEncryption(format!(
                "AES key length {other} is neither 16 nor 32 bytes"
            )));
        }
    };

    let mut out = iv;
    out.extend_from_slice(&ct);
    Ok(out)
}

/// AES-256-CBC with a zero IV and no padding: the inverse of
/// [`aes_cbc_no_iv_decrypt`], used to wrap the file key into `/UE` and `/OE`
/// and to encrypt `/Perms`.
///
/// ISO 32000-2 calls the `/Perms` step "AES-256 in ECB mode". For a single
/// 16-byte block with a zero IV, CBC *is* ECB — the XOR is against zero — so
/// this is that algorithm rather than an approximation of it, and using one
/// helper for both keeps a second AES mode out of the crate.
pub(crate) fn aes_cbc_no_iv_encrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() % 16 != 0 {
        return Err(CosError::Internal(
            "the unpadded AES path needs a whole number of blocks".into(),
        ));
    }
    let iv = [0u8; 16];
    let mut buf = data.to_vec();
    let len = buf.len();
    let ct = match key.len() {
        16 => Aes128CbcEnc::new_from_slices(key, &iv)
            .map_err(|_| CosError::UnsupportedEncryption("bad AES-128 key length".into()))?
            .encrypt_padded_mut::<NoPadding>(&mut buf, len)
            .map_err(|_| CosError::UnsupportedEncryption("AES-128 encrypt failed".into()))?
            .to_vec(),
        32 => Aes256CbcEnc::new_from_slices(key, &iv)
            .map_err(|_| CosError::UnsupportedEncryption("bad AES-256 key length".into()))?
            .encrypt_padded_mut::<NoPadding>(&mut buf, len)
            .map_err(|_| CosError::UnsupportedEncryption("AES-256 encrypt failed".into()))?
            .to_vec(),
        other => {
            return Err(CosError::UnsupportedEncryption(format!(
                "AES key length {other} is neither 16 nor 32 bytes"
            )));
        }
    };
    Ok(ct)
}

/// AES-256-CBC with a zero IV and no padding, used to unwrap `/UE` and `/OE`.
fn aes_cbc_no_iv_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 32 {
        return Err(CosError::UnsupportedEncryption("/UE or /OE is too short".into()));
    }
    let iv = [0u8; 16];
    let mut buf = data[..32].to_vec();
    let dec = Aes256CbcDec::new_from_slices(key, &iv)
        .map_err(|_| CosError::UnsupportedEncryption("bad AES-256 key length".into()))?;
    let plain = dec
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|_| CosError::UnsupportedEncryption("failed to unwrap the file key".into()))?;
    Ok(plain.to_vec())
}

fn string_bytes(o: Option<&Object>) -> Vec<u8> {
    match o {
        Some(Object::String(s)) => s.as_bytes().to_vec(),
        _ => Vec::new(),
    }
}

/// ISO 32000-1 Algorithm 5: compute `/U` for `/R` 3 and 4.
pub(crate) fn compute_u_r3(key: &[u8], id0: &[u8]) -> Vec<u8> {
    let mut h = Md5::new();
    h.update(PAD);
    h.update(id0);
    let mut block = rc4(key, &h.finalize());
    for i in 1..=19u8 {
        let salted: Vec<u8> = key.iter().map(|b| b ^ i).collect();
        block = rc4(&salted, &block);
    }
    let mut out = block;
    out.extend_from_slice(&PAD[..16]);
    out
}

/// ISO 32000-1 Algorithm 3: compute `/O` from the owner and user passwords.
pub(crate) fn compute_o(
    owner_password: &[u8],
    user_password: &[u8],
    r: i64,
    key_len: usize,
) -> Vec<u8> {
    let mut digest = {
        let mut h = Md5::new();
        h.update(pad_password(owner_password));
        h.finalize().to_vec()
    };
    if r >= 3 {
        for _ in 0..50 {
            let mut h = Md5::new();
            h.update(&digest);
            digest = h.finalize().to_vec();
        }
    }
    let n = if r == 2 { 5 } else { key_len.clamp(5, 16) };
    let rc4_key = &digest[..n];

    let mut data = pad_password(user_password).to_vec();
    if r == 2 {
        data = rc4(rc4_key, &data);
    } else {
        for i in 0..=19u8 {
            let salted: Vec<u8> = rc4_key.iter().map(|b| b ^ i).collect();
            data = rc4(&salted, &data);
        }
    }
    data
}

/// Building self-consistent `/Encrypt` dictionaries, for fixtures.
///
/// This is deliberately **not** the Phase 8 "encryption creation" feature from
/// spec 3: there is no password change, no key rotation, no AES-256, and no
/// public API for protecting a document. It exists so that the decryption path
/// can be tested against files this crate knows the ground truth for, because
/// crypto that is only tested against its own inverse is not tested at all.
pub mod fixture {
    use super::*;
    use crate::object::{Name, PdfString};

    /// `/V 2 /R 3`, 128-bit RC4, empty user and owner passwords.
    pub fn rc4_128(id0: &[u8], p: i32) -> (Dictionary, Decryptor) {
        build(id0, p, 2, 3, 16, None)
    }

    /// `/V 4 /R 4`, AES-128 (`/AESV2`), empty user and owner passwords.
    pub fn aes_128(id0: &[u8], p: i32) -> (Dictionary, Decryptor) {
        build(id0, p, 4, 4, 16, Some("AESV2"))
    }

    fn build(
        id0: &[u8],
        p: i32,
        v: i64,
        r: i64,
        key_len: usize,
        cfm: Option<&str>,
    ) -> (Dictionary, Decryptor) {
        let o = compute_o(b"", b"", r, key_len);
        let key = algorithm_2(b"", &o, p, id0, r, key_len, true);
        let u = compute_u_r3(&key, id0);

        let mut d = Dictionary::new();
        d.insert(Name::new("Filter"), Object::name("Standard"));
        d.insert(Name::new("V"), Object::Integer(v));
        d.insert(Name::new("R"), Object::Integer(r));
        d.insert(Name::new("Length"), Object::Integer(key_len as i64 * 8));
        d.insert(Name::new("P"), Object::Integer(p as i64));
        d.insert(Name::new("O"), Object::String(PdfString::new_hex(&o)));
        d.insert(Name::new("U"), Object::String(PdfString::new_hex(&u)));

        if let Some(cfm) = cfm {
            let mut stdcf = Dictionary::new();
            stdcf.insert(Name::new("CFM"), Object::name(cfm));
            stdcf.insert(Name::new("AuthEvent"), Object::name("DocOpen"));
            stdcf.insert(Name::new("Length"), Object::Integer(key_len as i64));
            let mut cf = Dictionary::new();
            cf.insert(Name::new("StdCF"), Object::Dictionary(stdcf));
            d.insert(Name::new("CF"), Object::Dictionary(cf));
            d.insert(Name::new("StmF"), Object::name("StdCF"));
            d.insert(Name::new("StrF"), Object::name("StdCF"));
        }

        let decryptor = Decryptor::new(&d, id0, "", None)
            .expect("a fixture dictionary this crate built must open");
        (d, decryptor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Name, PdfString};

    #[test]
    fn rc4_matches_the_published_test_vectors() {
        // RFC 6229 / the original Applied Cryptography vectors.
        assert_eq!(
            rc4(b"Key", b"Plaintext"),
            vec![0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]
        );
        assert_eq!(rc4(b"Wiki", b"pedia"), vec![0x10, 0x21, 0xBF, 0x04, 0x20]);
        assert_eq!(
            rc4(b"Secret", b"Attack at dawn"),
            vec![
                0x45, 0xA0, 0x1F, 0x64, 0x5F, 0xC3, 0x5B, 0x38, 0x35, 0x52, 0x54, 0x4B, 0x9B, 0xF5
            ]
        );
    }

    #[test]
    fn rc4_is_its_own_inverse() {
        let data = b"content stream bytes".repeat(10);
        assert_eq!(rc4(b"key12", &rc4(b"key12", &data)), data);
    }

    #[test]
    fn permissions_decode_the_documented_bits() {
        // All bits set except printing (bit 3).
        let p = Permissions::from_bits(!(1 << 2));
        assert!(!p.print);
        assert!(p.modify);
        assert!(p.copy);
        assert!(Permissions::all().print);
    }

    fn encrypt_dict_r3(id0: &[u8], p: i32) -> (Dictionary, Vec<u8>) {
        // Build a self-consistent /R 3 128-bit RC4 dictionary with an empty
        // user password, the way a real producer would.
        let o = vec![0x5au8; 32];
        let key = algorithm_2(b"", &o, p, id0, 3, 16, true);
        let u = compute_u_r3(&key, id0);

        let mut d = Dictionary::new();
        d.insert(Name::new("Filter"), Object::name("Standard"));
        d.insert(Name::new("V"), Object::Integer(2));
        d.insert(Name::new("R"), Object::Integer(3));
        d.insert(Name::new("Length"), Object::Integer(128));
        d.insert(Name::new("P"), Object::Integer(p as i64));
        d.insert(Name::new("O"), Object::String(PdfString::new_hex(&o)));
        d.insert(Name::new("U"), Object::String(PdfString::new_hex(&u)));
        (d, key)
    }

    #[test]
    fn opens_an_r3_document_with_the_empty_user_password() {
        let id0 = b"0123456789abcdef";
        // Everything permitted except printing (bit 3).
        let p = !(1 << 2);
        let (dict, key) = encrypt_dict_r3(id0, p);
        let dec = Decryptor::new(&dict, id0, "", None).unwrap();
        assert_eq!(dec.file_key, key);
        assert_eq!(dec.password_kind, PasswordKind::User);
        assert_eq!(dec.stream_cipher(), Cipher::Rc4);
        assert!(
            !dec.permissions.print,
            "/P is reported faithfully, even though it is not enforced"
        );
        assert!(dec.permissions.copy);
    }

    #[test]
    fn rejects_a_wrong_password_rather_than_producing_garbage() {
        let id0 = b"0123456789abcdef";
        let mut d = encrypt_dict_r3(id0, -1).0;
        // Corrupt /U so nothing can validate.
        d.insert(Name::new("U"), Object::String(PdfString::new_hex([0u8; 32])));
        let err = Decryptor::new(&d, id0, "hunter2", None).unwrap_err();
        assert!(matches!(err, CosError::PasswordRequired));
    }

    #[test]
    fn rc4_object_keys_differ_per_object() {
        let id0 = b"0123456789abcdef";
        let (dict, _) = encrypt_dict_r3(id0, -1);
        let dec = Decryptor::new(&dict, id0, "", None).unwrap();
        let a = dec.object_key(ObjId::new(1, 0), Cipher::Rc4);
        let b = dec.object_key(ObjId::new(2, 0), Cipher::Rc4);
        assert_ne!(a, b, "Algorithm 1 must salt with the object number");
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn aes128_round_trips_through_the_object_layer() {
        let key = [7u8; 16];
        let data = b"BT /F1 12 Tf (hello) Tj ET".to_vec();
        let ct = aes_cbc_encrypt(&key, &data, &[]).unwrap();
        assert_eq!(ct.len() % 16, 0);
        assert_ne!(&ct[16..], &data[..]);
        assert_eq!(aes_cbc_decrypt(&key, &ct).unwrap(), data);
    }

    #[test]
    fn aes256_round_trips() {
        let key = [3u8; 32];
        for len in [0usize, 1, 15, 16, 17, 1000] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7) as u8).collect();
            let ct = aes_cbc_encrypt(&key, &data, &[]).unwrap();
            assert_eq!(aes_cbc_decrypt(&key, &ct).unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn aes_decrypt_of_a_too_short_stream_is_empty_not_an_error() {
        assert_eq!(aes_cbc_decrypt(&[0u8; 16], b"short").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn hash_2b_r5_is_a_plain_sha256() {
        let expected = Sha256::digest(b"pwsalt").to_vec();
        assert_eq!(hash_2b(b"pw", b"salt", b"", 5), expected);
    }

    #[test]
    fn hash_2b_r6_terminates_and_returns_32_bytes() {
        let h = hash_2b(b"password", b"saltsalt", b"", 6);
        assert_eq!(h.len(), 32);
        // Different salts must give different hashes.
        assert_ne!(h, hash_2b(b"password", b"taltsalt", b"", 6));
    }

    #[test]
    fn identity_crypt_filter_leaves_data_alone() {
        let mut d = Dictionary::new();
        d.insert(Name::new("Filter"), Object::name("Standard"));
        d.insert(Name::new("V"), Object::Integer(4));
        d.insert(Name::new("R"), Object::Integer(4));
        d.insert(Name::new("StmF"), Object::name("Identity"));
        d.insert(Name::new("StrF"), Object::name("Identity"));
        let (stream, string, _) = crypt_filters(&d, 4, 128).unwrap();
        assert_eq!(stream, Cipher::None);
        assert_eq!(string, Cipher::None);
    }

    #[test]
    fn unsupported_revision_is_reported_not_guessed() {
        let mut d = Dictionary::new();
        d.insert(Name::new("Filter"), Object::name("Standard"));
        d.insert(Name::new("V"), Object::Integer(3));
        d.insert(Name::new("R"), Object::Integer(3));
        let err = Decryptor::new(&d, b"id", "", None).unwrap_err();
        assert!(matches!(err, CosError::UnsupportedEncryption(_)));
    }
}
