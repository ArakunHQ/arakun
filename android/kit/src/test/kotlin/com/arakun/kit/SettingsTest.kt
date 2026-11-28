package com.arakun.kit

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

private fun json(s: String) = parseJson(s)!!

class SettingsTest {
    // As wire::settings writes them.
    private val sets = """
        [{"key":"modelId","label":"Model","current":"opus","options":[
           {"id":"opus","name":"Opus","detail":"Most capable","disabled":null},
           {"id":"haiku","name":"Haiku","detail":null,"disabled":"Not on your plan"}]},
         {"key":"reasoningEffort","label":"Reasoning","current":"high","options":[{"id":"high","name":"High"},{"id":"low"}]},
         {"label":"No key","options":[]}]
    """

    @Test
    fun readsSettings() {
        val list = SettingSet.list(json(sets))
        assertEquals(listOf("modelId", "reasoningEffort"), list.map { it.key })
        val model = list[0]
        assertEquals("Model", model.label)
        assertEquals("opus", model.current)
        assertEquals("Most capable", model.options[0].detail)
        assertNull(model.options[0].disabled)
        assertEquals("Not on your plan", model.options[1].disabled)
        // A choice without a name shows its id.
        assertEquals("low", list[1].options[1].name)
        assertEquals("haiku", model.on("haiku"))
        assertEquals("opus", model.on(null))
    }

    @Test
    fun readsAnOptionsAnswer() {
        val o = StartOptions.of(json("""{"type":"options","env":"/w/app","agent":"claude","settings":$sets}"""))
        assertEquals("/w/app", o.env)
        assertEquals("claude", o.agent)
        assertEquals(2, o.settings.size)
        assertTrue(StartOptions.of(json("""{"type":"options"}""")).settings.isEmpty())
    }

    @Test
    fun aNewModelDropsTheReasoningPicked() {
        var picked = SettingSet.pick(emptyMap(), SettingSet.EFFORT, "low")
        picked = SettingSet.pick(picked, SettingSet.MODEL, "haiku")
        assertEquals(mapOf(SettingSet.MODEL to "haiku"), picked)
        picked = SettingSet.pick(picked, SettingSet.EFFORT, "high")
        assertEquals(listOf("modelId" to "haiku", "reasoningEffort" to "high"), SettingSet.startPairs(picked))
        assertEquals(emptyList(), SettingSet.startPairs(mapOf("other" to "x")))
    }

    @Test
    fun readsPlanLimits() {
        // As wire::plan_usage writes them: a signed-in Claude, a Droid that is not.
        val u = PlanLimits.of(
            json(
                """
                {"type":"usage","agents":[
                  {"agent":"claude","name":"Claude Code","signed_in":true,"plan":"Max",
                   "rows":[{"label":"5h","percent":42,"detail":"resets 15:40"},{"label":"Week","percent":104,"detail":null}],
                   "idle":["Opus week"],"note":null,"tone":"normal","at":1700000000},
                  {"agent":"droid","name":"Droid","signed_in":false,"note":"Sign in to droid on the computer."}]}
                """,
            ),
        )
        val claude = u.agents[0]
        assertTrue(claude.signedIn)
        assertEquals("Max", claude.plan)
        assertEquals(listOf(42, 104), claude.rows.map { it.percent })
        assertEquals("resets 15:40", claude.rows[0].detail)
        assertNull(claude.rows[1].detail)
        assertEquals(listOf("Opus week"), claude.idle)
        assertNull(claude.note)
        val droid = u.agents[1]
        assertFalse(droid.signedIn)
        assertEquals("Sign in to droid on the computer.", droid.note)
        assertTrue(droid.rows.isEmpty())
        assertNull(droid.at)
        assertEquals(1_700_000_000_000L, u.at)
    }

    @Test
    fun readsContextAndTokens() {
        val s = LiveSession("s", "", "").applyState(
            json("""{"status":"idle","usage":{"summary":"12k tok · 15% context","context_percent":15,"tokens":12000}}"""),
        )
        assertEquals(15, s.contextPercent)
        assertEquals(12_000L, s.tokens)
        val none = s.applyState(json("""{"status":"idle","usage":{"summary":null,"context_percent":null,"tokens":0}}"""))
        assertNull(none.contextPercent)
        assertNull(none.tokens)
    }
}
