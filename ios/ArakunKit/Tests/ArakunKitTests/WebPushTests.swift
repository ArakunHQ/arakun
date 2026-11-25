import Foundation
import Testing
@testable import ArakunKit

private func b64(_ s: String) -> Data { Base64URL.decode(s)! }

/// The user agent's keys in RFC 8291 Appendix A.
private let rfcKeys = PushKeys(secret: b64("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94"), auth: b64("BTBZMqHH6r4Tts7J_aSIgg"))!

/// The message in RFC 8291 Appendix A, whole: header, then the one record.
private let rfcBody = b64(
    "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
)

/// A notice sealed to `rfcKeys` the way core/src/phone/push.rs seals one,
/// with five zero bytes of padding after the delimiter. Made with Python's
/// `cryptography` from RFC 8291's steps.
private let paddedNotice =
    "AAECAwQFBgcICQoLDA0ODwAAEABBBMI-9dwJM25aCrYnGb2s7YEUVwrFyD4jrsaqGUYlrosfH6Mfx-vx6c5Eta6FMaffzAJPdmUOx5ngcfuyzp1gIENPpQy46bMS7f-3Oc-ty7mrrREnxX6w9BMjER4xoOnSTwNB_BfR6FyO80O7xhd3bFc9t0ge6OJkCkvw2KkesErz2BR5MQfDS3iYu6uPEjja11bsvZEh1Wq3GxCbRrxPBYnhSWK2680QSw"

struct WebPushTests {
    @Test func decryptsTheRFC8291Example() throws {
        #expect(rfcKeys.publicKey == b64("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"))
        let plain = try WebPush.decrypt(rfcBody, keys: rfcKeys)
        #expect(String(decoding: plain, as: UTF8.self) == "When I grow up, I want to be a watermelon")
    }

    @Test func readsANoticeFromTheAlert() throws {
        let n = try WebPush.open(paddedNotice, keys: rfcKeys)
        #expect(n == PushNotice(kind: "input", session: "s1", title: "Fix the relay", body: "Allow cargo test?"))
        // Padding on the base64 is taken too.
        #expect(try WebPush.open(paddedNotice + "==", keys: rfcKeys) == n)
    }

    @Test func aNoticeWithTheEnvironmentAndTurnLengthHasASubtitle() throws {
        let n = try #require(PushNotice(json: Data(#"{"kind":"done","session":"s1","title":"t","body":"b","env":"app","secs":252}"#.utf8)))
        #expect(n.env == "app")
        #expect(n.secs == 252)
        #expect(n.subtitle == "app \u{b7} 4m 12s")
        #expect(PushNotice.subtitle(env: "app", secs: 42) == "app \u{b7} 42s")
        #expect(PushNotice.subtitle(env: "app", secs: 60) == "app \u{b7} 1m 0s")
        #expect(PushNotice.subtitle(env: "app", secs: nil) == "app")
        #expect(PushNotice.subtitle(env: "", secs: 90) == "")
    }

    @Test func aNoticeWithoutThemHasNoSubtitleAndAnOldOneStillReads() throws {
        let old = try #require(PushNotice(json: Data(#"{"kind":"done","session":"s1","title":"t","body":"b"}"#.utf8)))
        #expect(old == PushNotice(kind: "done", session: "s1", title: "t", body: "b"))
        #expect(old.env == "")
        #expect(old.secs == nil)
        #expect(old.subtitle == "")
    }

    @Test func refusesWhatIsNotForThesePhoneKeys() {
        #expect(throws: WebPush.Failure.doesNotDecrypt) { try WebPush.decrypt(rfcBody, keys: PushKeys.generate()) }
        var other = rfcKeys.stored
        other[other.count - 1] ^= 1
        #expect(throws: WebPush.Failure.doesNotDecrypt) { try WebPush.decrypt(rfcBody, keys: PushKeys(stored: other)!) }
    }

    @Test func refusesATamperedBody() {
        var bad = rfcBody
        bad[bad.count - 1] ^= 1
        #expect(throws: WebPush.Failure.doesNotDecrypt) { try WebPush.decrypt(bad, keys: rfcKeys) }
        var salt = rfcBody
        salt[0] ^= 1
        #expect(throws: WebPush.Failure.doesNotDecrypt) { try WebPush.decrypt(salt, keys: rfcKeys) }
    }

    @Test func refusesABrokenHeader() {
        #expect(throws: WebPush.Failure.malformed("header")) { try WebPush.decrypt(rfcBody.prefix(20), keys: rfcKeys) }
        var idlen = rfcBody
        idlen[20] = 64
        #expect(throws: WebPush.Failure.malformed("key id")) { try WebPush.decrypt(idlen, keys: rfcKeys) }
        #expect(throws: WebPush.Failure.malformed("short")) { try WebPush.decrypt(rfcBody.prefix(21 + 65 + 16), keys: rfcKeys) }
        // A record size smaller than the record: there would be a second one.
        var rs = rfcBody
        rs.replaceSubrange(16..<20, with: Data(bigEndian: UInt32(18)))
        #expect(throws: WebPush.Failure.malformed("more than one record")) { try WebPush.decrypt(rs, keys: rfcKeys) }
        // Not a point on the curve.
        var key = rfcBody
        key[22] ^= 1
        #expect(throws: WebPush.Failure.malformed("sender key")) { try WebPush.decrypt(key, keys: rfcKeys) }
        #expect(throws: WebPush.Failure.malformed("e")) { try WebPush.open("not base64!", keys: rfcKeys) }
    }

    @Test func aDecryptedBodyThatIsNoNoticeIsRefused() {
        // The RFC's plaintext is not JSON.
        #expect(throws: WebPush.Failure.malformed("notice")) { try WebPush.open(Base64URL.encode(rfcBody), keys: rfcKeys) }
        #expect(PushNotice(json: Data(#"{"kind":"done","title":"t"}"#.utf8)) == nil)
        #expect(PushNotice(json: Data("[]".utf8)) == nil)
        #expect(PushNotice(json: Data(#"{"session":"s9"}"#.utf8)) == PushNotice(kind: "", session: "s9", title: "", body: ""))
    }

    @Test func keysAreKeptAndReadBack() {
        let k = PushKeys.generate()
        #expect(k.publicKey.count == 65 && k.publicKey.first == 4)
        #expect(k.auth.count == 16)
        #expect(PushKeys(stored: k.stored) == k)
        #expect(PushKeys(stored: k.stored.prefix(47)) == nil)
        #expect(PushKeys(secret: Data(count: 31), auth: k.auth) == nil)
        #expect(PushKeys(secret: k.stored.prefix(32), auth: Data(count: 15)) == nil)
        #expect(PushKeys.generate() != k)
    }

    /// What `Subscription::from_browser` in core/src/phone/push.rs takes:
    /// an https endpoint, a 65-byte key and a 16-byte secret, base64url
    /// without padding.
    @Test func theSubscriptionIsWhatTheComputerTakes() throws {
        let url = URL(string: "https://app.arakun.dev/push/v1/apns/ab")!
        let sub = rfcKeys.subscription(endpoint: url)
        let json = try JSONSerialization.data(withJSONObject: sub)
        let back = try #require(JSON.parse(json))
        #expect(back["endpoint"].str == "https://app.arakun.dev/push/v1/apns/ab")
        #expect(back["keys"]["p256dh"].str == "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4")
        #expect(back["keys"]["auth"].str == "BTBZMqHH6r4Tts7J_aSIgg")
    }

    @Test func theEndpointNamesTheTokenInHex() {
        let token = Data([0x00, 0xAB, 0x10, 0xFF])
        let base = AccountHost.base
        #expect(PushEndpoint.url(base: base, token: token, sandbox: false)?.absoluteString == "https://app.arakun.dev/push/v1/apns/00ab10ff")
        #expect(PushEndpoint.url(base: base, token: token, sandbox: true)?.absoluteString == "https://app.arakun.dev/push/v1/apns-sandbox/00ab10ff")
        #expect(PushEndpoint.url(base: URL(string: "http://localhost:8787")!, token: token, sandbox: true) == nil)
        #expect(PushEndpoint.url(base: base, token: Data(), sandbox: false) == nil)
    }

    @Test func readsWhatTheComputerSaysAboutNotices() {
        let s = PushState(JSON.parse(Data(#"{"notifying":true,"notices":{"done":false,"input":true,"error":true}}"#.utf8))!)
        #expect(s == PushState(notifying: true, notices: NoticeChoice(done: false)))
        // An older computer that leaves some out: those stay on.
        let old = PushState(JSON.parse(Data(#"{"name":"phone","notices":{"error":false}}"#.utf8))!)
        #expect(old?.notifying == false)
        #expect(old?.notices == NoticeChoice(error: false))
        #expect(PushState(.null) == nil)
        var choice = NoticeChoice()
        choice[.input] = false
        #expect(choice == NoticeChoice(input: false))
        #expect(!choice[.input] && choice[.done] && choice[.error])
        let sent = try? JSONSerialization.data(withJSONObject: ["notices": NoticeChoice(input: false).json])
        #expect(JSON.parse(sent ?? Data())?["notices"] == JSON.parse(Data(#"{"done":true,"input":false,"error":true}"#.utf8)))
    }
}
