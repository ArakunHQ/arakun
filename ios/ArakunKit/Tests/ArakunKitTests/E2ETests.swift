import Foundation
import Testing
@testable import ArakunKit

/// The vectors core/src/phone/e2e.rs writes, which phone/e2e.js is checked against too.
private let vectorsDir = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .deletingLastPathComponent().deletingLastPathComponent()
    .appending(path: "phone/test/vectors")

private func load(_ name: String) throws -> [String: Any] {
    let data = try Data(contentsOf: vectorsDir.appending(path: name))
    return try JSONSerialization.jsonObject(with: data) as! [String: Any]
}

private func hex(_ s: String) -> Data {
    var d = Data()
    var i = s.startIndex
    while i < s.endIndex {
        let j = s.index(i, offsetBy: 2)
        d.append(UInt8(s[i..<j], radix: 16)!)
        i = j
    }
    return d
}

struct E2ETests {
    let v: [String: Any]
    init() throws { v = try load("e2e.json") }
    func bytes(_ name: String) -> Data { Base64URL.decode(v[name] as! String)! }
    var label: String { v["label"] as! String }
    var channel: UInt32 { UInt32(v["channel"] as! Int) }

    @Test func labelMatchesRustAndTheWorker() throws {
        let l = try load("label.json")
        #expect(E2E.label(of: hex(l["public_key_hex"] as! String)) == l["label"] as! String)
        #expect(E2E.label(of: bytes("pc_pub")) == label)
    }

    @Test func trustsTheComputersHello() throws {
        let hello = try JSONSerialization.jsonObject(with: Data((v["hello_pc"] as! String).utf8)) as! [String: Any]
        let pc = try E2E.verifyPc(hello, label: label, channel: channel, ePhone: bytes("e_phone"), noncePhone: bytes("nonce_phone"))
        #expect(pc.e == bytes("e_pc"))
        #expect(pc.nonce == bytes("nonce_pc"))
        #expect(pc.arakun == "0.0.0-vectors")

        func rejects(_ h: [String: Any], label: String? = nil, channel: UInt32? = nil, ePhone: String = "e_phone", noncePhone: String = "nonce_phone", _ why: String) {
            #expect(throws: E2E.Failure.badHello(why)) {
                try E2E.verifyPc(h, label: label ?? self.label, channel: channel ?? self.channel, ePhone: bytes(ePhone), noncePhone: bytes(noncePhone))
            }
        }
        rejects(hello, label: "aaaaaaaaaaaaaaaa", "label")
        rejects(hello, channel: channel + 1, "sig")
        rejects(hello, ePhone: "e_pc", "sig")
        rejects(hello, noncePhone: "nonce_pc", "sig")
        var v2 = hello
        v2["v"] = 2
        rejects(v2, "version")
        var forged = hello
        var sig = Base64URL.decode(hello["sig"] as! String)!
        sig[3] ^= 1
        forged["sig"] = Base64URL.encode(sig)
        rejects(forged, "sig")
    }

    @Test func derivesTheSameKeys() {
        let keys = E2E.deriveKeys(shared: bytes("shared"), noncePhone: bytes("nonce_phone"), noncePc: bytes("nonce_pc"))
        #expect(keys.phoneToPc == bytes("key_phone_to_pc"))
        #expect(keys.pcToPhone == bytes("key_pc_to_phone"))
    }

    @Test func framesSealToTheSameBytes() throws {
        let phone = E2E.Session(sealKey: bytes("key_phone_to_pc"), openKey: bytes("key_pc_to_phone"))
        let pc = E2E.Session(sealKey: bytes("key_pc_to_phone"), openKey: bytes("key_phone_to_pc"))
        for f in v["frames"] as! [[String: Any]] {
            let sealed = Base64URL.decode(f["sealed"] as! String)!
            let plain = Data((f["plain"] as! String).utf8)
            let (from, to) = f["from"] as! String == "pc" ? (pc, phone) : (phone, pc)
            #expect(try from.seal(plain) == sealed)
            #expect(try to.open(sealed) == plain)
        }
    }

    @Test func framesOutOfOrderOrChangedDoNotOpen() throws {
        let a = E2E.Session(sealKey: bytes("key_pc_to_phone"), openKey: bytes("key_phone_to_pc"))
        let b = E2E.Session(sealKey: bytes("key_phone_to_pc"), openKey: bytes("key_pc_to_phone"))
        let first = try a.seal(Data("one".utf8))
        let second = try a.seal(Data("two".utf8))
        #expect(throws: E2E.Failure.outOfOrder) { try b.open(second) }
        #expect(try b.open(first) == Data("one".utf8))
        #expect(throws: E2E.Failure.outOfOrder) { try b.open(first) }
        var forged = second
        forged[forged.startIndex + 12] ^= 1
        #expect(throws: E2E.Failure.doesNotDecrypt) { try b.open(forged) }
        #expect(try b.open(second) == Data("two".utf8))
        #expect(throws: E2E.Failure.doesNotDecrypt) { try b.open(Data(count: 3)) }
    }

    @Test func handshakeHelloIsWellFormed() throws {
        let h = E2E.Handshake()
        let obj = try JSONSerialization.jsonObject(with: h.hello) as! [String: Any]
        #expect(obj["v"] as? Int == 1)
        #expect(Base64URL.decode(obj["e"] as! String)?.count == 65)
        #expect(Base64URL.decode(obj["nonce"] as! String)?.count == 16)
    }
}

struct PartTests {
    @Test func joinsPiecesBackIntoTheMessage() throws {
        let v = try load("part.json")
        for c in v["cases"] as! [[String: Any]] {
            var j = Joiner()
            var whole: Data?
            for p in c["parts"] as! [String] {
                let obj = try JSONSerialization.jsonObject(with: Data(p.utf8)) as! [String: Any]
                whole = j.add(obj["part"] as! [String: Any])
            }
            #expect(whole.map { String(decoding: $0, as: UTF8.self) } == v["message"] as? String)
        }
    }
}

struct PairingTests {
    @Test func readsTheRelayLink() {
        let l = PairingLink("https://lmx5lt63vsxoyes4.arakun.dev/#p=abc123&s=s%2Fecret")
        #expect(l?.label == "lmx5lt63vsxoyes4")
        #expect(l?.host == "lmx5lt63vsxoyes4.arakun.dev")
        #expect(l?.code == "abc123")
        #expect(l?.secret == "s/ecret")
        #expect(l?.secure == true)
        #expect(PairingLink("http://lab.localhost:8787/#p=a&s=b")?.host == "lab.localhost:8787")
        #expect(PairingLink("https://x.arakun.dev/") == nil)
        #expect(PairingLink("https://x.arakun.dev/#p=a") == nil)
        #expect(PairingLink("not a link") == nil)
    }

    @Test func endKinds() {
        #expect(EndKind.of(reason: "", code: 4401) == .unpaired)
        #expect(EndKind.of(reason: "bad_code", code: 1000) == .pairing)
        #expect(EndKind.of(reason: "bad_hello", code: 1000) == .untrusted)
        #expect(EndKind.of(reason: "limit:daily", code: 1000) == .limit)
        #expect(EndKind.of(reason: "", code: 1006) == .gone)
    }
}

import CryptoKit

struct HandshakeTests {
    /// The computer's side played here, as core/src/phone/e2e.rs answers a hello.
    @Test func agreesWithAComputerOnTheKeys() throws {
        let identity = P256.Signing.PrivateKey()
        let pub = identity.publicKey.x963Representation
        let label = E2E.label(of: pub)
        let channel: UInt32 = 3
        let phone = E2E.Handshake()
        let hello = try JSONSerialization.jsonObject(with: phone.hello) as! [String: Any]
        let ePhone = Base64URL.decode(hello["e"] as! String)!
        let noncePhone = Base64URL.decode(hello["nonce"] as! String)!

        let eph = P256.KeyAgreement.PrivateKey()
        let ePc = eph.publicKey.x963Representation
        let noncePc = Data((0..<16).map { _ in UInt8.random(in: 0...255) })
        let signed = E2E.transcript(label: label, channel: channel, ePhone: ePhone, ePc: ePc, noncePhone: noncePhone, noncePc: noncePc)
        let sig = try identity.signature(for: signed).rawRepresentation
        let reply: [String: Any] = ["v": 1, "pub": Base64URL.encode(pub), "e": Base64URL.encode(ePc),
                                    "nonce": Base64URL.encode(noncePc), "sig": Base64URL.encode(sig), "arakun": "1.2.3"]
        let session = try phone.finish(try JSONSerialization.data(withJSONObject: reply), label: label, channel: channel)
        #expect(session.arakun == "1.2.3")
        #expect(throws: E2E.Failure.badHello("sig")) {
            try phone.finish(try JSONSerialization.data(withJSONObject: reply), label: label, channel: channel + 1)
        }

        let shared = try eph.sharedSecretFromKeyAgreement(with: P256.KeyAgreement.PublicKey(x963Representation: ePhone))
        let keys = E2E.deriveKeys(shared: shared.withUnsafeBytes { Data($0) }, noncePhone: noncePhone, noncePc: noncePc)
        let pc = E2E.Session(sealKey: keys.pcToPhone, openKey: keys.phoneToPc)
        #expect(try pc.open(session.seal(Data("hi".utf8))) == Data("hi".utf8))
        #expect(try session.open(pc.seal(Data("yo".utf8))) == Data("yo".utf8))
    }
}
