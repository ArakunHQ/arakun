import Foundation
import Testing
@testable import ArakunKit

/// `fetch` over the channel: `{req}` out, `{res}` back in pieces, as
/// phone/relay.js and core/src/phone/relay.rs speak it.
struct RequestTests {
    @Test func theRequestIsWhatThePageSends() throws {
        let post = try JSONSerialization.data(withJSONObject: RelayLink.request(id: 3, method: "POST", path: "/push", body: #"{"subscription":null}"#))
        #expect(JSON.parse(post) == JSON.parse(Data(#"{"req":{"id":3,"method":"POST","path":"/push","body":"{\"subscription\":null}"}}"#.utf8)))
        let get = try JSONSerialization.data(withJSONObject: RelayLink.request(id: 4, method: "GET", path: "/me", body: nil))
        #expect(JSON.parse(get)?["req"]["body"] == .null)
        #expect(JSON.parse(get)?["req"]["id"].int == 4)
    }

    @Test func joinsAnAnswerInPieces() {
        var r = Replies()
        r.expect(1)
        r.expect(2)
        let first = r.add(["id": 1, "status": 200, "type": "application/json", "body": Base64URL.encode(Data(#"{"a":"#.utf8)), "more": true])
        #expect(first?.id == nil)
        // Another request's answer in between.
        let two = r.add(["id": 2, "status": 403, "body": "", "more": false])
        #expect(two?.id == 2)
        #expect(two?.status == 403)
        #expect(two?.body.isEmpty == true)
        let one = r.add(["id": 1, "body": Base64URL.encode(Data("1}".utf8)), "more": false])
        #expect(one?.id == 1)
        #expect(one?.status == 200)
        #expect(one.map { String(decoding: $0.body, as: UTF8.self) } == #"{"a":1}"#)
        // Answered once: a piece after the last is nobody's.
        let again = r.add(["id": 1, "body": "", "more": false])
        #expect(again?.id == nil)
    }

    @Test func ignoresAnAnswerNobodyAskedFor() {
        var r = Replies()
        let unasked = r.add(["id": 9, "status": 200, "body": "", "more": false])
        #expect(unasked?.id == nil)
        let noID = r.add(["status": 200])
        #expect(noID?.id == nil)
    }

    @Test func anAnswerWithoutAStatusIsAFailure() {
        var r = Replies()
        r.expect(5)
        let got = r.add(["id": 5, "body": ""])
        #expect(got?.status == 500)
    }
}
