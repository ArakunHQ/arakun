package com.arakun.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.arakun.android.AppModel
import com.arakun.kit.LiveSession
import com.arakun.kit.SettingSet

/** The session's model and reasoning, as it runs; a pick applies from its next message on. */
@Composable
fun SettingsSheet(model: AppModel, s: LiveSession, onBack: () -> Unit) {
    val c = Palette.current
    // Asked when the sheet opens, and again when the link comes back: a request sent offline is dropped.
    LaunchedEffect(s.id, model.canSend) { model.loadSettings() }
    // A pick shows at once; the computer's answer confirms it, or puts back what holds.
    var picked by remember(s.id) { mutableStateOf(emptyMap<String, String>()) }
    LaunchedEffect(model.settings) { picked = emptyMap() }
    SheetTopBar("Model and reasoning", "From the next message on", onBack = onBack)
    val sets = model.settings?.filter { it.options.isNotEmpty() }
    when {
        sets == null -> Waiting(model)
        sets.isEmpty() -> Text(
            "This agent has not said what it offers yet. Try again in a moment.",
            style = Type.callout,
            color = c.textMuted,
            textAlign = TextAlign.Center,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 32.dp, vertical = 60.dp),
        )
        else -> Column(
            Modifier.verticalScroll(rememberScrollState()).navigationBarsPadding().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(24.dp),
        ) {
            sets.forEach { set ->
                SettingRows(set, set.on(picked[set.key]), enabled = model.canSend) { id ->
                    if (model.setSetting(set.key, id)) picked = SettingSet.pick(picked, set.key, id)
                }
            }
        }
    }
}

/** One setting's choices, one of them on: the session's sheet and the New session sheet share it. */
@Composable
fun SettingRows(set: SettingSet, on: String, enabled: Boolean, pick: (String) -> Unit) {
    val c = Palette.current
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Eyebrow(set.label)
        // One choice among the rows, for screen readers.
        Column(Modifier.fillMaxWidth().card().selectableGroup()) {
            set.options.forEachIndexed { i, o ->
                if (i > 0) Box(Modifier.padding(start = 14.dp).fillMaxWidth().height(1.dp).background(c.line))
                OptionRow(o, o.id == on, enabled && o.disabled == null) { pick(o.id) }
            }
        }
    }
}

@Composable
private fun OptionRow(o: SettingSet.Option, on: Boolean, enabled: Boolean, pick: () -> Unit) {
    val c = Palette.current
    Row(
        Modifier
            .fillMaxWidth()
            .selectable(selected = on, enabled = enabled, role = Role.RadioButton, onClick = pick)
            .alpha(if (enabled || on) 1f else 0.5f)
            .heightIn(min = 48.dp)
            .padding(horizontal = 14.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(o.name, style = geist(15f, FontWeight.Medium))
            // Why it cannot be picked says more than what it is.
            (o.disabled?.takeIf(String::isNotEmpty) ?: o.detail)?.let {
                Text(it, style = mono(11.5f), color = c.textFaint, maxLines = 2)
            }
        }
        Box(
            Modifier.size(18.dp).border(1.5.dp, if (on) c.amberInk else c.lineStrong, CircleShape),
            contentAlignment = Alignment.Center,
        ) {
            if (on) Box(Modifier.size(10.dp).background(c.amberInk, CircleShape))
        }
    }
}
