import Foundation
import Testing
@testable import ArakunKit

private func json(_ s: String) -> JSON { JSON.parse(Data(s.utf8))! }

struct SettingsTests {
    @Test func readsASessionsSettings() {
        // As `wire::settings` writes them: `disabled` is the reason, or null.
        let sets = SettingSet.list(json(#"""
        {"type":"settings","id":"s1","settings":[
         {"key":"modelId","label":"Model","current":"opus","options":[
          {"id":"opus","name":"Opus","detail":"Most capable","disabled":null},
          {"id":"haiku","name":"Haiku","detail":null,"disabled":"Not on your plan"},
          {"name":"no id"}]},
         {"key":"reasoningEffort","label":"Reasoning","current":"high","options":[{"id":"high","name":"High"}]},
         {"label":"no key","options":[]}]}
        """#))
        #expect(sets.map(\.key) == ["modelId", "reasoningEffort"])
        #expect(sets[0].options.map(\.id) == ["opus", "haiku"])
        #expect(sets[0].options[0].detail == "Most capable")
        #expect(!sets[0].options[0].isDisabled)
        #expect(sets[0].options[1].disabled == "Not on your plan")
        #expect(sets[0].name() == "Opus")
        #expect(sets[0].name(of: "haiku") == "Haiku")
        #expect(sets[1].name(of: "gone") == nil)
    }

    @Test func aDisabledFlagWithoutAReasonStillDisables() {
        let set = SettingSet(json(#"{"key":"modelId","options":[{"id":"a","disabled":true},{"id":"b","disabled":false}]}"#))
        #expect(set.options.map(\.isDisabled) == [true, false])
        // No name: the id stands in.
        #expect(set.options[0].name == "a")
    }

    @Test func optionsSayWhichPlaceAndAgentTheyAreFor() {
        let o = StartOptions(json(#"{"type":"options","env":"/w/app","agent":"claude","settings":[{"key":"modelId","label":"Model","current":"x","options":[]}]}"#))
        #expect(o.isFor(env: "/w/app", agent: "claude"))
        #expect(!o.isFor(env: "/w/app", agent: "droid"))
        #expect(!o.isFor(env: "/w/other", agent: "claude"))
        #expect(o.settings.first?.label == "Model")
    }

    @Test func aStartSendsTheModelFirstAndANewModelDropsTheReasoning() {
        var p = StartPicks()
        #expect(p.pairs.isEmpty)
        p.pick(StartPicks.reasoning, "high")
        p.pick(StartPicks.model, "opus")
        // The reasoning on offer depends on the model.
        #expect(p[StartPicks.reasoning] == nil)
        p.pick(StartPicks.reasoning, "low")
        #expect(p.pairs == [["modelId", "opus"], ["reasoningEffort", "low"]])
        p.pick("other", "x")
        #expect(p.pairs.count == 2)
        p.clear()
        #expect(p.pairs.isEmpty)
    }

    @Test func readsThePlanLimits() {
        // As `wire::plan_usage` writes them.
        let cards = PlanCard.list(json(#"""
        {"type":"usage","agents":[
         {"agent":"claude","name":"Claude Code","signed_in":true,"plan":"Max","rows":[
          {"label":"5-hour","percent":42,"detail":"resets in 2h"},{"label":"Week","percent":100,"detail":null}],
          "idle":["Opus week"],"note":null,"tone":"normal","at":1700000000},
         {"agent":"droid","name":"Droid","signed_in":false,"note":"Sign in to droid on the computer."}]}
        """#))
        #expect(cards.map(\.agent) == ["claude", "droid"])
        let c = cards[0]
        #expect(c.signedIn && c.plan == "Max")
        #expect(c.rows == [PlanCard.Row(label: "5-hour", percent: 42, detail: "resets in 2h"), PlanCard.Row(label: "Week", percent: 100, detail: nil)])
        #expect(c.idle == ["Opus week"])
        #expect(c.note == nil && c.tone == "normal")
        #expect(c.at == Date(timeIntervalSince1970: 1700000000))
        let d = cards[1]
        #expect(!d.signedIn && d.rows.isEmpty && d.plan == nil && d.at == nil)
        #expect(d.note == "Sign in to droid on the computer.")
    }

    @Test func readsContextAndTokensFromTheUsage() {
        var s = LiveSession(id: "s1", title: "", agent: "")
        s.apply(state: json(#"{"status":"idle","usage":{"summary":"12.3k tok · 15% context","context_percent":15,"tokens":12300}}"#))
        #expect(s.usage == "12.3k tok · 15% context")
        #expect(s.contextPercent == 15)
        #expect(s.tokens == 12300)
        // An agent that does not say its context window.
        s.apply(state: json(#"{"status":"idle","usage":{"summary":"","context_percent":null,"tokens":0}}"#))
        #expect(s.usage == nil && s.contextPercent == nil && s.tokens == nil)
        s.apply(state: json(#"{"status":"idle","usage":{"context_percent":130}}"#))
        #expect(s.contextPercent == 100)
    }

    @Test func namesTheAgents() {
        #expect(Agents.name("droid") == "Droid")
        #expect(Agents.name("new-one") == "new-one")
        #expect(Agents.all.first?.id == "claude")
    }
}
