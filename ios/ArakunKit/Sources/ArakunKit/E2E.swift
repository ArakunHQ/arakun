// The phone's half of the end-to-end encryption with the computer through the
// relay, as phone/e2e.js does it (core/src/phone/e2e.rs is the computer's):
// P-256 ECDH for the shared secret, the computer's P-256 ECDSA signature over
// the whole handshake, HKDF-SHA256 for the two session keys, AES-256-GCM for
// the frames. Tests check it against phone/test/vectors.
import CryptoKit
import Foundation

public enum E2E {
    public static let version = 1
    static let transcriptTag = "arakun-phone-v1"
    static let keysInfo = "arakun-phone-v1 keys"

    public enum Failure: Error, Equatable, CustomStringConvertible {
        case badHello(String)
        case outOfOrder
        case doesNotDecrypt

        public var description: String {
            switch self {
            case .badHello(let why): "bad hello: \(why)"
            case .outOfOrder: "frame out of order"
            case .doesNotDecrypt: "frame does not decrypt"
            }
        }
    }

    /// The label a computer's public key (65 bytes, uncompressed) derives.
    public static func label(of publicKey: Data) -> String {
        Base32.encode(SHA256.hash(data: publicKey).prefix(10))
    }

    /// What the computer signs: tag, label, channel (u32 big-endian), both
    /// ephemeral keys, both nonces; the phone's before the computer's.
    static func transcript(label: String, channel: UInt32, ePhone: Data, ePc: Data, noncePhone: Data, noncePc: Data) -> Data {
        var d = Data(transcriptTag.utf8)
        d.append(Data(label.utf8))
        d.append(Data(bigEndian: channel))
        d.append(ePhone)
        d.append(ePc)
        d.append(noncePhone)
        d.append(noncePc)
        return d
    }

    /// The parts of the computer's hello, once it is trusted.
    public struct PcHello: Equatable {
        public let pub: Data
        public let e: Data
        public let nonce: Data
        public let arakun: String
    }

    /// Checks the computer's hello answers ours and comes from the key `label`
    /// names.
    public static func verifyPc(_ hello: [String: Any], label: String, channel: UInt32, ePhone: Data, noncePhone: Data) throws -> PcHello {
        guard (hello["v"] as? Int) == version else { throw Failure.badHello("version") }
        func field(_ name: String, _ len: Int) throws -> Data {
            guard let s = hello[name] as? String, let d = Base64URL.decode(s), d.count == len else {
                throw Failure.badHello(name)
            }
            return d
        }
        let pub = try field("pub", 65)
        guard self.label(of: pub) == label else { throw Failure.badHello("label") }
        let e = try field("e", 65)
        let nonce = try field("nonce", 16)
        let sig = try field("sig", 64)
        let signed = transcript(label: label, channel: channel, ePhone: ePhone, ePc: e, noncePhone: noncePhone, noncePc: nonce)
        guard let key = try? P256.Signing.PublicKey(x963Representation: pub),
              let signature = try? P256.Signing.ECDSASignature(rawRepresentation: sig),
              key.isValidSignature(signature, for: signed)
        else { throw Failure.badHello("sig") }
        return PcHello(pub: pub, e: e, nonce: nonce, arakun: hello["arakun"] as? String ?? "")
    }

    /// The two session keys out of the shared secret: phone to computer, then
    /// computer to phone. The salt is both nonces, the phone's first.
    public static func deriveKeys(shared: Data, noncePhone: Data, noncePc: Data) -> (phoneToPc: Data, pcToPhone: Data) {
        let bits = HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: shared),
            salt: noncePhone + noncePc,
            info: Data(keysInfo.utf8),
            outputByteCount: 64
        ).withUnsafeBytes { Data($0) }
        return (bits.prefix(32), bits.suffix(32))
    }

    /// One encrypted channel: the key this side seals with, the key it opens
    /// with, and a counter for each. Frames must be sealed and opened in order.
    public final class Session {
        private let sealKey: SymmetricKey
        private let openKey: SymmetricKey
        private var sealed: UInt64 = 0
        private var opened: UInt64 = 0
        private let aad = Data([Frame.Kind.data.rawValue])
        public var arakun = ""

        public init(sealKey: Data, openKey: Data) {
            self.sealKey = SymmetricKey(data: sealKey)
            self.openKey = SymmetricKey(data: openKey)
        }

        /// Four zero bytes, then the counter as u64 big-endian.
        static func nonce(_ counter: UInt64) -> AES.GCM.Nonce {
            // A 12-byte nonce is always accepted.
            try! AES.GCM.Nonce(data: Data(count: 4) + Data(bigEndian: counter))
        }

        /// `plain` as a data frame payload: its number, then the ciphertext and tag.
        public func seal(_ plain: Data) throws -> Data {
            let counter = sealed
            sealed += 1
            let box = try AES.GCM.seal(plain, using: sealKey, nonce: Self.nonce(counter), authenticating: aad)
            return Data(bigEndian: counter) + box.ciphertext + box.tag
        }

        /// The plaintext of a data frame payload, which must be the next one.
        public func open(_ payload: Data) throws -> Data {
            guard payload.count >= 8 + 16, let counter = payload.readBigEndian(UInt64.self) else {
                throw Failure.doesNotDecrypt
            }
            guard counter == opened else { throw Failure.outOfOrder }
            let body = payload.dropFirst(8)
            do {
                let box = try AES.GCM.SealedBox(nonce: Self.nonce(opened), ciphertext: body.dropLast(16), tag: body.suffix(16))
                let plain = try AES.GCM.open(box, using: openKey, authenticating: aad)
                opened += 1
                return plain
            } catch {
                throw Failure.doesNotDecrypt
            }
        }
    }

    /// A handshake under way: `hello` goes to the computer, and `finish` takes
    /// its answer and gives the session, or throws why it is not trusted.
    public struct Handshake {
        let key: P256.KeyAgreement.PrivateKey
        let nonce: Data
        public let hello: Data

        public init() {
            key = P256.KeyAgreement.PrivateKey()
            var n = Data(count: 16)
            n.withUnsafeMutableBytes { _ = SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!) }
            nonce = n
            let e = key.publicKey.x963Representation
            let obj: [String: Any] = ["v": E2E.version, "e": Base64URL.encode(e), "nonce": Base64URL.encode(nonce)]
            hello = try! JSONSerialization.data(withJSONObject: obj)
        }

        public func finish(_ reply: Data, label: String, channel: UInt32) throws -> Session {
            guard let obj = try? JSONSerialization.jsonObject(with: reply) as? [String: Any] else {
                throw Failure.badHello("not json")
            }
            let pc = try verifyPc(obj, label: label, channel: channel, ePhone: key.publicKey.x963Representation, noncePhone: nonce)
            guard let theirs = try? P256.KeyAgreement.PublicKey(x963Representation: pc.e),
                  let shared = try? key.sharedSecretFromKeyAgreement(with: theirs)
            else { throw Failure.badHello("e") }
            let keys = deriveKeys(shared: shared.withUnsafeBytes { Data($0) }, noncePhone: nonce, noncePc: pc.nonce)
            let session = Session(sealKey: keys.phoneToPc, openKey: keys.pcToPhone)
            session.arakun = pc.arakun
            return session
        }
    }
}

/// A frame between the phone and the relay: the type byte, then the payload.
public struct Frame: Equatable {
    public enum Kind: UInt8 {
        case hello = 0x01
        case data = 0x02
        case control = 0x03
    }

    public let kind: Kind
    public let payload: Data

    public init(_ kind: Kind, _ payload: Data) {
        self.kind = kind
        self.payload = payload
    }

    public init?(decoding bytes: Data) {
        guard let first = bytes.first, let kind = Kind(rawValue: first) else { return nil }
        self.kind = kind
        payload = bytes.dropFirst()
    }

    public var encoded: Data { Data([kind.rawValue]) + payload }
}
