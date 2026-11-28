package com.arakun.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.arakun.android.AppModel
import com.arakun.android.R
import com.arakun.kit.PlanLimits
import kotlinx.coroutines.delay

/** Both agents' plan limits on the computer open, as its Usage tab shows them. */
@Composable
fun PlanLimitsScreen(model: AppModel, onBack: () -> Unit) {
    val c = Palette.current
    // Kept fresh while on screen, as the computer reads them again when they are stale.
    // A request sent offline is dropped, so the link coming back asks again.
    LaunchedEffect(model.canSend) {
        while (model.canSend) {
            model.loadUsage()
            delay(60_000)
        }
    }
    Column(
        Modifier
            .fillMaxSize()
            .background(c.ground)
            // Nothing under the screen takes a tap meant for it.
            .pointerInput(Unit) {},
    ) {
        SheetTopBar(
            "Plan limits",
            model.host.ifEmpty { "This computer" },
            action = { IconTap(R.drawable.ic_refresh01, "Refresh", c.text, enabled = model.canSend, onClick = model::loadUsage) },
            onBack = onBack,
        )
        val limits = model.planLimits
        if (limits == null) {
            Waiting(model)
        } else {
            val now = rememberNow(30_000)
            Column(
                Modifier.verticalScroll(rememberScrollState()).navigationBarsPadding().padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                limits.agents.forEach { AgentCard(it) }
                val at = limits.at
                Text(
                    "Read from the agents' own bookkeeping on the computer, as Arakun's Usage tab shows them." +
                        (at?.let { shortAgo(it, now) }?.let { if (it == "now") " Updated now." else " Updated $it ago." } ?: ""),
                    style = Type.callout,
                    color = c.textFaint,
                )
            }
        }
    }
}

@Composable
private fun AgentCard(a: PlanLimits.Agent) {
    val c = Palette.current
    Column(Modifier.fillMaxWidth().card().padding(16.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
        Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(a.name, style = geist(16f, FontWeight.SemiBold))
            val plan = if (a.signedIn) a.plan else "Not signed in"
            plan?.let { Text(it, style = mono(11.5f), color = c.textMuted) }
        }
        if (a.signedIn) {
            a.rows.forEach { LimitRow(it) }
            a.balance?.let {
                Row(Modifier.fillMaxWidth()) {
                    Text("Extra usage balance", style = Type.callout, color = c.textMuted, modifier = Modifier.weight(1f))
                    Text(it, style = mono(13f), color = c.text)
                }
            }
            if (a.idle.isNotEmpty()) {
                Text("Not active: ${a.idle.joinToString(", ")}", style = mono(11.5f), color = c.textFaint)
            }
        }
        a.note?.let {
            val tone = when {
                !a.signedIn -> c.textMuted
                a.tone == "danger" -> c.failure
                a.tone == "warn" -> c.attention
                else -> c.textMuted
            }
            Text(it, style = Type.callout, color = tone)
        }
    }
}

@Composable
private fun LimitRow(r: PlanLimits.Row) {
    val c = Palette.current
    val words = "${r.percent}%" + (r.detail?.let { " · $it" } ?: "")
    Column(
        // Read as one line: the label and how full it is.
        Modifier.clearAndSetSemantics { contentDescription = "${r.label}: $words" },
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(r.label, style = geist(14f, FontWeight.Medium), modifier = Modifier.weight(1f))
            Text(words, style = mono(11.5f), color = c.textMuted)
        }
        Box(Modifier.fillMaxWidth().height(6.dp).background(c.line, RoundedCornerShape(3.dp))) {
            Box(
                Modifier
                    .fillMaxHeight()
                    .fillMaxWidth(r.percent.coerceIn(0, 100) / 100f)
                    .background(barTone(r.percent, c.textMuted, c.attention, c.failure), RoundedCornerShape(3.dp)),
            )
        }
    }
}

/** A window's bar by how full it is: calm, then a warning from 70%, then full. */
internal fun barTone(percent: Int, calm: Color, warn: Color, full: Color): Color = when {
    percent >= 100 -> full
    percent >= 70 -> warn
    else -> calm
}
