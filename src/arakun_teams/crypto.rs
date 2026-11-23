//! The cryptography of Arakun Teams, as `cloud/PROTOCOL.md` says it: the
//! device's two P-256 key pairs, the envelopes that carry a team key to one
//! device, and what is sealed with a team key (channel names, messages).
//!
//! ring signs (ECDSA P-256 with SHA-256, raw r||s, as WebCrypto makes and
//! checks them), seals (AES-256-GCM), derives keys (HKDF-SHA256) and makes the
//! one-use ECDH keys of the envelopes it sends. ring cannot keep an ECDH key,
//! so p256 holds the device's `enc` key and opens the envelopes sent to it.
//!
//! `testdata/vectors.json` holds values WebCrypto made, which the tests open
//! and check, and values this file made, which `testdata/vectors.mjs --check`
//! checks with WebCrypto.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::elliptic_curve::sec1::ToSec1Point as _;
use ring::signature::KeyPair as _;
use ring::{aead, agreement, hkdf, rand, signature};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

/// The protocol's version, in every encrypted object.
pub const V: u8 = 1;
const PREFIX: &str = "arakun-teams/v1";
/// An uncompressed SEC1 P-256 point.
const POINT_LEN: usize = 65;
const NONCE_LEN: usize = 12;
const SIG_LEN: usize = 64;
pub const KEY_LEN: usize = 32;

/// One epoch's team key.
pub type TeamKey = [u8; KEY_LEN];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CryptoError {
    /// A field is not the size or shape the protocol says; the word tells which.
    Malformed(&'static str),
    /// The signature does not check out against the key it should.
    BadSignature,
    /// Sealed for another device, team or epoch than the one opening it.
    NotForUs,
    /// The ciphertext does not open: a wrong key, or it was changed.
    Decrypt,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::Malformed(what) => write!(f, "malformed {what}"),
            CryptoError::BadSignature => write!(f, "the signature does not check out"),
            CryptoError::NotForUs => write!(f, "sealed for another device, team or epoch"),
            CryptoError::Decrypt => write!(f, "does not decrypt"),
        }
    }
}

pub fn b64(bytes: impl AsRef<[u8]>) -> String {
    B64.encode(bytes)
}

pub fn unb64(text: &str) -> Option<Vec<u8>> {
    B64.decode(text).ok()
}

/// A base64url field of exactly `len` bytes.
fn field(text: &str, len: usize, what: &'static str) -> Result<Vec<u8>, CryptoError> {
    unb64(text)
        .filter(|b| b.len() == len)
        .ok_or(CryptoError::Malformed(what))
}

fn point(text: &str, what: &'static str) -> Result<Vec<u8>, CryptoError> {
    field(text, POINT_LEN, what).and_then(|p| {
        if p[0] == 4 {
            Ok(p)
        } else {
            Err(CryptoError::Malformed(what))
        }
    })
}

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("the OS has random bytes");
    out
}

pub fn new_team_key() -> TeamKey {
    random()
}

/// base64url of the first 16 bytes of SHA-256 of the `sig` public key.
pub fn device_id_of(sig_pub: &[u8]) -> String {
    b64(&sha2::Sha256::digest(sig_pub)[..16])
}

/// Whether `id` has the shape of a device ID: 16 bytes in base64url, 22
/// characters with no padding.
pub fn is_device_id(id: &str) -> bool {
    id.len() == 22
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The numbers two people read to each other to check that a device is who
/// it says, Signal style: six groups of five digits from both devices'
/// public keys (base64url), each given as `(sig_pub, enc_pub)`. Both keys
/// count: team keys are sealed to `enc_pub`, and the first device of an
/// account has no endorsement that ties its `enc_pub` to its `sig_pub`.
/// Sorted first, so each side gets the same numbers whichever device it is
/// on. `None` when a key is not a P-256 point.
pub fn compare_numbers(a: (&str, &str), b: (&str, &str)) -> Option<[String; 6]> {
    let keys = |(sig, enc): (&str, &str)| -> Option<Vec<u8>> {
        let mut both = point(sig, "sig_pub").ok()?;
        both.extend(point(enc, "enc_pub").ok()?);
        Some(both)
    };
    let (a, b) = (keys(a)?, keys(b)?);
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut h = sha2::Sha512::new();
    h.update(b"arakun-teams compare v2");
    h.update(&lo);
    h.update(&hi);
    let digest = h.finalize();
    Some(std::array::from_fn(|i| {
        let n = digest[i * 5..i * 5 + 5]
            .iter()
            .fold(0u64, |n, b| (n << 8) | u64::from(*b));
        format!("{:05}", n % 100_000)
    }))
}

/// Whether `sig` (base64url, raw r||s) is the signature of `sig_pub` over the
/// UTF-8 bytes of `text`.
pub fn verify(sig_pub: &[u8], text: &str, sig: &str) -> bool {
    let Some(sig) = unb64(sig).filter(|s| s.len() == SIG_LEN) else {
        return false;
    };
    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, sig_pub)
        .verify(text.as_bytes(), &sig)
        .is_ok()
}

// ---- what is signed, and the associated data --------------------------------

/// What a device's endorsement of another device of the same user signs.
pub fn device_signed(github_id: u64, sig_pub: &str, enc_pub: &str) -> String {
    format!("{PREFIX}/device|{github_id}|{sig_pub}|{enc_pub}")
}

/// The HKDF info, and the associated data, of an envelope.
fn envelope_info(team_id: &str, epoch: u64, device_id: &str) -> String {
    format!("{PREFIX}/envelope|{team_id}|{epoch}|{device_id}")
}

fn envelope_signed(e: &Envelope) -> String {
    format!(
        "{}|{}|{}|{}",
        envelope_info(&e.team_id, e.epoch, &e.device_id),
        e.eph_pub,
        e.nonce,
        e.ct
    )
}

/// The associated data of a sealed channel name or message.
pub fn sealed_ad(what: Sealing, team_id: &str, channel_id: &str, id: &str) -> String {
    let what = match what {
        Sealing::Channel => "channel",
        Sealing::Message => "message",
    };
    format!("{PREFIX}/{what}|{team_id}|{channel_id}|{id}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sealing {
    Channel,
    Message,
}

fn message_signed(team_id: &str, channel_id: &str, m: &WireMessage) -> String {
    format!(
        "{}|{}|{}|{}",
        sealed_ad(Sealing::Message, team_id, channel_id, &m.id),
        m.epoch,
        m.nonce,
        m.ct
    )
}

// ---- the device -----------------------------------------------------------------

/// This install's two key pairs: `sig` signs what it publishes, `enc`
/// receives team keys. Made once, kept in the keyring, never sent anywhere.
pub struct Device {
    sig: signature::EcdsaKeyPair,
    sig_pkcs8: Vec<u8>,
    enc: p256::SecretKey,
    sig_pub: String,
    enc_pub: String,
    id: String,
}

/// The device's secrets as the keyring keeps them.
#[derive(Serialize, Deserialize)]
struct Secret {
    v: u8,
    /// The `sig` pair as PKCS#8, base64url.
    sig: String,
    /// The `enc` private scalar, 32 bytes, base64url.
    enc: String,
}

impl Device {
    pub fn generate() -> Device {
        let rng = rand::SystemRandom::new();
        let doc = signature::EcdsaKeyPair::generate_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &rng,
        )
        .expect("ring can make a P-256 key");
        // A random 32 bytes is a valid scalar but for a chance of about 2^-32.
        let enc = loop {
            if let Ok(k) = p256::SecretKey::from_slice(&random::<KEY_LEN>()) {
                break k;
            }
        };
        Device::from_parts(doc.as_ref(), enc).expect("keys just made are good")
    }

    fn from_parts(sig_pkcs8: &[u8], enc: p256::SecretKey) -> Option<Device> {
        let sig = signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            sig_pkcs8,
            &rand::SystemRandom::new(),
        )
        .ok()?;
        let sig_raw = sig.public_key().as_ref().to_vec();
        let enc_raw = enc.public_key().to_sec1_point(false);
        Some(Device {
            id: device_id_of(&sig_raw),
            sig_pub: b64(&sig_raw),
            enc_pub: b64(enc_raw.as_bytes()),
            sig,
            sig_pkcs8: sig_pkcs8.to_vec(),
            enc,
        })
    }

    /// The device as the keyring keeps it: JSON of PKCS#8 and the raw scalar.
    pub fn to_secret(&self) -> String {
        serde_json::to_string(&Secret {
            v: V,
            sig: b64(&self.sig_pkcs8),
            enc: b64(self.enc.to_bytes()),
        })
        .expect("plain JSON")
    }

    pub fn from_secret(text: &str) -> Option<Device> {
        let s: Secret = serde_json::from_str(text).ok()?;
        if s.v != V {
            return None;
        }
        let enc = p256::SecretKey::from_slice(&unb64(&s.enc)?).ok()?;
        Device::from_parts(&unb64(&s.sig)?, enc)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn sig_pub(&self) -> &str {
        &self.sig_pub
    }

    pub fn enc_pub(&self) -> &str {
        &self.enc_pub
    }

    /// A signature over the UTF-8 bytes of `text`: raw r||s, base64url.
    pub fn sign(&self, text: &str) -> String {
        b64(self
            .sig
            .sign(&rand::SystemRandom::new(), text.as_bytes())
            .expect("signing does not fail"))
    }

    /// Endorses another device of the same user, so the server and the
    /// other members trust it.
    pub fn endorse(&self, github_id: u64, sig_pub: &str, enc_pub: &str) -> String {
        self.sign(&device_signed(github_id, sig_pub, enc_pub))
    }

    /// The ECDH secret with a public point: its x coordinate, as WebCrypto's
    /// `deriveBits(…, 256)` gives it.
    fn agree(&self, public: &[u8]) -> Result<[u8; KEY_LEN], CryptoError> {
        let theirs = p256::PublicKey::from_sec1_bytes(public)
            .map_err(|_| CryptoError::Malformed("eph_pub"))?;
        let shared = p256::ecdh::diffie_hellman(self.enc.to_nonzero_scalar(), theirs.as_affine());
        let mut out = [0u8; KEY_LEN];
        out.copy_from_slice(shared.raw_secret_bytes());
        Ok(out)
    }
}

// ---- envelopes ---------------------------------------------------------------------

/// One epoch's team key, sealed to one device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u8,
    pub team_id: String,
    pub epoch: u64,
    /// The recipient.
    pub device_id: String,
    /// The sender.
    pub from: String,
    pub eph_pub: String,
    pub nonce: String,
    pub ct: String,
    pub sig: String,
}

/// The AES-256-GCM key of an envelope: HKDF-SHA256 of the ECDH secret, with
/// the one-use public key as salt and the envelope's info string.
fn envelope_key(shared: &[u8], eph_pub: &[u8], info: &str) -> aead::LessSafeKey {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, eph_pub).extract(shared);
    let info = [info.as_bytes()];
    let okm = prk
        .expand(&info, &aead::AES_256_GCM)
        .expect("32 bytes is in HKDF's range");
    aead::LessSafeKey::new(aead::UnboundKey::from(okm))
}

/// Seals `key` (epoch `epoch` of `team_id`) to the device `to_id`, whose
/// `enc` public key is `to_enc_pub` (base64url), signed by `from`.
pub fn seal_envelope(
    team_id: &str,
    epoch: u64,
    key: &TeamKey,
    to_id: &str,
    to_enc_pub: &str,
    from: &Device,
) -> Result<Envelope, CryptoError> {
    let theirs = point(to_enc_pub, "enc_pub")?;
    let rng = rand::SystemRandom::new();
    let eph = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
        .expect("ring can make a key");
    let eph_pub = eph
        .compute_public_key()
        .expect("a fresh key has a public half")
        .as_ref()
        .to_vec();
    let info = envelope_info(team_id, epoch, to_id);
    let sealer = agreement::agree_ephemeral(
        eph,
        &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &theirs),
        |shared| envelope_key(shared, &eph_pub, &info),
    )
    .map_err(|_| CryptoError::Malformed("enc_pub"))?;
    let nonce: [u8; NONCE_LEN] = random();
    let mut ct = key.to_vec();
    sealer
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(info.as_bytes()),
            &mut ct,
        )
        .map_err(|_| CryptoError::Decrypt)?;
    let mut e = Envelope {
        v: V,
        team_id: team_id.to_string(),
        epoch,
        device_id: to_id.to_string(),
        from: from.id().to_string(),
        eph_pub: b64(&eph_pub),
        nonce: b64(nonce),
        ct: b64(&ct),
        sig: String::new(),
    };
    e.sig = from.sign(&envelope_signed(&e));
    Ok(e)
}

/// The team key in an envelope for `me`, of `team_id`, signed by the device
/// whose `sig` public key is `sender_sig_pub` (raw). The caller checks that
/// the sender is a device of one of the team's admins, and that it is the
/// device `e.from` names.
pub fn open_envelope(
    e: &Envelope,
    team_id: &str,
    me: &Device,
    sender_sig_pub: &[u8],
) -> Result<TeamKey, CryptoError> {
    if e.v != V {
        return Err(CryptoError::Malformed("v"));
    }
    if e.team_id != team_id || e.device_id != me.id() || e.epoch == 0 {
        return Err(CryptoError::NotForUs);
    }
    if device_id_of(sender_sig_pub) != e.from {
        return Err(CryptoError::BadSignature);
    }
    if !verify(sender_sig_pub, &envelope_signed(e), &e.sig) {
        return Err(CryptoError::BadSignature);
    }
    let eph_pub = point(&e.eph_pub, "eph_pub")?;
    let nonce = field(&e.nonce, NONCE_LEN, "nonce")?;
    let mut ct = field(&e.ct, KEY_LEN + aead::AES_256_GCM.tag_len(), "ct")?;
    let info = envelope_info(&e.team_id, e.epoch, &e.device_id);
    let opener = envelope_key(&me.agree(&eph_pub)?, &eph_pub, &info);
    let plain = opener
        .open_in_place(
            aead::Nonce::try_assume_unique_for_key(&nonce).map_err(|_| CryptoError::Decrypt)?,
            aead::Aad::from(info.as_bytes()),
            &mut ct,
        )
        .map_err(|_| CryptoError::Decrypt)?;
    plain.try_into().map_err(|_| CryptoError::Decrypt)
}

// ---- sealed with a team key ---------------------------------------------------

/// Anything encrypted with a team key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sealed {
    pub v: u8,
    pub epoch: u64,
    pub nonce: String,
    pub ct: String,
}

fn team_cipher(key: &TeamKey) -> aead::LessSafeKey {
    aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, key).expect("32 bytes is an AES-256 key"),
    )
}

fn seal_with(
    key: &TeamKey,
    nonce: [u8; NONCE_LEN],
    epoch: u64,
    ad: &str,
    plaintext: &[u8],
) -> Sealed {
    let mut ct = plaintext.to_vec();
    team_cipher(key)
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(ad.as_bytes()),
            &mut ct,
        )
        .expect("AES-GCM seals anything this size");
    Sealed {
        v: V,
        epoch,
        nonce: b64(nonce),
        ct: b64(&ct),
    }
}

/// AES-256-GCM with the team key of `epoch`, a random nonce and `ad`.
pub fn seal(key: &TeamKey, epoch: u64, ad: &str, plaintext: &[u8]) -> Sealed {
    seal_with(key, random(), epoch, ad, plaintext)
}

/// What `sealed` holds, given the key of its epoch.
pub fn open(key: &TeamKey, sealed: &Sealed, ad: &str) -> Result<Vec<u8>, CryptoError> {
    if sealed.v != V {
        return Err(CryptoError::Malformed("v"));
    }
    open_raw(key, &sealed.nonce, &sealed.ct, ad)
}

fn open_raw(key: &TeamKey, nonce: &str, ct: &str, ad: &str) -> Result<Vec<u8>, CryptoError> {
    let nonce = field(nonce, NONCE_LEN, "nonce")?;
    let mut ct = unb64(ct).ok_or(CryptoError::Malformed("ct"))?;
    let plain = team_cipher(key)
        .open_in_place(
            aead::Nonce::try_assume_unique_for_key(&nonce).map_err(|_| CryptoError::Decrypt)?,
            aead::Aad::from(ad.as_bytes()),
            &mut ct,
        )
        .map_err(|_| CryptoError::Decrypt)?;
    Ok(plain.to_vec())
}

/// A channel's name, sealed for its team: the id is the channel's own.
pub fn seal_channel_name(
    team_id: &str,
    channel_id: &str,
    epoch: u64,
    key: &TeamKey,
    name: &str,
) -> Sealed {
    let ad = sealed_ad(Sealing::Channel, team_id, channel_id, channel_id);
    seal(key, epoch, &ad, name.as_bytes())
}

pub fn open_channel_name(
    team_id: &str,
    channel_id: &str,
    key: &TeamKey,
    sealed: &Sealed,
) -> Result<String, CryptoError> {
    let ad = sealed_ad(Sealing::Channel, team_id, channel_id, channel_id);
    String::from_utf8(open(key, sealed, &ad)?).map_err(|_| CryptoError::Malformed("name"))
}

/// What this device keeps on disk: the nonce, then AES-256-GCM of `plain`
/// with a key that never leaves the keyring.
pub fn seal_local(key: &TeamKey, plain: &[u8]) -> Vec<u8> {
    let nonce: [u8; NONCE_LEN] = random();
    let mut out = plain.to_vec();
    team_cipher(key)
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(LOCAL_AD),
            &mut out,
        )
        .expect("AES-GCM seals anything this size");
    [nonce.as_slice(), &out].concat()
}

pub fn open_local(key: &TeamKey, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < NONCE_LEN {
        return Err(CryptoError::Malformed("file"));
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    let mut ct = ct.to_vec();
    let plain = team_cipher(key)
        .open_in_place(
            aead::Nonce::try_assume_unique_for_key(nonce).map_err(|_| CryptoError::Decrypt)?,
            aead::Aad::from(LOCAL_AD),
            &mut ct,
        )
        .map_err(|_| CryptoError::Decrypt)?;
    Ok(plain.to_vec())
}

const LOCAL_AD: &[u8] = b"arakun-teams/v1/local";

// ---- messages ------------------------------------------------------------------

/// A message as the server stores and forwards it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireMessage {
    pub id: String,
    pub device_id: String,
    pub epoch: u64,
    pub nonce: String,
    pub ct: String,
    pub sig: String,
}

/// What a message says, inside its `ct`. Kinds this app does not know yet
/// (`bot`, `decision`, `decision_accept`) still carry `text`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Body {
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub bot: Option<serde_json::Value>,
    #[serde(default)]
    pub decision: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordination: Option<serde_json::Value>,
}

impl Body {
    pub fn text(text: impl Into<String>, reply_to: Option<String>) -> Body {
        Body {
            kind: "text".into(),
            text: text.into(),
            reply_to,
            mentions: Vec::new(),
            bot: None,
            decision: None,
            coordination: None,
        }
    }
}

/// A message from `me`, sealed with the key of `epoch` and signed.
pub fn seal_message(
    team_id: &str,
    channel_id: &str,
    id: &str,
    epoch: u64,
    key: &TeamKey,
    me: &Device,
    body: &Body,
) -> WireMessage {
    let plain = serde_json::to_vec(body).expect("plain JSON");
    let ad = sealed_ad(Sealing::Message, team_id, channel_id, id);
    let sealed = seal(key, epoch, &ad, &plain);
    sign_message(team_id, channel_id, id, me, sealed)
}

fn sign_message(
    team_id: &str,
    channel_id: &str,
    id: &str,
    me: &Device,
    sealed: Sealed,
) -> WireMessage {
    let mut m = WireMessage {
        id: id.to_string(),
        device_id: me.id().to_string(),
        epoch: sealed.epoch,
        nonce: sealed.nonce,
        ct: sealed.ct,
        sig: String::new(),
    };
    m.sig = me.sign(&message_signed(team_id, channel_id, &m));
    m
}

/// Whether a message's signature is its device's: `sender_sig_pub` (raw) is
/// the `sig` key of the device `m.device_id` names.
pub fn verify_message(
    team_id: &str,
    channel_id: &str,
    m: &WireMessage,
    sender_sig_pub: &[u8],
) -> bool {
    device_id_of(sender_sig_pub) == m.device_id
        && verify(
            sender_sig_pub,
            &message_signed(team_id, channel_id, m),
            &m.sig,
        )
}

/// What a message says, with the key of its epoch. The signature is checked
/// apart, with [`verify_message`].
pub fn open_message(
    team_id: &str,
    channel_id: &str,
    m: &WireMessage,
    key: &TeamKey,
) -> Result<Body, CryptoError> {
    let ad = sealed_ad(Sealing::Message, team_id, channel_id, &m.id);
    let plain = open_raw(key, &m.nonce, &m.ct, &ad)?;
    serde_json::from_slice(&plain).map_err(|_| CryptoError::Malformed("body"))
}

// ---- IDs -------------------------------------------------------------------------

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A ULID: 48 bits of milliseconds and 80 random bits, in 26 characters of
/// Crockford base32, so IDs sort by time.
pub fn ulid() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    ulid_from(ms, random())
}

fn ulid_from(ms: u64, rand: [u8; 10]) -> String {
    let mut n: u128 = u128::from(ms & 0xffff_ffff_ffff) << 80;
    for (i, b) in rand.iter().enumerate() {
        n |= u128::from(*b) << (72 - 8 * i);
    }
    (0..26)
        .map(|i| CROCKFORD[((n >> (125 - 5 * i)) & 31) as usize] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const VECTORS: &str = include_str!("testdata/vectors.json");

    fn vectors() -> Value {
        serde_json::from_str(VECTORS).unwrap()
    }

    fn s<'a>(v: &'a Value, path: &[&str]) -> &'a str {
        path.iter().fold(v, |v, k| &v[*k]).as_str().unwrap()
    }

    /// A device from the vectors' WebCrypto keys: PKCS#8 and the JWK `d`.
    fn vector_device(v: &Value, who: &str) -> Device {
        let pkcs8 = unb64(s(v, &[who, "sig_pkcs8"])).unwrap();
        let d = unb64(s(v, &[who, "enc_d"])).unwrap();
        Device::from_parts(&pkcs8, p256::SecretKey::from_slice(&d).unwrap()).unwrap()
    }

    fn team_key(v: &Value) -> TeamKey {
        unb64(s(v, &["team_key"])).unwrap().try_into().unwrap()
    }

    #[test]
    fn webcrypto_keys_load_and_derive_the_same_ids() {
        let v = vectors();
        for who in ["sender", "recipient"] {
            let d = vector_device(&v, who);
            assert_eq!(d.sig_pub(), s(&v, &[who, "sig_pub"]));
            assert_eq!(d.enc_pub(), s(&v, &[who, "enc_pub"]));
            assert_eq!(d.id(), s(&v, &[who, "device_id"]));
            assert_eq!(d.id().len(), 22);
        }
    }

    #[test]
    fn compare_numbers_match_on_both_sides() {
        fn pair(k: &(String, String)) -> (&str, &str) {
            (&k.0, &k.1)
        }
        let keys = |d: &Device| (d.sig_pub().to_string(), d.enc_pub().to_string());
        let (a, b) = (keys(&Device::generate()), keys(&Device::generate()));
        let mine = compare_numbers(pair(&a), pair(&b)).unwrap();
        assert_eq!(compare_numbers(pair(&b), pair(&a)), Some(mine.clone()));
        assert!(
            mine.iter()
                .all(|g| g.len() == 5 && g.bytes().all(|c| c.is_ascii_digit()))
        );
        let c = keys(&Device::generate());
        assert_ne!(compare_numbers(pair(&a), pair(&c)), Some(mine.clone()));
        // The same signing key with another encryption key, as a server
        // swapping a first device's enc_pub would show it, reads differently.
        let swapped = (b.0.as_str(), c.1.as_str());
        assert_ne!(compare_numbers(pair(&a), swapped), Some(mine));
        assert_eq!(compare_numbers(("not a key", &a.1), pair(&b)), None);
        assert_eq!(compare_numbers((&a.0, "not a key"), pair(&b)), None);
    }

    #[test]
    fn device_id_is_the_start_of_the_sha256_of_sig_pub() {
        // SHA-256 of 65 zero bytes starts 98ce42deef51d40269d542f5314bef2c.
        let id = device_id_of(&[0u8; 65]);
        assert_eq!(unb64(&id).unwrap().len(), 16);
        assert_eq!(
            unb64(&id).unwrap(),
            sha2::Sha256::digest([0u8; 65])[..16].to_vec()
        );
        let d = Device::generate();
        assert_eq!(d.id(), device_id_of(&unb64(d.sig_pub()).unwrap()));
        assert!(is_device_id(&id) && is_device_id(d.id()));
        for bad in ["", "short", "a/b+c=d0123456789012345", &format!("{id}A")] {
            assert!(!is_device_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_device_survives_the_keyring() {
        let d = Device::generate();
        let back = Device::from_secret(&d.to_secret()).unwrap();
        assert_eq!(back.id(), d.id());
        assert_eq!(back.enc_pub(), d.enc_pub());
        assert!(Device::from_secret("{}").is_none());
        assert!(Device::from_secret("not json").is_none());
    }

    #[test]
    fn checks_webcrypto_signatures() {
        let v = vectors();
        let sender = unb64(s(&v, &["sender", "sig_pub"])).unwrap();
        let text = s(&v, &["endorsement", "text"]);
        let sig = s(&v, &["endorsement", "sig"]);
        assert!(verify(&sender, text, sig));
        assert_eq!(
            text,
            device_signed(
                v["github_id"].as_u64().unwrap(),
                s(&v, &["recipient", "sig_pub"]),
                s(&v, &["recipient", "enc_pub"])
            )
        );
        assert!(!verify(&sender, &format!("{text}x"), sig));
        let other = unb64(s(&v, &["recipient", "sig_pub"])).unwrap();
        assert!(!verify(&other, text, sig));
        // DER, or anything but 64 bytes, is not what WebCrypto makes.
        assert!(!verify(&sender, text, &sig[..sig.len() - 2]));
    }

    #[test]
    fn signatures_are_raw_and_check_out() {
        let d = Device::generate();
        let sig = d.sign("hello");
        assert_eq!(unb64(&sig).unwrap().len(), 64);
        assert!(verify(&unb64(d.sig_pub()).unwrap(), "hello", &sig));
        assert!(!verify(&unb64(d.sig_pub()).unwrap(), "hellO", &sig));
    }

    #[test]
    fn opens_a_webcrypto_envelope() {
        let v = vectors();
        let me = vector_device(&v, "recipient");
        let e: Envelope = serde_json::from_value(v["envelope"].clone()).unwrap();
        let sender = unb64(s(&v, &["sender", "sig_pub"])).unwrap();
        let key = open_envelope(&e, s(&v, &["team_id"]), &me, &sender).unwrap();
        assert_eq!(key, team_key(&v));
    }

    #[test]
    fn envelopes_round_trip() {
        let (admin, member) = (Device::generate(), Device::generate());
        let key = new_team_key();
        let e = seal_envelope("T", 2, &key, member.id(), member.enc_pub(), &admin).unwrap();
        assert_eq!((e.v, e.epoch, e.from.as_str()), (1, 2, admin.id()));
        let admin_pub = unb64(admin.sig_pub()).unwrap();
        assert_eq!(open_envelope(&e, "T", &member, &admin_pub), Ok(key));
        // The shapes the Worker checks (cloud/src/wire.ts).
        assert_eq!(unb64(&e.eph_pub).unwrap().len(), 65);
        assert_eq!(unb64(&e.nonce).unwrap().len(), 12);
        assert_eq!(unb64(&e.ct).unwrap().len(), 48);
        assert_eq!(unb64(&e.sig).unwrap().len(), 64);
    }

    #[test]
    fn a_tampered_envelope_does_not_open() {
        let (admin, member) = (Device::generate(), Device::generate());
        let key = new_team_key();
        let e = seal_envelope("T", 2, &key, member.id(), member.enc_pub(), &admin).unwrap();
        let admin_pub = unb64(admin.sig_pub()).unwrap();
        let flip = |s: &str| {
            let mut b = unb64(s).unwrap();
            let last = b.len() - 1;
            b[last] ^= 1;
            b64(b)
        };
        let other = Device::generate();
        let changed: Vec<(Envelope, &str)> = vec![
            (
                Envelope {
                    ct: flip(&e.ct),
                    ..e.clone()
                },
                "ct",
            ),
            (
                Envelope {
                    nonce: flip(&e.nonce),
                    ..e.clone()
                },
                "nonce",
            ),
            (
                Envelope {
                    eph_pub: other.enc_pub().into(),
                    ..e.clone()
                },
                "eph_pub",
            ),
            (
                Envelope {
                    sig: flip(&e.sig),
                    ..e.clone()
                },
                "sig",
            ),
            (
                Envelope {
                    epoch: 3,
                    ..e.clone()
                },
                "epoch",
            ),
            (
                Envelope {
                    from: other.id().into(),
                    ..e.clone()
                },
                "from",
            ),
            (Envelope { v: 2, ..e.clone() }, "v"),
        ];
        for (bad, what) in changed {
            assert!(
                open_envelope(&bad, "T", &member, &admin_pub).is_err(),
                "{what}"
            );
        }
        // Another team, another device, another signer.
        assert_eq!(
            open_envelope(&e, "U", &member, &admin_pub),
            Err(CryptoError::NotForUs)
        );
        assert_eq!(
            open_envelope(&e, "T", &other, &admin_pub),
            Err(CryptoError::NotForUs)
        );
        let other_pub = unb64(other.sig_pub()).unwrap();
        assert_eq!(
            open_envelope(&e, "T", &member, &other_pub),
            Err(CryptoError::BadSignature)
        );
        // Re-signed by the sender after the epoch changed, the key still
        // derives from the old epoch and does not open.
        let mut moved = Envelope {
            epoch: 3,
            ..e.clone()
        };
        moved.sig = admin.sign(&envelope_signed(&moved));
        assert_eq!(
            open_envelope(&moved, "T", &member, &admin_pub),
            Err(CryptoError::Decrypt)
        );
        // Sealed to someone else, then readdressed to us.
        let mut readdressed =
            seal_envelope("T", 2, &key, other.id(), other.enc_pub(), &admin).unwrap();
        readdressed.device_id = member.id().into();
        readdressed.sig = admin.sign(&envelope_signed(&readdressed));
        assert_eq!(
            open_envelope(&readdressed, "T", &member, &admin_pub),
            Err(CryptoError::Decrypt)
        );
    }

    #[test]
    fn opens_a_webcrypto_message_and_channel_name() {
        let v = vectors();
        let key = team_key(&v);
        let (team, channel) = (s(&v, &["team_id"]), s(&v, &["channel_id"]));
        let m: WireMessage = serde_json::from_value(v["message"]["wire"].clone()).unwrap();
        let sender = unb64(s(&v, &["sender", "sig_pub"])).unwrap();
        assert!(verify_message(team, channel, &m, &sender));
        let body = open_message(team, channel, &m, &key).unwrap();
        assert_eq!(body, Body::text("hello, team", None));
        let sealed: Sealed = serde_json::from_value(v["channel_name"]["sealed"].clone()).unwrap();
        assert_eq!(
            open_channel_name(team, channel, &key, &sealed).unwrap(),
            "general"
        );
    }

    #[test]
    fn seals_byte_for_byte_like_webcrypto() {
        // A known answer: the same key, nonce, associated data and plaintext
        // give WebCrypto's ciphertext.
        let v = vectors();
        let key = team_key(&v);
        let (team, channel) = (s(&v, &["team_id"]), s(&v, &["channel_id"]));
        let nonce: [u8; 12] = unb64(s(&v, &["message", "wire", "nonce"]))
            .unwrap()
            .try_into()
            .unwrap();
        let id = s(&v, &["message", "wire", "id"]);
        let ad = sealed_ad(Sealing::Message, team, channel, id);
        let body = s(&v, &["message", "body"]);
        let sealed = seal_with(&key, nonce, 3, &ad, body.as_bytes());
        assert_eq!(sealed.ct, s(&v, &["message", "wire", "ct"]));
        // And our JSON of the same body is the same bytes.
        assert_eq!(
            serde_json::to_string(&Body::text("hello, team", None)).unwrap(),
            body
        );
        let ad = sealed_ad(Sealing::Channel, team, channel, channel);
        let name = seal_with(&key, nonce, 3, &ad, b"general");
        assert_eq!(name.ct, s(&v, &["channel_name", "sealed", "ct"]));
    }

    #[test]
    fn messages_round_trip_and_refuse_tampering() {
        let me = Device::generate();
        let key = new_team_key();
        let body = Body::text("ship it", Some("01JD0000000000000000000009".into()));
        let m = seal_message("T", "C", "01JD000000000000000000000A", 4, &key, &me, &body);
        let pubkey = unb64(me.sig_pub()).unwrap();
        assert!(verify_message("T", "C", &m, &pubkey));
        assert_eq!(open_message("T", "C", &m, &key).unwrap(), body);

        let flip = |s: &str| {
            let mut b = unb64(s).unwrap();
            b[0] ^= 1;
            b64(b)
        };
        for (bad, what) in [
            (
                WireMessage {
                    ct: flip(&m.ct),
                    ..m.clone()
                },
                "ct",
            ),
            (
                WireMessage {
                    nonce: flip(&m.nonce),
                    ..m.clone()
                },
                "nonce",
            ),
            (
                WireMessage {
                    epoch: 5,
                    ..m.clone()
                },
                "epoch",
            ),
            (
                WireMessage {
                    id: "01JD000000000000000000000B".into(),
                    ..m.clone()
                },
                "id",
            ),
            (
                WireMessage {
                    sig: flip(&m.sig),
                    ..m.clone()
                },
                "sig",
            ),
        ] {
            assert!(!verify_message("T", "C", &bad, &pubkey), "{what}");
        }
        // Another channel or team: neither the signature nor the seal holds.
        assert!(!verify_message("T", "D", &m, &pubkey));
        assert!(open_message("T", "D", &m, &key).is_err());
        assert!(open_message("U", "C", &m, &key).is_err());
        // Another device's key, or the key of another epoch.
        let other = Device::generate();
        assert!(!verify_message(
            "T",
            "C",
            &m,
            &unb64(other.sig_pub()).unwrap()
        ));
        assert_eq!(
            open_message("T", "C", &m, &new_team_key()),
            Err(CryptoError::Decrypt)
        );
        let tampered = WireMessage {
            ct: flip(&m.ct),
            ..m.clone()
        };
        assert_eq!(
            open_message("T", "C", &tampered, &key),
            Err(CryptoError::Decrypt)
        );
    }

    #[test]
    fn reads_kinds_it_does_not_know() {
        let me = Device::generate();
        let key = new_team_key();
        let plain = br#"{"kind":"decision","text":"We ship Friday","reply_to":null,"mentions":["bot"],"bot":null,"decision":{"of":"X"},"later":1}"#;
        let ad = sealed_ad(Sealing::Message, "T", "C", "M");
        let m = sign_message("T", "C", "M", &me, seal(&key, 1, &ad, plain));
        let body = open_message("T", "C", &m, &key).unwrap();
        assert_eq!(body.kind, "decision");
        assert_eq!(body.text, "We ship Friday");
    }

    #[test]
    fn endorsements_sign_what_the_server_checks() {
        let (old, new) = (Device::generate(), Device::generate());
        let sig = old.endorse(42, new.sig_pub(), new.enc_pub());
        let text = format!(
            "arakun-teams/v1/device|42|{}|{}",
            new.sig_pub(),
            new.enc_pub()
        );
        assert!(verify(&unb64(old.sig_pub()).unwrap(), &text, &sig));
    }

    #[test]
    fn ulids_look_like_the_servers() {
        let id = ulid();
        assert_eq!(id.len(), 26);
        // cloud/src/util.ts: ^[0-7][0-9A-HJKMNP-TV-Z]{25}$
        assert!(matches!(id.as_bytes()[0], b'0'..=b'7'));
        assert!(id.bytes().all(|b| CROCKFORD.contains(&b)));
        // The ULID spec's example time, 1469918176385 ms, is 01ARYZ6S41.
        assert_eq!(&ulid_from(1_469_918_176_385, [0; 10])[..10], "01ARYZ6S41");
        assert_eq!(&ulid_from(0, [0xff; 10])[10..], "ZZZZZZZZZZZZZZZZ");
        assert!(ulid_from(1, [0; 10]) < ulid_from(2, [0; 10]));
    }

    /// The values the `rust` part of the vectors holds: made here once, and
    /// checked with WebCrypto by `vectors.mjs --check`. Here they must still
    /// check out with this code.
    #[test]
    fn the_values_webcrypto_checked_still_hold() {
        let v = vectors();
        let r = &v["rust"];
        assert!(r.is_object(), "run the ignored print_rust_vectors test");
        let signer = unb64(s(&v, &["recipient", "sig_pub"])).unwrap();
        assert!(verify(
            &signer,
            s(r, &["endorsement", "text"]),
            s(r, &["endorsement", "sig"])
        ));
        let e: Envelope = serde_json::from_value(r["envelope"].clone()).unwrap();
        let sender = vector_device(&v, "sender");
        assert_eq!(
            open_envelope(&e, s(&v, &["team_id"]), &sender, &signer),
            Ok(team_key(&v))
        );
        let m: WireMessage = serde_json::from_value(r["message"]["wire"].clone()).unwrap();
        let (team, channel) = (s(&v, &["team_id"]), s(&v, &["channel_id"]));
        assert!(verify_message(team, channel, &m, &signer));
        let body = open_message(team, channel, &m, &team_key(&v)).unwrap();
        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            s(r, &["message", "body"])
        );
    }

    /// Prints the `rust` part of the vectors: an endorsement, an envelope to
    /// the sender and a message, all by the recipient device.
    #[test]
    #[ignore]
    fn print_rust_vectors() {
        let v = vectors();
        let me = vector_device(&v, "recipient");
        let text = device_signed(9001, "AAAA", "BBBB");
        let e = seal_envelope(
            s(&v, &["team_id"]),
            v["epoch"].as_u64().unwrap(),
            &team_key(&v),
            s(&v, &["sender", "device_id"]),
            s(&v, &["sender", "enc_pub"]),
            &me,
        )
        .unwrap();
        let body = Body::text("hello back, from Rust", None);
        let m = seal_message(
            s(&v, &["team_id"]),
            s(&v, &["channel_id"]),
            "01JD0000000000000000000003",
            v["epoch"].as_u64().unwrap(),
            &team_key(&v),
            &me,
            &body,
        );
        let out = serde_json::json!({
            "endorsement": { "text": text, "sig": me.sign(&text) },
            "envelope": e,
            "message": { "body": serde_json::to_string(&body).unwrap(), "wire": m },
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    }
}
