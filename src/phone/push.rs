//! Web Push to the phones: how the computer tells a phone something while the
//! page is closed. The phone's browser hands us a subscription (an endpoint at
//! its push service and two keys); we encrypt each message for that phone
//! (RFC 8291, aes128gcm) and sign the request as this computer (RFC 8292,
//! VAPID), and the service wakes the page's service worker with it. The
//! service sees only ciphertext. Everything here is `ring`, which rustls
//! already brings.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::signature::KeyPair as _;
use ring::{aead, agreement, hkdf, rand, signature};
use serde::{Deserialize, Serialize};

/// How long the service keeps a message for a phone that is off.
const TTL_SECS: u32 = 60 * 60;
/// How long a signed request stays good (the services allow a day).
const TOKEN_FOR: u64 = 12 * 60 * 60;
/// Who to write to about this sender, as VAPID asks.
const CONTACT: &str = "https://arakun.dev";
/// One record carries the whole message; the services take up to 4 KB.
const RECORD_SIZE: u32 = 4096;
const MAX_PAYLOAD: usize = 3800;

/// Where a phone's browser takes messages for it, and the keys to seal them with.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Subscription {
    pub endpoint: String,
    /// The phone's P-256 public key, base64url.
    pub p256dh: String,
    /// The phone's 16-byte auth secret, base64url.
    pub auth: String,
}

impl Subscription {
    /// What `PushSubscription.toJSON()` gives in the browser.
    pub fn from_browser(v: &serde_json::Value) -> Option<Subscription> {
        let endpoint = v["endpoint"].as_str()?.to_string();
        if !endpoint.starts_with("https://") {
            return None;
        }
        let keys = &v["keys"];
        let sub = Subscription { endpoint, p256dh: keys["p256dh"].as_str()?.into(), auth: keys["auth"].as_str()?.into() };
        let (public, auth) = (B64.decode(&sub.p256dh).ok()?, B64.decode(&sub.auth).ok()?);
        (public.len() == 65 && auth.len() == 16).then_some(sub)
    }
}

/// The key pair this computer signs its push requests with. One per computer,
/// kept in the phone settings: the phone's browser binds its subscription to
/// the public half, so a new pair would orphan every subscription.
pub struct Vapid {
    pair: signature::EcdsaKeyPair,
    pkcs8: Vec<u8>,
}

impl Vapid {
    pub fn generate() -> Vapid {
        let rng = rand::SystemRandom::new();
        let doc = signature::EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .expect("ring can make a P-256 key");
        Vapid::from_pkcs8(doc.as_ref()).expect("a key ring just made parses")
    }

    pub fn from_pkcs8(pkcs8: &[u8]) -> Option<Vapid> {
        let rng = rand::SystemRandom::new();
        let pair = signature::EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, &rng).ok()?;
        Some(Vapid { pair, pkcs8: pkcs8.to_vec() })
    }

    /// The pair as the settings keep it.
    pub fn pkcs8_b64(&self) -> String {
        B64.encode(&self.pkcs8)
    }

    /// The public key the phone subscribes with (`applicationServerKey`), base64url.
    pub fn public_key(&self) -> String {
        B64.encode(self.pair.public_key().as_ref())
    }

    /// `Authorization` for a request to `endpoint`: a token for its service, and our key.
    fn authorization(&self, endpoint: &str, now: u64) -> Option<String> {
        let aud = audience(endpoint)?;
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(serde_json::json!({ "aud": aud, "exp": now + TOKEN_FOR, "sub": CONTACT }).to_string());
        let signing = format!("{header}.{claims}");
        let sig = self.pair.sign(&rand::SystemRandom::new(), signing.as_bytes()).ok()?;
        Some(format!("vapid t={signing}.{}, k={}", B64.encode(sig.as_ref()), self.public_key()))
    }
}

/// The push service's origin, which a token is made out to.
fn audience(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.split_once("://")?;
    let host = rest.split('/').next().filter(|h| !h.is_empty())?;
    Some(format!("{scheme}://{host}"))
}

/// Why a message did not get through.
#[derive(Debug, PartialEq, Eq)]
pub enum Fail {
    /// The subscription no longer exists: the phone unsubscribed or the browser dropped it.
    Gone,
    Other(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Gone => write!(f, "the subscription is gone"),
            Fail::Other(s) => f.write_str(s),
        }
    }
}

/// Sends `payload` to one phone. Blocks on the network: for a background thread.
pub fn send(vapid: &Vapid, sub: &Subscription, payload: &[u8], now: u64) -> Result<(), Fail> {
    let body = encrypt(sub, payload).ok_or_else(|| Fail::Other("the subscription's keys are malformed".into()))?;
    let auth = vapid.authorization(&sub.endpoint, now).ok_or_else(|| Fail::Other("could not sign the request".into()))?;
    let request = ureq::http::Request::builder()
        .method("POST")
        .uri(&sub.endpoint)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("TTL", TTL_SECS.to_string())
        .header("Urgency", "high")
        .header("Authorization", auth)
        .body(body)
        .map_err(|e| Fail::Other(e.to_string()))?;
    let mut response = agent().run(request).map_err(|e| Fail::Other(e.to_string()))?;
    match response.status().as_u16() {
        200..=202 => Ok(()),
        404 | 410 => Err(Fail::Gone),
        status => {
            let text = response.body_mut().with_config().limit(4096).read_to_string().unwrap_or_default();
            Err(Fail::Other(format!("the push service answered {status}: {}", text.trim())))
        }
    }
}

fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(20)))
            .http_status_as_error(false)
            .user_agent("arakun")
            .build()
            .into()
    })
}

/// `payload` sealed for the phone as RFC 8291 lays it out: the header (salt,
/// record size, our public key) and one record, the message plus a delimiter.
fn encrypt(sub: &Subscription, payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    let ua_public = B64.decode(&sub.p256dh).ok()?;
    let auth = B64.decode(&sub.auth).ok()?;
    if ua_public.len() != 65 || auth.len() != 16 {
        return None;
    }
    let rng = rand::SystemRandom::new();
    let as_private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).ok()?;
    let as_public = as_private.compute_public_key().ok()?;
    let mut salt = [0u8; 16];
    rand::SecureRandom::fill(&rng, &mut salt).ok()?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &ua_public);
    let (cek, nonce) = agreement::agree_ephemeral(as_private, &peer, |shared| {
        derive(shared, &auth, &ua_public, as_public.as_ref(), &salt)
    })
    .ok()?;

    let mut record = payload.to_vec();
    // The last (only) record ends with 2; a 1 would mean more follow.
    record.push(2);
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).ok()?);
    key.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::empty(), &mut record).ok()?;

    let mut out = Vec::with_capacity(16 + 4 + 1 + 65 + record.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    out.push(65);
    out.extend_from_slice(as_public.as_ref());
    out.extend_from_slice(&record);
    Some(out)
}

/// The content key and nonce both sides reach from the shared secret (RFC 8291 §3).
fn derive(shared: &[u8], auth: &[u8], ua_public: &[u8], as_public: &[u8], salt: &[u8]) -> ([u8; 16], [u8; 12]) {
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    expand(&hkdf::Salt::new(hkdf::HKDF_SHA256, auth).extract(shared), &info, &mut ikm);
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let mut cek = [0u8; 16];
    expand(&prk, b"Content-Encoding: aes128gcm\0", &mut cek);
    let mut nonce = [0u8; 12];
    expand(&prk, b"Content-Encoding: nonce\0", &mut nonce);
    (cek, nonce)
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn expand(prk: &hkdf::Prk, info: &[u8], out: &mut [u8]) {
    prk.expand(&[info], Len(out.len())).expect("short outputs").fill(out).expect("the length matches");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phone's side: its key pair and auth secret, and how it opens a message.
    struct Phone {
        private: agreement::EphemeralPrivateKey,
        public: Vec<u8>,
        auth: [u8; 16],
    }

    impl Phone {
        fn new() -> Phone {
            let rng = rand::SystemRandom::new();
            let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
            let public = private.compute_public_key().unwrap().as_ref().to_vec();
            let mut auth = [0u8; 16];
            rand::SecureRandom::fill(&rng, &mut auth).unwrap();
            Phone { private, public, auth }
        }

        fn subscription(&self) -> Subscription {
            Subscription {
                endpoint: "https://fcm.googleapis.com/fcm/send/abc".into(),
                p256dh: B64.encode(&self.public),
                auth: B64.encode(self.auth),
            }
        }

        fn open(self, body: &[u8]) -> Vec<u8> {
            let (salt, rest) = body.split_at(16);
            let (rs, rest) = rest.split_at(4);
            assert_eq!(u32::from_be_bytes(rs.try_into().unwrap()), RECORD_SIZE);
            assert_eq!(rest[0], 65);
            let (as_public, record) = rest[1..].split_at(65);
            let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public);
            let (cek, nonce) = agreement::agree_ephemeral(self.private, &peer, |shared| {
                derive(shared, &self.auth, &self.public, as_public, salt)
            })
            .unwrap();
            let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
            let mut record = record.to_vec();
            let plain = key.open_in_place(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::empty(), &mut record).unwrap();
            assert_eq!(plain.last(), Some(&2));
            plain[..plain.len() - 1].to_vec()
        }
    }

    #[test]
    fn a_message_sealed_for_a_phone_opens_on_it() {
        let phone = Phone::new();
        let sub = phone.subscription();
        let body = encrypt(&sub, br#"{"kind":"input","title":"Fix the build"}"#).unwrap();
        assert_eq!(phone.open(&body), br#"{"kind":"input","title":"Fix the build"}"#);

        assert!(encrypt(&sub, &[0u8; MAX_PAYLOAD + 1]).is_none());
        let short = Subscription { auth: "c2hvcnQ".into(), ..sub };
        assert!(encrypt(&short, b"x").is_none(), "malformed keys are refused");
    }

    #[test]
    fn the_request_is_signed_as_this_computer() {
        let vapid = Vapid::generate();
        let again = Vapid::from_pkcs8(&B64.decode(vapid.pkcs8_b64()).unwrap()).unwrap();
        assert_eq!(again.public_key(), vapid.public_key());
        assert_eq!(B64.decode(vapid.public_key()).unwrap().len(), 65);

        let auth = vapid.authorization("https://fcm.googleapis.com/fcm/send/abc", 1_700_000_000).unwrap();
        let (t, k) = auth.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
        assert_eq!(k, vapid.public_key());
        let mut parts = t.split('.');
        let (header, claims_b64, sig) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
        let claims: serde_json::Value = serde_json::from_slice(&B64.decode(claims_b64).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["exp"], 1_700_000_000 + TOKEN_FOR);
        assert_eq!(claims["sub"], CONTACT);
        let public = signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, B64.decode(k).unwrap());
        public.verify(format!("{header}.{claims_b64}").as_bytes(), &B64.decode(sig).unwrap()).unwrap();
    }

    /// A push service of our own: answers each request with the next status
    /// given, and keeps what it got.
    fn service(statuses: &'static [u16]) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/wpush/abc", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for status in statuses {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let want: usize = head
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                    .unwrap();
                let have = n - head.split("\r\n\r\n").next().unwrap().len() - 4;
                let mut rest = vec![0u8; want - have];
                s.read_exact(&mut rest).unwrap();
                tx.send(format!("{head}<{want} bytes>")).unwrap();
                write!(s, "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
        });
        (endpoint, rx)
    }

    #[test]
    fn a_notice_goes_out_signed_and_sealed_and_a_gone_subscription_is_told_apart() {
        let (endpoint, got) = service(&[201, 410, 500]);
        let vapid = Vapid::generate();
        let phone = Phone::new();
        let sub = Subscription { endpoint, ..phone.subscription() };
        assert_eq!(send(&vapid, &sub, b"{\"kind\":\"done\"}", 1_700_000_000), Ok(()));
        let req = got.recv().unwrap();
        assert!(req.starts_with("POST /wpush/abc HTTP/1.1"), "{req}");
        assert!(req.contains("content-encoding: aes128gcm") || req.contains("Content-Encoding: aes128gcm"), "{req}");
        assert!(req.contains("ttl: 3600") || req.contains("TTL: 3600"), "{req}");
        assert!(req.contains("vapid t=") && req.contains(&format!(", k={}", vapid.public_key())), "{req}");
        // Header (86 bytes) + the message, its delimiter and the 16-byte tag.
        assert!(req.ends_with(&format!("<{} bytes>", 86 + 15 + 1 + 16)), "{req}");

        assert_eq!(send(&vapid, &sub, b"x", 1_700_000_000), Err(Fail::Gone));
        got.recv().unwrap();
        assert!(matches!(send(&vapid, &sub, b"x", 1_700_000_000), Err(Fail::Other(e)) if e.contains("500")));
    }

    #[test]
    fn a_browser_subscription_is_read_and_checked() {
        let phone = Phone::new();
        let good = serde_json::json!({
            "endpoint": "https://web.push.apple.com/abc", "expirationTime": null,
            "keys": { "p256dh": B64.encode(&phone.public), "auth": B64.encode(phone.auth) },
        });
        let sub = Subscription::from_browser(&good).unwrap();
        assert_eq!(sub.endpoint, "https://web.push.apple.com/abc");
        let mut bad = good.clone();
        bad["keys"]["auth"] = serde_json::json!("c2hvcnQ");
        assert!(Subscription::from_browser(&bad).is_none());
        let mut plain = good.clone();
        plain["endpoint"] = serde_json::json!("http://push.example/abc");
        assert!(Subscription::from_browser(&plain).is_none());
        assert_eq!(audience("https://updates.push.services.mozilla.com/wpush/v2/x"), Some("https://updates.push.services.mozilla.com".into()));
    }

    /// Rings every phone paired with this computer, skipping the window and
    /// session checks, to tell a delivery problem from a trigger problem:
    /// `cargo test phone::push::tests::ring_the_real_phones -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn ring_the_real_phones() {
        let mut settings = super::super::Settings::load();
        let vapid = settings.vapid();
        let payload = serde_json::json!({ "kind": "input", "session": "probe", "title": "Arakun", "body": "Test push from the computer." });
        let mut rang = 0;
        for device in settings.devices.iter().filter(|d| d.push.is_some()) {
            let sub = device.push.as_ref().unwrap();
            let outcome = send(&vapid, sub, payload.to_string().as_bytes(), crate::model::unix_now());
            eprintln!("{} ({}): {:?}", device.name, &sub.endpoint[..sub.endpoint.len().min(48)], outcome);
            rang += outcome.is_ok() as usize;
        }
        assert!(rang > 0, "no phone with a push subscription took the message");
    }
}
