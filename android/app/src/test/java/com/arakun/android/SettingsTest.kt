package com.arakun.android

import com.arakun.android.ui.barTone
import com.arakun.kit.PlanLimits
import com.arakun.kit.SettingSet
import com.arakun.kit.parseJson
import androidx.compose.ui.graphics.Color
import kotlin.test.Test
import kotlin.test.assertEquals

class SettingsTest {
    @Test
    fun aStartCarriesItsSettingsAsPairs() {
        // As core/src/phone/mod.rs reads `start`: settings is [[key, value], …].
        val picked = SettingSet.pick(mapOf(SettingSet.EFFORT to "low"), SettingSet.MODEL, "haiku")
        val m = message("type" to "start", "env" to "/w/app", "settings" to SettingSet.startPairs(picked), "uploads" to listOf("u1"))
        assertEquals("""{"type":"start","env":"/w/app","settings":[["modelId","haiku"]],"uploads":["u1"]}""", m.toString())
    }

    @Test
    fun theDemoSpeaksTheWire() {
        val sets = SettingSet.list(parseJson(Demo.settings)!!)
        assertEquals(listOf(SettingSet.MODEL, SettingSet.EFFORT), sets.map { it.key })
        val limits = PlanLimits.of(parseJson(Demo.usage(1_700_000_000))!!)
        assertEquals(listOf("claude", "droid"), limits.agents.map { it.agent })
        assertEquals("$4.20", limits.agents[1].balance)
    }

    @Test
    fun aBarWarnsAsItFills() {
        val (calm, warn, full) = Triple(Color.Gray, Color.Yellow, Color.Red)
        assertEquals(calm, barTone(69, calm, warn, full))
        assertEquals(warn, barTone(70, calm, warn, full))
        assertEquals(full, barTone(100, calm, warn, full))
    }
}
