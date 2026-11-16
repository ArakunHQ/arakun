//! Who this computer is to the relay: one P-256 key pair, made once and kept in
//! the phone settings. Its label, the first level of the host phones reach it
//! at, is derived from the public key, so nobody can claim a label that is not
//! theirs and a phone can check it is talking to the key it expects. The relay
//! challenges the computer to sign with this key before trusting it, and the
//! phone checks the same key signs the hello of the end-to-end handshake.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::signature::KeyPair as _;
use ring::{rand, signature};
use sha2::Digest as _;

/// Base32 alphabet of RFC 4648, in lower case: what a host name may carry.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
/// How much of the hash the label keeps: 80 bits, 16 characters.
const LABEL_BYTES: usize = 10;

pub struct Identity {
    pair: signature::EcdsaKeyPair,
    pkcs8: Vec<u8>,
}

impl Identity {
    pub fn generate() -> Identity {
        let rng = rand::SystemRandom::new();
        let doc = signature::EcdsaKeyPair::generate_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &rng,
        )
        .expect("ring can make a P-256 key");
        Identity::from_pkcs8(doc.as_ref()).expect("a key ring just made parses")
    }

    pub fn from_pkcs8(pkcs8: &[u8]) -> Option<Identity> {
        let rng = rand::SystemRandom::new();
        let pair = signature::EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8,
            &rng,
        )
        .ok()?;
        Some(Identity {
            pair,
            pkcs8: pkcs8.to_vec(),
        })
    }

    /// The pair as the settings keep it.
    pub fn pkcs8_b64(&self) -> String {
        B64.encode(&self.pkcs8)
    }

    /// The public key as the uncompressed point: 0x04 then x and y, 65 bytes.
    pub fn public_raw(&self) -> &[u8] {
        self.pair.public_key().as_ref()
    }

    pub fn label(&self) -> String {
        label_of(self.public_raw())
    }

    /// An ECDSA signature over `data`: r then s, 32 bytes each, as WebCrypto makes and checks them.
    #[cfg_attr(not(test), allow(dead_code))] // the relay client claims the label with it
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        self.pair
            .sign(&rand::SystemRandom::new(), data)
            .expect("signing does not fail")
            .as_ref()
            .to_vec()
    }
}

/// The label a public key derives: base32 of the first 80 bits of its SHA-256.
pub fn label_of(public_raw: &[u8]) -> String {
    let digest = sha2::Sha256::digest(public_raw);
    base32(&digest[..LABEL_BYTES])
}

/// Sixteen lower-case base32 characters: what a label looks like.
#[cfg_attr(not(test), allow(dead_code))] // the relay client checks the host it dials with it
pub fn is_label(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| ALPHABET.contains(&b))
}

/// RFC 4648 without padding; 10 bytes come out as 16 characters exactly.
fn base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            out.push(ALPHABET[((acc >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vector the Worker's tests use too: both sides must agree on it.
    const VECTOR: &str = include_str!("../../phone/test/vectors/label.json");

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn the_label_matches_the_shared_vector() {
        let v: serde_json::Value = serde_json::from_str(VECTOR).unwrap();
        let public = unhex(v["public_key_hex"].as_str().unwrap());
        assert_eq!(public.len(), 65);
        assert_eq!(
            // sha2 0.11's digest is a hybrid-array, which doesn't format as hex.
            sha2::Sha256::digest(&public)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            v["sha256_hex"].as_str().unwrap()
        );
        assert_eq!(label_of(&public), v["label"].as_str().unwrap());
    }

    #[test]
    fn base32_is_rfc_4648_in_lower_case() {
        assert_eq!(base32(&[0; 10]), "aaaaaaaaaaaaaaaa");
        assert_eq!(base32(&[0xff; 5]), "77777777");
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
    }

    #[test]
    fn a_fresh_identity_has_a_label_and_signs_verifiably() {
        let me = Identity::generate();
        assert_eq!(me.public_raw().len(), 65);
        assert_eq!(me.public_raw()[0], 0x04);
        assert!(is_label(&me.label()), "{}", me.label());
        assert_eq!(me.label(), label_of(me.public_raw()));

        let sig = me.sign(b"nonce and label");
        assert_eq!(sig.len(), 64, "fixed r||s, as WebCrypto expects");
        let public =
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, me.public_raw());
        assert!(public.verify(b"nonce and label", &sig).is_ok());
        assert!(public.verify(b"something else", &sig).is_err());
    }

    #[test]
    fn the_identity_survives_the_settings_round_trip() {
        let me = Identity::generate();
        let again = Identity::from_pkcs8(&B64.decode(me.pkcs8_b64()).unwrap()).unwrap();
        assert_eq!(again.public_raw(), me.public_raw());
        assert_eq!(again.label(), me.label());
        assert!(Identity::from_pkcs8(b"not a key").is_none());
    }

    #[test]
    fn labels_have_one_shape() {
        assert!(is_label("5pmbdp76s6bol6ax"));
        assert!(!is_label("5pmbdp76s6bol6a"));
        assert!(!is_label("5PMBDP76S6BOL6AX"));
        assert!(!is_label("5pmbdp76s6bol6a1"));
    }
}
