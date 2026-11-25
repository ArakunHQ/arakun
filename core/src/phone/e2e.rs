//! End-to-end encryption between a phone and this computer through the relay,
//! which only passes bytes along. Each connection starts with a handshake in
//! the clear: the phone sends an ephemeral P-256 key and a nonce (`hello`), the
//! computer answers with its own and signs the whole exchange with its
//! identity, the key its label derives from, so the phone knows the ephemeral
//! key it is agreeing with belongs to the computer it scanned. Both derive two
//! AES-256-GCM keys (one per direction) from the ECDH secret with HKDF-SHA256,
//! and every `data` frame from then on is numbered and sealed; a frame out of
//! order or tampered with ends the session. `phone/public/e2e.js` is the phone's half:
//! the two must agree byte for byte, which `phone/test/vectors/e2e.json` checks.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::{aead, agreement, hkdf, rand, signature};
use serde_json::{Value, json};

use super::identity::{Identity, label_of};
use super::relay::DATA;

/// The protocol the hello speaks; a phone that knows only an older one is told so.
pub const VERSION: u64 = 1;
const TRANSCRIPT_TAG: &[u8] = b"arakun-phone-v1";
const KEYS_INFO: &[u8] = b"arakun-phone-v1 keys";
const NONCE_LEN: usize = 16;
const POINT_LEN: usize = 65;
const KEY_LEN: usize = 32;
const COUNTER_LEN: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum E2eError {
    /// The hello is not what the protocol says; the word tells which part.
    BadHello(&'static str),
    /// The frame's number is not the one expected next: a replay, a loss, or a trick.
    Counter,
    /// The ciphertext does not open with the session's key.
    Cipher,
}

impl std::fmt::Display for E2eError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            E2eError::BadHello(what) => write!(f, "bad hello: {what}"),
            E2eError::Counter => write!(f, "frame out of order"),
            E2eError::Cipher => write!(f, "frame does not decrypt"),
        }
    }
}

/// What a phone opens the handshake with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneHello {
    pub version: u64,
    /// The phone's ephemeral public key, uncompressed (65 bytes).
    pub ephemeral: Vec<u8>,
    pub nonce: [u8; NONCE_LEN],
}

impl PhoneHello {
    pub fn parse(payload: &[u8]) -> Result<PhoneHello, E2eError> {
        let v: Value =
            serde_json::from_slice(payload).map_err(|_| E2eError::BadHello("not json"))?;
        let version = v["v"].as_u64().ok_or(E2eError::BadHello("v"))?;
        let ephemeral = field(&v, "e", POINT_LEN)?;
        let nonce = field(&v, "nonce", NONCE_LEN)?
            .try_into()
            .expect("checked length");
        Ok(PhoneHello {
            version,
            ephemeral,
            nonce,
        })
    }
}

/// A base64url field of exactly `len` bytes.
fn field(v: &Value, name: &'static str, len: usize) -> Result<Vec<u8>, E2eError> {
    let bytes = v[name]
        .as_str()
        .and_then(|s| B64.decode(s).ok())
        .ok_or(E2eError::BadHello(name))?;
    if bytes.len() != len {
        return Err(E2eError::BadHello(name));
    }
    Ok(bytes)
}

/// This side's half of a handshake: an ephemeral key and a nonce, used once.
pub struct Handshake {
    private: agreement::EphemeralPrivateKey,
    public: Vec<u8>,
    nonce: [u8; NONCE_LEN],
}

impl Handshake {
    pub fn begin() -> Handshake {
        let rng = rand::SystemRandom::new();
        let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)
            .expect("ring can make a key");
        let public = private
            .compute_public_key()
            .expect("a fresh key has a public half")
            .as_ref()
            .to_vec();
        let mut nonce = [0u8; NONCE_LEN];
        rand::SecureRandom::fill(&rng, &mut nonce).expect("the OS has random bytes");
        Handshake {
            private,
            public,
            nonce,
        }
    }

    /// The computer's answer to a phone's hello on `channel`: the hello to send
    /// back (JSON) and the session both sides now share. The phone's hello must
    /// speak this version.
    pub fn answer(
        self,
        hello: &PhoneHello,
        identity: &Identity,
        channel: u32,
        arakun: &str,
    ) -> Result<(Vec<u8>, Session), E2eError> {
        if hello.version != VERSION {
            return Err(E2eError::BadHello("version"));
        }
        let label = identity.label();
        let signed = transcript(
            &label,
            channel,
            &hello.ephemeral,
            &self.public,
            &hello.nonce,
            &self.nonce,
        );
        let reply = json!({
            "v": VERSION,
            "pub": B64.encode(identity.public_raw()),
            "e": B64.encode(&self.public),
            "nonce": B64.encode(self.nonce),
            "sig": B64.encode(identity.sign(&signed)),
            "arakun": arakun,
        });
        let shared = agree(self.private, &hello.ephemeral)?;
        let (phone_to_pc, pc_to_phone) = derive_keys(&shared, &hello.nonce, &self.nonce);
        Ok((
            reply.to_string().into_bytes(),
            Session::new(pc_to_phone, phone_to_pc),
        ))
    }

    /// The hello a phone opens with, from this handshake. The app is the phone
    /// when it looks at an `arakun server` (`crate::servers`).
    pub fn hello(&self) -> Vec<u8> {
        json!({ "v": VERSION, "e": B64.encode(&self.public), "nonce": B64.encode(self.nonce) })
            .to_string()
            .into_bytes()
    }

    /// The phone's side: checks the computer's hello is from the key `label`
    /// names and signs this very exchange, then derives the same session.
    pub fn finish(self, reply: &[u8], label: &str, channel: u32) -> Result<Session, E2eError> {
        let v: Value = serde_json::from_slice(reply).map_err(|_| E2eError::BadHello("not json"))?;
        if v["v"].as_u64() != Some(VERSION) {
            return Err(E2eError::BadHello("version"));
        }
        let public = field(&v, "pub", POINT_LEN)?;
        if label_of(&public) != label {
            return Err(E2eError::BadHello("label"));
        }
        let ephemeral = field(&v, "e", POINT_LEN)?;
        let nonce: [u8; NONCE_LEN] = field(&v, "nonce", NONCE_LEN)?
            .try_into()
            .expect("checked length");
        let sig = field(&v, "sig", 64)?;
        let signed = transcript(
            label,
            channel,
            &self.public,
            &ephemeral,
            &self.nonce,
            &nonce,
        );
        signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, &public)
            .verify(&signed, &sig)
            .map_err(|_| E2eError::BadHello("sig"))?;
        let shared = agree(self.private, &ephemeral)?;
        let (phone_to_pc, pc_to_phone) = derive_keys(&shared, &self.nonce, &nonce);
        Ok(Session::new(phone_to_pc, pc_to_phone))
    }
}

/// What the computer signs: everything both sides saw, in a fixed order with
/// fixed lengths, so no part can be moved into another. It always starts with
/// `TRANSCRIPT_TAG`, and it is the only thing the identity key signs that
/// does. Anything else signed by this key must start with a tag of its own
/// (the relay claim uses `arakun-relay-claim-v1\0`), or a signature over it
/// could be passed off as a handshake.
fn transcript(
    label: &str,
    channel: u32,
    e_phone: &[u8],
    e_pc: &[u8],
    nonce_phone: &[u8],
    nonce_pc: &[u8],
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(TRANSCRIPT_TAG.len() + label.len() + 4 + 2 * POINT_LEN + 2 * NONCE_LEN);
    out.extend_from_slice(TRANSCRIPT_TAG);
    out.extend_from_slice(label.as_bytes());
    out.extend_from_slice(&channel.to_be_bytes());
    out.extend_from_slice(e_phone);
    out.extend_from_slice(e_pc);
    out.extend_from_slice(nonce_phone);
    out.extend_from_slice(nonce_pc);
    out
}

/// The ECDH secret with the other side's ephemeral key: the x coordinate, 32 bytes.
fn agree(private: agreement::EphemeralPrivateKey, peer: &[u8]) -> Result<Vec<u8>, E2eError> {
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, peer);
    agreement::agree_ephemeral(private, &peer, |secret| secret.to_vec())
        .map_err(|_| E2eError::BadHello("e"))
}

/// The two session keys out of the shared secret: phone to computer, then
/// computer to phone. Salt is both nonces, phone's first.
pub fn derive_keys(
    shared: &[u8],
    nonce_phone: &[u8],
    nonce_pc: &[u8],
) -> ([u8; KEY_LEN], [u8; KEY_LEN]) {
    struct Len(usize);
    impl hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let mut salt = Vec::with_capacity(2 * NONCE_LEN);
    salt.extend_from_slice(nonce_phone);
    salt.extend_from_slice(nonce_pc);
    let mut out = [0u8; 2 * KEY_LEN];
    hkdf::Salt::new(hkdf::HKDF_SHA256, &salt)
        .extract(shared)
        .expand(&[KEYS_INFO], Len(out.len()))
        .expect("64 bytes is within HKDF's reach")
        .fill(&mut out)
        .expect("the buffer is the asked length");
    let mut a = [0u8; KEY_LEN];
    let mut b = [0u8; KEY_LEN];
    a.copy_from_slice(&out[..KEY_LEN]);
    b.copy_from_slice(&out[KEY_LEN..]);
    (a, b)
}

/// One encrypted channel: a key and a counter per direction.
pub struct Session {
    seal_key: aead::LessSafeKey,
    open_key: aead::LessSafeKey,
    sealed: u64,
    opened: u64,
}

impl Session {
    fn new(seal: [u8; KEY_LEN], open: [u8; KEY_LEN]) -> Session {
        let key = |k: &[u8]| {
            aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, k).expect("32 bytes"))
        };
        Session {
            seal_key: key(&seal),
            open_key: key(&open),
            sealed: 0,
            opened: 0,
        }
    }

    /// `plain` as a data frame payload: its number, then the ciphertext and tag.
    pub fn seal(&mut self, plain: &[u8]) -> Vec<u8> {
        let counter = self.sealed;
        self.sealed += 1;
        let mut out = Vec::with_capacity(COUNTER_LEN + plain.len() + aead::AES_256_GCM.tag_len());
        out.extend_from_slice(&counter.to_be_bytes());
        let mut body = plain.to_vec();
        self.seal_key
            .seal_in_place_append_tag(nonce_for(counter), aead::Aad::from([DATA]), &mut body)
            .expect("sealing does not fail");
        out.extend_from_slice(&body);
        out
    }

    /// The plaintext of a data frame payload, which must be the next one.
    pub fn open(&mut self, payload: &[u8]) -> Result<Vec<u8>, E2eError> {
        if payload.len() < COUNTER_LEN {
            return Err(E2eError::Cipher);
        }
        let counter = u64::from_be_bytes(payload[..COUNTER_LEN].try_into().expect("8 bytes"));
        if counter != self.opened {
            return Err(E2eError::Counter);
        }
        let mut body = payload[COUNTER_LEN..].to_vec();
        let plain = self
            .open_key
            .open_in_place(nonce_for(counter), aead::Aad::from([DATA]), &mut body)
            .map_err(|_| E2eError::Cipher)?
            .to_vec();
        self.opened += 1;
        Ok(plain)
    }
}

/// Four zero bytes, then the counter: unique per key as long as the counter is.
fn nonce_for(counter: u64) -> aead::Nonce {
    let mut n = [0u8; aead::NONCE_LEN];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    aead::Nonce::assume_unique_for_key(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL: u32 = 7;

    fn handshake() -> (Identity, Session, Session) {
        let me = Identity::generate();
        let phone = Handshake::begin();
        let hello = PhoneHello::parse(&phone.hello()).unwrap();
        let (reply, pc) = Handshake::begin()
            .answer(&hello, &me, CHANNEL, "test")
            .unwrap();
        let phone = phone.finish(&reply, &me.label(), CHANNEL).unwrap();
        (me, pc, phone)
    }

    #[test]
    fn both_sides_end_up_with_the_same_keys_and_talk() {
        let (_, mut pc, mut phone) = handshake();
        let frame = pc.seal(b"from the computer");
        assert_eq!(frame.len(), 8 + 17 + 16);
        assert_eq!(&frame[..8], &[0; 8]);
        assert_eq!(phone.open(&frame).unwrap(), b"from the computer");
        let back = phone.seal(b"from the phone");
        assert_eq!(pc.open(&back).unwrap(), b"from the phone");
        let second = phone.seal(b"");
        assert_eq!(&second[..8], &1u64.to_be_bytes());
        assert_eq!(pc.open(&second).unwrap(), b"");
    }

    #[test]
    fn a_frame_out_of_order_or_changed_does_not_open() {
        let (_, mut pc, mut phone) = handshake();
        let first = pc.seal(b"one");
        let second = pc.seal(b"two");
        assert_eq!(phone.open(&second), Err(E2eError::Counter), "skipping one");
        assert_eq!(phone.open(&first).unwrap(), b"one");
        assert_eq!(phone.open(&first), Err(E2eError::Counter), "replaying it");
        let mut forged = second.clone();
        forged[12] ^= 1;
        assert_eq!(phone.open(&forged), Err(E2eError::Cipher));
        assert_eq!(
            phone.open(&second).unwrap(),
            b"two",
            "the good one still counts as next"
        );
        assert_eq!(phone.open(&[0u8; 3]), Err(E2eError::Cipher));
    }

    #[test]
    fn the_phone_refuses_a_computer_that_is_not_the_one_it_scanned() {
        let me = Identity::generate();
        let other = Identity::generate();
        let phone = Handshake::begin();
        let hello = PhoneHello::parse(&phone.hello()).unwrap();
        let (reply, _) = Handshake::begin()
            .answer(&hello, &other, CHANNEL, "test")
            .unwrap();
        assert_eq!(
            phone.finish(&reply, &me.label(), CHANNEL).err(),
            Some(E2eError::BadHello("label"))
        );

        // The right key, but signing another channel's exchange.
        let phone = Handshake::begin();
        let hello = PhoneHello::parse(&phone.hello()).unwrap();
        let (reply, _) = Handshake::begin()
            .answer(&hello, &me, CHANNEL + 1, "test")
            .unwrap();
        assert_eq!(
            phone.finish(&reply, &me.label(), CHANNEL).err(),
            Some(E2eError::BadHello("sig"))
        );
    }

    #[test]
    fn a_hello_that_is_not_right_is_named() {
        assert_eq!(
            PhoneHello::parse(b"nope").err(),
            Some(E2eError::BadHello("not json"))
        );
        assert_eq!(
            PhoneHello::parse(br#"{"e":"","nonce":""}"#).err(),
            Some(E2eError::BadHello("v"))
        );
        let short = json!({"v": 1, "e": B64.encode([4u8; 10]), "nonce": B64.encode([0u8; 16])});
        assert_eq!(
            PhoneHello::parse(short.to_string().as_bytes()).err(),
            Some(E2eError::BadHello("e"))
        );
        let bad_point = json!({"v": 1, "e": B64.encode([4u8; 65]), "nonce": B64.encode([0u8; 16])});
        let hello = PhoneHello::parse(bad_point.to_string().as_bytes()).unwrap();
        let me = Identity::generate();
        assert_eq!(
            Handshake::begin().answer(&hello, &me, 1, "t").err(),
            Some(E2eError::BadHello("e"))
        );
        let old = PhoneHello {
            version: 0,
            ..hello
        };
        assert_eq!(
            Handshake::begin().answer(&old, &me, 1, "t").err(),
            Some(E2eError::BadHello("version"))
        );
    }

    /// What `phone/public/e2e.js` is checked against: `WRITE_VECTORS=1 cargo test
    /// phone::e2e::vectors` writes it; the plain run checks Rust still agrees
    /// with it, so a change on either side shows up.
    const VECTORS: &str = "../phone/test/vectors/e2e.json";

    #[test]
    fn vectors() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(VECTORS);
        if std::env::var_os("WRITE_VECTORS").is_some() {
            std::fs::write(&path, write_vectors()).unwrap();
            return;
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let bytes = |name: &str| B64.decode(v[name].as_str().unwrap()).unwrap();
        let label = v["label"].as_str().unwrap();
        let channel = v["channel"].as_u64().unwrap() as u32;
        assert_eq!(label_of(&bytes("pc_pub")), label);

        let hello_pc: Value = serde_json::from_str(v["hello_pc"].as_str().unwrap()).unwrap();
        let signed = transcript(
            label,
            channel,
            &bytes("e_phone"),
            &bytes("e_pc"),
            &bytes("nonce_phone"),
            &bytes("nonce_pc"),
        );
        let sig = B64.decode(hello_pc["sig"].as_str().unwrap()).unwrap();
        signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, bytes("pc_pub"))
            .verify(&signed, &sig)
            .unwrap();
        assert_eq!(
            B64.decode(hello_pc["e"].as_str().unwrap()).unwrap(),
            bytes("e_pc")
        );

        let (phone_to_pc, pc_to_phone) =
            derive_keys(&bytes("shared"), &bytes("nonce_phone"), &bytes("nonce_pc"));
        assert_eq!(phone_to_pc.to_vec(), bytes("key_phone_to_pc"));
        assert_eq!(pc_to_phone.to_vec(), bytes("key_pc_to_phone"));

        let mut pc = Session::new(pc_to_phone, phone_to_pc);
        let mut phone = Session::new(phone_to_pc, pc_to_phone);
        for f in v["frames"].as_array().unwrap() {
            let plain = f["plain"].as_str().unwrap().as_bytes();
            let sealed = B64.decode(f["sealed"].as_str().unwrap()).unwrap();
            match f["from"].as_str().unwrap() {
                "pc" => {
                    assert_eq!(pc.seal(plain), sealed, "the computer seals the same bytes");
                    assert_eq!(phone.open(&sealed).unwrap(), plain);
                }
                _ => {
                    assert_eq!(phone.seal(plain), sealed, "the phone seals the same bytes");
                    assert_eq!(pc.open(&sealed).unwrap(), plain);
                }
            }
        }
    }

    /// A handshake with everything written down, so the JavaScript side can
    /// check the parts it can compute (ephemeral private keys never leave ring).
    fn write_vectors() -> String {
        let me = Identity::generate();
        let phone = Handshake::begin();
        let pc = Handshake::begin();
        let (e_phone, e_pc) = (phone.public.clone(), pc.public.clone());
        let (nonce_phone, nonce_pc) = (phone.nonce, pc.nonce);
        let hello_phone = String::from_utf8(phone.hello()).unwrap();
        let hello = PhoneHello::parse(hello_phone.as_bytes()).unwrap();
        // ring never shows an ephemeral private key, so the secret is taken from
        // the phone's agreement; the computer's, inside `answer`, must equal it.
        let shared = agree(phone.private, &e_pc).unwrap();
        let (reply, mut pc_session) = pc.answer(&hello, &me, CHANNEL, "0.0.0-vectors").unwrap();
        let (key_phone_to_pc, key_pc_to_phone) = derive_keys(&shared, &nonce_phone, &nonce_pc);
        let mut phone_session = Session::new(key_phone_to_pc, key_pc_to_phone);
        let mut frames = Vec::new();
        for (from, plain) in [
            ("phone", r#"{"pair":{"secret":"s3cr3t","name":"Pixel"}}"#),
            ("pc", r#"{"paired":{"id":"ab12","ticket":"t","token":"k"}}"#),
            ("phone", r#"{"type":"ping"}"#),
            ("pc", ""),
        ] {
            let sealed = if from == "pc" {
                pc_session.seal(plain.as_bytes())
            } else {
                phone_session.seal(plain.as_bytes())
            };
            frames.push(json!({ "from": from, "plain": plain, "sealed": B64.encode(sealed) }));
        }
        serde_json::to_string_pretty(&json!({
            "note": "Written by `WRITE_VECTORS=1 cargo test phone::e2e::vectors`; checked by phone/test/e2e.test.mjs.",
            "label": me.label(),
            "channel": CHANNEL,
            "pc_pub": B64.encode(me.public_raw()),
            "e_phone": B64.encode(&e_phone),
            "e_pc": B64.encode(&e_pc),
            "nonce_phone": B64.encode(nonce_phone),
            "nonce_pc": B64.encode(nonce_pc),
            "hello_phone": hello_phone,
            "hello_pc": String::from_utf8(reply).unwrap(),
            "shared": B64.encode(&shared),
            "key_phone_to_pc": B64.encode(key_phone_to_pc),
            "key_pc_to_phone": B64.encode(key_pc_to_phone),
            "frames": frames,
        }))
        .unwrap()
    }

    #[test]
    fn a_claim_signature_is_never_a_transcript_signature() {
        let me = Identity::generate();
        let label = me.label();
        let e = [4u8; POINT_LEN];
        let n = [9u8; NONCE_LEN];
        let handshake = transcript(&label, 7, &e, &e, &n, &n);
        // The nonce a relay would need us to sign so that the claim reads as
        // a transcript ending in our label: refused outright.
        let forged_nonce = &handshake[..handshake.len() - label.len()];
        assert_eq!(
            super::super::relay::claim_message(forged_nonce, &label),
            None
        );
        // Any claim we do sign starts with another tag.
        let claim = super::super::relay::claim_message(&n, &label).unwrap();
        assert!(handshake.starts_with(TRANSCRIPT_TAG));
        assert!(!claim.starts_with(TRANSCRIPT_TAG));
        assert_ne!(claim.len(), handshake.len());
        // So a claim signature does not verify as a handshake.
        let sig = me.sign(&claim);
        let key =
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, me.public_raw());
        assert!(key.verify(&claim, &sig).is_ok());
        assert!(key.verify(&handshake, &sig).is_err());
    }
}
