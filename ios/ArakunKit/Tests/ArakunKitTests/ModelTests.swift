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

    @Test func aNumberPastIntIsNilNotATrap() {
        #expect(json("1e20").int == nil)
        #expect(json("-1e20").int == nil)
        #expect(json("42").int == 42)
        #expect(json("2.9").int == 2)
    }

    @Test func readsTheImagesAUserEntryNames() {
        let e = Entry(json(#"{"type":"user","text":"look","at":0,"pastes":[],"images":[{"name":"shot.png","url":"/image?x"}]}"#))
        guard case .user(_, _, _, _, let images) = e else { Issue.record("not a user entry"); return }
        #expect(images == ["shot.png"])
    }

    @Test func aToolChangesWithItsExitCodeAndDiffs() {
        let a = Entry(json(#"{"type":"tool","id":"t","title":"x","kind":"edit","status":"completed","diffs":[{"path":"a","added":1,"removed":0}]}"#))
        let b = Entry(json(#"{"type":"tool","id":"t","title":"x","kind":"edit","status":"completed","diffs":[{"path":"a","added":2,"removed":0}]}"#))
        let c = Entry(json(#"{"type":"tool","id":"t","title":"x","kind":"edit","status":"completed","exit_code":1,"diffs":[{"path":"a","added":1,"removed":0}]}"#))
        #expect(a != b)
        #expect(a != c)
        #expect(a == Entry(json(#"{"type":"tool","id":"t","title":"x","kind":"edit","status":"completed","diffs":[{"path":"a","added":1,"removed":0}]}"#)))
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
