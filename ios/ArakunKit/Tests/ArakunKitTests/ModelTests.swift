import Foundation
import Testing
@testable import ArakunKit

private func json(_ s: String) -> JSON { JSON.parse(Data(s.utf8))! }

struct ModelTests {
    @Test func readsAnInbox() {
        let inbox = Inbox(json(#"""
        {"type":"inbox","host":"mac","sessions":[{"id":"s1","title":"Fix it","agent":"Claude","status":"needs_input",
         "updated":1700000000,"added":3,"removed":1,"project":"app","env":"main","place":"/w/app","pr":null,
         "asks":{"kind":"permission","text":"Run cargo test"},"new_field":[1,2]}],
         "places":[{"path":"/w/app","project":"app","env":"main","branch":"main","agent":"claude"}]}
        """#))
        #expect(inbox.host == "mac")
        #expect(inbox.sessions.first?.status == .needsInput)
        #expect(inbox.sessions.first?.asks == "Run cargo test")
        #expect(inbox.places.first?.branch == "main")
    }

    @Test func mergesTheTailAfterAReconnect() {
        var s = LiveSession(id: "s1", title: "", agent: "")
        s.apply(session: json(#"{"from":2,"count":4,"entries":[{"type":"agent","text":"c"},{"type":"agent","text":"d"}],"status":"working"}"#))
        #expect(s.from == 2 && s.entries.count == 2)
        s.apply(entries: json(#"{"from":0,"count":4,"older":true,"entries":[{"type":"agent","text":"a"},{"type":"agent","text":"b"}]}"#))
        #expect(s.from == 0 && s.entries == [.agent("a"), .agent("b"), .agent("c"), .agent("d")])
        s.apply(entries: json(#"{"from":3,"count":5,"entries":[{"type":"agent","text":"D"},{"type":"agent","text":"e"}]}"#))
        #expect(s.entries == [.agent("a"), .agent("b"), .agent("c"), .agent("D"), .agent("e")])
        s.apply(session: json(#"{"from":3,"count":5,"entries":[{"type":"agent","text":"D"},{"type":"agent","text":"E"}],"status":"idle"}"#))
        #expect(s.from == 0 && s.entries.count == 5 && s.entries.last == .agent("E"))
        #expect(s.status == .idle)
    }

    @Test func readsAPermissionAndAQuestion() {
        var s = LiveSession(id: "s1", title: "", agent: "")
        s.apply(state: json(#"""
        {"status":"needs_input","permission":{"tool_call_id":"t","title":"Run ls","options":[{"id":"a","name":"Allow","kind":"allow_once"}]},
         "question":{"tool_call_id":"q","questions":[{"index":1,"topic":"T","question":"Which?","options":["x","y"],"multi":false}]},
         "usage":{"summary":"12k tokens"},"elapsed_ms":2000,"queue":["next"],"changes":{"files":1,"added":2,"removed":0}}
        """#))
        #expect(s.permission?.options.first?.kind == "allow_once")
        #expect(s.question?.questions.first?.options == ["x", "y"])
        #expect(s.queue == ["next"])
        #expect(s.changes.added == 2)
        #expect(s.turnStarted != nil)
    }
}
