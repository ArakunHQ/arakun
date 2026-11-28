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

    @Test func readsASecret() {
        var s = LiveSession(id: "s1", title: "", agent: "")
        s.apply(state: json(#"""
        {"status":"needs_input","secret":{"id":7,"label":"OpenAI API key","reason":"To call the API.","deliver":"handle",
         "url":"https://platform.openai.com/api-keys","fields":[{"name":"key","label":"Key"}],"timeout_secs":300,"private":true}}
        """#))
        #expect(s.secret?.id == 7)
        #expect(s.secret?.fields == [SecretAsk.Field(name: "key", label: "Key")])
        #expect(s.secret?.url?.host == "platform.openai.com")
        #expect(s.secret?.isPrivate == true)
        // Only an http(s) link, and a connection is private only when it says so.
        s.apply(state: json(#"{"secret":{"id":8,"label":"L","reason":"R","url":"javascript:alert(1)","fields":[]}}"#))
        #expect(s.secret?.url == nil)
        #expect(s.secret?.isPrivate == false)
        s.apply(state: json(#"{"secret":null}"#))
        #expect(s.secret == nil)
    }

    @Test func readsTheChangesTurnByTurn() {
        let turns = ChangedTurn.list(json(#"""
        {"type":"changes","id":"s1","turns":[
         {"entry":4,"text":"Fix the limit","at":1700000000,"files":[
          {"path":"cloud/src/relay.ts","name":"relay.ts","added":12,"removed":5,"diffs":[[6,0],[8,1]]},
          {"path":"README.md","name":"README.md","added":1,"removed":0,"diffs":[[9,0],["x",1],[3],[1e20,0]]}]},
         {"entry":0,"text":"","at":0,"files":[]}]}
        """#))
        #expect(turns.count == 2)
        #expect(turns[0].text == "Fix the limit")
        #expect(turns[0].at == Date(timeIntervalSince1970: 1_700_000_000))
        #expect(turns[1].at == nil)
        let relay = turns[0].files[0]
        #expect(relay.diffs == [DiffRef(entry: 6, index: 0), DiffRef(entry: 8, index: 1)])
        #expect(relay.dir == "cloud/src")
        #expect(relay.added == 12)
        // A pair that names no diff is left out, the file stays.
        #expect(turns[0].files[1].diffs == [DiffRef(entry: 9, index: 0)])
        #expect(turns[0].files[1].dir == "")
        #expect(ChangedTurn.list(json(#"{"type":"changes"}"#)).isEmpty)
    }

    @Test func readsADiffWithItsHunksAndLines() throws {
        let d = try #require(FileDiff(json(#"""
        {"type":"diff","id":"s1","entry":6,"index":0,"path":"src/a.rs","name":"a.rs","added":1,"removed":1,
         "new_file":false,"deleted":false,"cut":40,
         "lines":["@@ -1,3 +1,3 @@ fn main",[" ",1,"fn main() {"],["-",2,"    old();"],["+",2,"    new();"],["+",null,""],[1,2],{"x":1}]}
        """#)))
        #expect(d.ref == DiffRef(entry: 6, index: 0))
        #expect(d.path == "src/a.rs")
        #expect(d.cut == 40)
        #expect(d.lines == [
            .hunk("@@ -1,3 +1,3 @@ fn main"),
            .text(mark: " ", number: 1, text: "fn main() {"),
            .text(mark: "-", number: 2, text: "    old();"),
            .text(mark: "+", number: 2, text: "    new();"),
            .text(mark: "+", number: nil, text: ""),
        ])
        let new = try #require(FileDiff(json(#"{"entry":1,"index":2,"new_file":true,"lines":[]}"#)))
        #expect(new.newFile && new.cut == 0)
        // Without its place there is nowhere to show it.
        #expect(FileDiff(json(#"{"type":"diff","path":"a"}"#)) == nil)
    }

    @Test func readsTheSubagentsOfASession() {
        let list = Subagent.list(json(#"""
        {"type":"subagents","id":"s1","brief":true,"subagents":[
         {"id":"a","name":"Explore","task":"Find the bug","state":"running","started":7,"ended":null,"call":"toolu_1","host":null},
         {"id":"b","name":"Review","task":"","state":"completed","started":8,"ended":9,"call":null,"host":"a","new_field":1}]}
        """#))
        #expect(list.map(\.id) == ["a", "b"])
        #expect(list[0].isRunning)
        #expect(list[0].started == Date(timeIntervalSince1970: 7))
        #expect(list[0].ended == nil)
        #expect(list[0].call == "toolu_1")
        #expect(list[1].ended == Date(timeIntervalSince1970: 9))
        #expect(list[1].host == "a")
        #expect(Subagent.list(json(#"{"type":"subagents","id":"s1"}"#)).isEmpty)
    }

    @Test func aToolNamesTheSubagentItRanUnder() {
        let under = Entry(json(#"{"type":"tool","id":"t","title":"x","kind":"read","status":"completed","parent":"task1"}"#))
        guard case .tool(let t) = under else { Issue.record("not a tool entry"); return }
        #expect(t.parent == "task1")
        let alone = Entry(json(#"{"type":"tool","id":"t","title":"x","kind":"read","status":"completed"}"#))
        guard case .tool(let a) = alone else { Issue.record("not a tool entry"); return }
        #expect(a.parent == nil)
        #expect(under != alone)
    }

    @Test func aSubagentsTranscriptPagesAsASessionDoes() {
        var t = SubagentTranscript(session: "s1", id: "a")
        t.apply(json(#"{"type":"subagent_entries","id":"s1","subagent":"a","from":2,"count":4,"entries":[{"type":"agent","text":"c"},{"type":"agent","text":"d"}]}"#))
        #expect(t.from == 2)
        t.apply(json(#"{"type":"subagent_entries","id":"s1","subagent":"a","from":0,"count":4,"older":true,"entries":[{"type":"agent","text":"a"},{"type":"agent","text":"b"}]}"#))
        #expect(t.from == 0)
        #expect(t.entries == ["a", "b", "c", "d"].map(Entry.agent))
        t.apply(json(#"{"type":"subagent_entries","id":"s1","subagent":"a","from":3,"count":5,"entries":[{"type":"agent","text":"D"},{"type":"agent","text":"e"}]}"#))
        #expect(t.entries == ["a", "b", "c", "D", "e"].map(Entry.agent))
        #expect(t.count == 5)
        // Another subagent's, or another session's, leaves it alone.
        let before = t
        t.apply(json(#"{"type":"subagent_entries","id":"s1","subagent":"b","from":0,"count":1,"entries":[{"type":"agent","text":"x"}]}"#))
        t.apply(json(#"{"type":"subagent_entries","id":"s2","subagent":"a","from":0,"count":1,"entries":[{"type":"agent","text":"x"}]}"#))
        #expect(t == before)
    }
}
