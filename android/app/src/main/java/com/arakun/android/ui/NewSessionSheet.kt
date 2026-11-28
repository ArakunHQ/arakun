package com.arakun.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.SheetValue
import androidx.compose.material3.rememberBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.arakun.android.AppModel
import com.arakun.android.R
import com.arakun.android.Shot
import com.arakun.kit.AGENTS
import com.arakun.kit.Place
import com.arakun.kit.SettingSet
import com.arakun.kit.startAgent

/** A new session in one of the places the computer offers, with its first prompt. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewSessionSheet(model: AppModel, onDismiss: () -> Unit) {
    val c = Palette.current
    var placePath by rememberSaveable { mutableStateOf(model.places.firstOrNull()?.path ?: "") }
    val place = model.places.firstOrNull { it.path == placePath }
    var agent by rememberSaveable { mutableStateOf(place?.startAgent() ?: AGENTS.first().first) }
    // The model and reasoning picked, by key; the rest stay as the agent would start.
    var picked by remember { mutableStateOf(emptyMap<String, String>()) }
    val text = rememberTextFieldState()
    var writing by remember { mutableStateOf(false) }
    // Photos for the first prompt; they stay while a start does not go out.
    val shots = remember { mutableStateListOf<Shot>() }
    val sheet = rememberBottomSheetState(SheetValue.Hidden, setOf(SheetValue.Hidden, SheetValue.Expanded))

    LaunchedEffect(model.places) {
        // The chosen place went away: fall back to the first one left.
        if (model.places.none { it.path == placePath }) {
            val first = model.places.firstOrNull()
            placePath = first?.path ?: ""
            agent = first?.startAgent() ?: agent
            picked = emptyMap()
        }
    }
    // What this agent offers there; asked again when the link comes back, as one sent offline is dropped.
    LaunchedEffect(placePath, agent, model.canSend) { place?.let { model.loadOptions(it, agent) } }
    val options = model.startOptions?.takeIf { it.env == placePath && it.agent == agent }?.settings?.filter { it.options.isNotEmpty() }

    LaunchedEffect(model.session?.id) {
        // Started, even from an earlier showing of the sheet: the session
        // opens over the inbox.
        if (model.session != null) onDismiss()
    }

    val prompt = text.text.trim().toString()
    val sendable = model.places.any { it.path == placePath } && (prompt.isNotEmpty() || shots.isNotEmpty()) && model.isLinked
    // The pending start and its doubt live in the model, so closing and
    // opening the sheet again neither unlocks it nor hides the warning. A
    // refusal ends the start, which lets the form be sent again.
    val pending = model.starting
    // The link dropped before the computer answered: the session may exist
    // already, so starting again is a separate, explicit choice.
    val unsure = model.startUnsure
    val ready = !pending && sendable

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheet,
        containerColor = c.ground,
        shape = RoundedCornerShape(topStart = Radius.dialog, topEnd = Radius.dialog),
        dragHandle = {
            Box(Modifier.padding(top = 10.dp, bottom = 4.dp).size(36.dp, 4.dp).background(c.lineStrong, CircleShape))
        },
    ) {
        Column(Modifier.navigationBarsPadding().imePadding()) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    "Cancel", style = geist(16f), color = c.textMuted,
                    modifier = Modifier.tap(onClick = onDismiss).padding(horizontal = 8.dp, vertical = 10.dp),
                )
                Spacer(Modifier.weight(1f))
                Text("New session", style = geist(17f, FontWeight.SemiBold))
                Spacer(Modifier.weight(1f))
                // Balances Cancel so the title stays centred.
                Text("Cancel", style = geist(16f), color = c.ground, modifier = Modifier.padding(horizontal = 8.dp))
            }
            Column(
                Modifier.weight(1f, fill = false).verticalScroll(rememberScrollState()).padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(24.dp),
            ) {
                Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Eyebrow("Where")
                    // One choice among the rows, for screen readers.
                    Column(Modifier.fillMaxWidth().card().selectableGroup()) {
                        model.places.forEachIndexed { i, p ->
                            if (i > 0) Box(Modifier.padding(start = 44.dp).fillMaxWidth().height(1.dp).background(c.line))
                            PlaceRow(p, p.path == placePath) {
                                if (p.path != placePath) {
                                    placePath = p.path
                                    // Each place starts its own agent; what was picked was for the other.
                                    agent = p.startAgent()
                                    picked = emptyMap()
                                }
                            }
                        }
                    }
                }
                Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Eyebrow("Agent")
                    Row(
                        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).selectableGroup(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        AGENTS.forEach { (id, name) ->
                            AgentChip(name, id == agent) {
                                if (id != agent) {
                                    agent = id
                                    picked = emptyMap()
                                }
                            }
                        }
                    }
                }
                when {
                    options != null -> options.forEach { set ->
                        SettingRows(set, set.on(picked[set.key]), enabled = true) { id -> picked = SettingSet.pick(picked, set.key, id) }
                    }
                    model.canSend -> Text("Loading models…", style = Type.callout, color = c.textMuted)
                }
                Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Eyebrow("Prompt")
                    Field(
                        state = text,
                        placeholder = "What should the agent do?",
                        style = Type.body,
                        lines = TextFieldLineLimits.MultiLine(minHeightInLines = 5, maxHeightInLines = 14),
                        padding = PaddingValues(14.dp),
                        modifier = Modifier
                            .fillMaxWidth()
                            .onFocusChanged { writing = it.isFocused }
                            .card(c.raised, if (writing) c.lineStrong else c.line),
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
                        PhotoButton(
                            shots.size,
                            add = { if (shots.size < MAX_SHOTS) shots.add(it) },
                            failed = { model.notice = it },
                            modifier = Modifier.card(),
                        )
                        ShotChips(shots, remove = { shots.removeAt(it) })
                    }
                }
            }
            if (unsure) {
                Text(
                    "The connection dropped before Arakun answered, so the session may have started. Check the inbox first.",
                    style = Type.callout,
                    color = c.textMuted,
                    modifier = Modifier.padding(horizontal = 16.dp),
                )
            }
            PrimaryButton(
                onClick = {
                    val p = model.places.firstOrNull { it.path == placePath } ?: return@PrimaryButton
                    if (model.startSession(p, prompt, shots.toList(), agent, picked)) shots.clear()
                },
                enabled = ready,
                modifier = Modifier.fillMaxWidth().padding(16.dp),
            ) {
                if (pending) CircularProgressIndicator(Modifier.size(16.dp), color = Brand.onAmber, strokeWidth = 2.dp)
                Text(
                    when {
                        unsure -> "Start it again anyway"
                        pending -> "Starting…"
                        else -> "Start session"
                    },
                )
            }
        }
    }
}

@Composable
private fun AgentChip(name: String, on: Boolean, pick: () -> Unit) {
    val c = Palette.current
    Text(
        name,
        style = geist(14f, FontWeight.Medium),
        color = if (on) c.text else c.textMuted,
        modifier = Modifier
            .card(if (on) c.raised else c.ground, if (on) c.amberInk else c.line, Radius.control)
            .selectable(selected = on, role = Role.RadioButton, onClick = pick)
            .heightIn(min = 40.dp)
            .padding(horizontal = 14.dp, vertical = 10.dp),
    )
}

@Composable
private fun PlaceRow(p: Place, on: Boolean, pick: () -> Unit) {
    val c = Palette.current
    Row(
        Modifier
            .fillMaxWidth()
            .selectable(selected = on, role = Role.RadioButton, onClick = pick)
            .heightIn(min = 48.dp)
            .padding(horizontal = 14.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        BrandIcon(R.drawable.ic_folder01, 18.dp, if (on) c.amberInk else c.textFaint)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(p.project.ifEmpty { p.path }, style = geist(15f, FontWeight.Medium))
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
                if (p.env.isNotEmpty()) Text(p.env, style = mono(11.5f), color = c.textFaint, maxLines = 1)
                p.branch?.takeIf { it != p.env }?.let {
                    BrandIcon(R.drawable.ic_git_branch, 11.dp, c.textFaint)
                    Text(it, style = mono(11.5f), color = c.textFaint, maxLines = 1)
                }
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
