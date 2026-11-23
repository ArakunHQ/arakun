package com.arakun.android.ui

import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.asPaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.clearText
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.arakun.android.AppModel
import com.arakun.android.R
import com.arakun.android.Telemetry
import com.arakun.kit.InboxSession
import com.arakun.kit.SessionStatus
import kotlinx.coroutines.delay

/** Every session on the computer, the ones waiting for you first. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun InboxScreen(model: AppModel) {
    val c = Palette.current
    var starting by rememberSaveable { mutableStateOf(false) }
    val query = rememberTextFieldState()
    var account by rememberSaveable { mutableStateOf(false) }
    var refreshing by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) {
        if (model.demoNew) {
            model.demoNew = false
            starting = true
        }
    }
    // The pull settles once the fresh link is up or has failed.
    LaunchedEffect(refreshing, model.connection) {
        if (refreshing && model.connection != AppModel.Connection.Connecting) refreshing = false
        if (refreshing) {
            delay(8_000)
            refreshing = false
        }
    }

    val q = query.text.trim().toString().lowercase()
    val shown = if (q.isEmpty()) model.inbox else model.inbox.filter { "${it.title} ${it.project} ${it.env}".lowercase().contains(q) }
    val waiting = shown.filter { it.status == SessionStatus.NEEDS_INPUT }
    val projects = shown.filter { it.status != SessionStatus.NEEDS_INPUT }.groupBy { it.project }.toList()
    val mood = inboxMood(model)
    val insets = WindowInsets.safeDrawing.asPaddingValues()

    Column(Modifier.fillMaxSize().background(c.ground).statusBarsPadding()) {
        TopBar(
            model = model,
            onNew = { starting = true },
            onAccount = { account = true },
        )
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = {
                refreshing = true
                model.refresh()
            },
            modifier = Modifier.fillMaxSize(),
        ) {
            LazyColumn(
                Modifier.fillMaxSize(),
                contentPadding = PaddingValues(start = 16.dp, end = 16.dp, bottom = 32.dp + insets.calculateBottomPadding()),
                verticalArrangement = Arrangement.spacedBy(24.dp),
            ) {
                item(key = "header") { Header(model, mood) }
                item(key = "search") { SearchField(query) }
                if (model.connection.hasBanner) item(key = "banner") { ConnectionBanner(model) }
                if (waiting.isNotEmpty()) {
                    item(key = "needs") { Group("Needs you", c.amberInk, waiting, highlight = true, open = model::open) }
                }
                items(projects, key = { "p:" + it.first }) { (project, sessions) ->
                    Group(project.ifEmpty { "Sessions" }, c.textFaint, sessions, highlight = false, open = model::open)
                }
                if (model.isLinked && model.inbox.isEmpty()) {
                    item(key = "empty") { Empty(mood) }
                } else if (model.inbox.isNotEmpty() && shown.isEmpty()) {
                    item(key = "nomatch") {
                        Text(
                            "No sessions match “${query.text.trim()}”.",
                            style = Type.callout,
                            color = c.textMuted,
                            textAlign = TextAlign.Center,
                            modifier = Modifier.fillMaxWidth().padding(vertical = 48.dp),
                        )
                    }
                }
            }
        }
    }

    if (starting) {
        NewSessionSheet(model, onDismiss = { starting = false })
    }
    if (account) AccountSheet(model, onDismiss = { account = false })
}

/** The raccoon's mood for the whole computer: what most needs you wins. */
private fun inboxMood(model: AppModel): Mood = when {
    model.connection is AppModel.Connection.ComputerAway -> Mood.SLEEPING
    model.connection == AppModel.Connection.Connecting -> Mood.THINKING
    model.inbox.any { it.status == SessionStatus.NEEDS_INPUT } -> Mood.WAITING
    model.inbox.any { it.status == SessionStatus.WORKING } -> Mood.WORKING
    else -> Mood.IDLE
}

@Composable
private fun TopBar(model: AppModel, onNew: () -> Unit, onAccount: () -> Unit) {
    val c = Palette.current
    var menu by remember { mutableStateOf(false) }
    Row(
        Modifier.fillMaxWidth().height(52.dp).padding(horizontal = 6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box {
            IconTap(R.drawable.ic_more_horizontal, "More", c.textMuted) { menu = true }
            DropdownMenu(
                expanded = menu,
                onDismissRequest = { menu = false },
                containerColor = c.raised,
                shape = RoundedCornerShape(Radius.card),
            ) {
                model.account?.let {
                    Text(
                        "Signed in as ${it.login}",
                        style = mono(11.5f),
                        color = c.textFaint,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp).widthIn(max = 260.dp),
                    )
                    HorizontalDivider(color = c.line)
                }
                // Also with one computer: the list is only read again there,
                // so it is the way to a computer signed in since.
                DropdownMenuItem(
                    text = { Text("All computers", style = Type.callout) },
                    leadingIcon = { BrandIcon(R.drawable.ic_computer, 18.dp, c.text) },
                    onClick = {
                        menu = false
                        model.leaveMachine()
                    },
                )
                DropdownMenuItem(
                    text = { Text("Account", style = Type.callout) },
                    leadingIcon = { BrandIcon(R.drawable.ic_user, 18.dp, c.text) },
                    onClick = {
                        menu = false
                        onAccount()
                    },
                )
                DropdownMenuItem(
                    text = { Text("Reconnect", style = Type.callout) },
                    leadingIcon = { BrandIcon(R.drawable.ic_refresh01, 18.dp, c.text) },
                    onClick = {
                        menu = false
                        model.reconnectNow()
                    },
                )
                if (Telemetry.crashes != null) {
                    val on = Telemetry.crashes == true
                    DropdownMenuItem(
                        text = { Text(if (on) "Crash reports: on" else "Crash reports: off", style = Type.callout) },
                        leadingIcon = {
                            BrandIcon(if (on) R.drawable.ic_checkmark_circle02 else R.drawable.ic_circle, 18.dp, c.text)
                        },
                        onClick = { Telemetry.answer(!on) },
                    )
                }
            }
        }
        Spacer(Modifier.weight(1f))
        IconTap(
            R.drawable.ic_pencil_edit02, "New session", c.text,
            enabled = model.isLinked && model.places.isNotEmpty(),
            onClick = onNew,
        )
    }
}

/** A toolbar icon with a touch target of 48. */
@Composable
fun IconTap(id: Int, label: String, tint: Color, enabled: Boolean = true, size: Int = 22, onClick: () -> Unit) {
    Box(
        Modifier
            .size(48.dp)
            .clip(CircleShape)
            .tap(enabled = enabled, onClick = onClick)
            .alpha(if (enabled) 1f else 0.4f)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) { BrandIcon(id, size.dp, tint) }
}

@Composable
private fun Header(model: AppModel, mood: Mood) {
    val c = Palette.current
    val needs = model.inbox.count { it.status == SessionStatus.NEEDS_INPUT }
    val working = model.inbox.count { it.status == SessionStatus.WORKING }
    val summary = if (model.inbox.isEmpty()) "" else buildList {
        if (needs > 0) add("$needs need${if (needs == 1) "s" else ""} you")
        if (working > 0) add("$working working")
        add("${model.inbox.size} in all")
    }.joinToString(" · ")
    Row(verticalAlignment = Alignment.Bottom, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                LinkDot(model.connection)
                Eyebrow(model.host.ifEmpty { "Arakun" })
            }
            Text("Sessions", style = geist(34f, FontWeight.SemiBold).copy(letterSpacing = (-0.03).em))
            if (summary.isNotEmpty()) Text(summary, style = Type.small, color = c.textMuted)
        }
        // The empty inbox shows a big one instead: one raccoon per screen.
        if (model.inbox.isNotEmpty()) Mascot(mood, 64.dp)
    }
}

@Composable
private fun SearchField(query: TextFieldState) {
    val c = Palette.current
    val shape = RoundedCornerShape(Radius.control)
    Field(
        state = query,
        placeholder = "Search sessions",
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
        modifier = Modifier.fillMaxWidth().heightIn(min = 40.dp).background(c.raised, shape).border(1.dp, c.line, shape),
        leading = { BrandIcon(R.drawable.ic_search01, 16.dp, c.textFaint) },
        trailing = {
            if (query.text.isNotEmpty()) {
                Box(Modifier.clip(CircleShape).tap { query.clearText() }.semantics { contentDescription = "Clear" }) {
                    BrandIcon(R.drawable.ic_cancel01, 16.dp, c.textFaint)
                }
            }
        },
    )
}

@Composable
private fun Group(title: String, color: Color, sessions: List<InboxSession>, highlight: Boolean, open: (String) -> Unit) {
    val c = Palette.current
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Row(Modifier.padding(horizontal = 4.dp)) {
            Eyebrow(title, color)
            Spacer(Modifier.weight(1f))
            Text("${sessions.size}", style = mono(11f), color = c.textFaint)
        }
        Column(
            Modifier.fillMaxWidth().card(
                fill = if (highlight) c.amberSoft else c.raised,
                stroke = if (highlight) Brand.amber.copy(alpha = 0.35f) else c.line,
            ),
        ) {
            sessions.forEachIndexed { i, s ->
                if (i > 0) {
                    Box(
                        Modifier.padding(start = 16.dp).fillMaxWidth().height(1.dp)
                            .background(if (highlight) Brand.amber.copy(alpha = 0.2f) else c.line),
                    )
                }
                SessionRow(s) { open(s.id) }
            }
        }
    }
}

/** Ticks every `period` ms, so a shown age keeps up. */
@Composable
fun rememberNow(period: Long): Long {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(period) {
        while (true) {
            delay(period)
            now = System.currentTimeMillis()
        }
    }
    return now
}

@Composable
fun SessionRow(session: InboxSession, onClick: () -> Unit) {
    val c = Palette.current
    val now = rememberNow(30_000)
    Column(
        Modifier.fillMaxWidth().tap(onClick = onClick).padding(horizontal = 16.dp, vertical = 14.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(
                session.title.ifEmpty { "New session" },
                style = geist(16f, FontWeight.Medium),
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Text(shortAgo(session.updated, now), style = mono(11.5f), color = c.textFaint, modifier = Modifier.padding(top = 3.dp))
        }
        session.asks?.let { asks ->
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                BrandIcon(R.drawable.ic_hand, 13.dp, c.amberInk, Modifier.padding(top = 2.dp))
                Text(asks, style = Type.code, color = c.amberInk, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
            StatusChip(session.status)
            Text(session.agent, style = Type.small, color = c.textMuted, maxLines = 1)
            if (session.env.isNotEmpty()) {
                Text(
                    session.env, style = mono(11.5f), color = c.textFaint, maxLines = 1,
                    overflow = TextOverflow.MiddleEllipsis, modifier = Modifier.weight(1f),
                )
            }
            if (session.env.isEmpty()) Spacer(Modifier.weight(1f))
            Changes(session.added, session.removed)
        }
    }
}

/** Lines added and removed, in the status colors with their signs. */
@Composable
fun Changes(added: Int, removed: Int) {
    if (added + removed <= 0) return
    val c = Palette.current
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("+$added", style = mono(11.5f), color = c.success)
        Text("−$removed", style = mono(11.5f), color = c.failure)
    }
}

val SessionStatus.word: String
    get() = when (this) {
        SessionStatus.IDLE -> "Idle"
        SessionStatus.WORKING -> "Working"
        SessionStatus.NEEDS_INPUT -> "Needs you"
        SessionStatus.ERROR -> "Error"
    }

/** A session's state: a dot and a word, never the color alone. */
@Composable
fun StatusChip(status: SessionStatus) {
    val c = Palette.current
    val tone = when (status) {
        SessionStatus.IDLE -> c.textMuted
        SessionStatus.WORKING -> c.attention
        SessionStatus.NEEDS_INPUT -> c.amberInk
        SessionStatus.ERROR -> c.failure
    }
    Row(
        Modifier.background(tone.copy(alpha = 0.12f), RoundedCornerShape(Radius.chip)).padding(horizontal = 7.dp, vertical = 3.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusDot(status)
        Text(status.word, style = mono(11f, FontWeight.Medium), color = tone)
    }
}

@Composable
fun StatusDot(status: SessionStatus) {
    val c = Palette.current
    val color = when (status) {
        SessionStatus.IDLE -> c.textFaint
        SessionStatus.WORKING -> c.attention
        SessionStatus.NEEDS_INPUT -> Brand.amber
        SessionStatus.ERROR -> c.failure
    }
    // With Remove animations on, a working dot stays lit instead of pulsing.
    val alpha = if (status == SessionStatus.WORKING && !rememberStill()) {
        rememberInfiniteTransition(label = "pulse").animateFloat(
            1f, 0.35f, infiniteRepeatable(tween(900), RepeatMode.Reverse), label = "dot",
        ).value
    } else {
        1f
    }
    Box(Modifier.size(7.dp).alpha(alpha).background(color, CircleShape))
}

/** The link's state as the live dot: amber while up. */
@Composable
private fun LinkDot(connection: AppModel.Connection) {
    val up = connection == AppModel.Connection.Connected
    Box(
        Modifier
            .size(7.dp)
            .background(if (up) Brand.amber else Palette.current.textFaint, CircleShape)
            .semantics { contentDescription = if (up) "Connected" else "Not connected" },
    )
}

@Composable
private fun Empty(mood: Mood) {
    val c = Palette.current
    Column(
        Modifier.fillMaxWidth().padding(vertical = 48.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Mascot(mood, 110.dp)
        Text("No sessions yet", style = geist(18f, FontWeight.SemiBold))
        Text(
            "Start one here with the pencil, or in Arakun on your computer.",
            style = Type.callout,
            color = c.textMuted,
            textAlign = TextAlign.Center,
        )
    }
}

/** Whether the banner has something to say. */
val AppModel.Connection.hasBanner: Boolean
    get() = this != AppModel.Connection.Connected && this != AppModel.Connection.Idle

/** How the link to the computer is doing, when it is not simply up. */
@Composable
fun ConnectionBanner(model: AppModel) {
    val c = Palette.current
    val now = rememberNow(30_000)
    when (val conn = model.connection) {
        AppModel.Connection.Connected, AppModel.Connection.Idle -> {}
        AppModel.Connection.Connecting -> BannerLine(R.drawable.ic_loading03, "Connecting to your computer…", null, c.textMuted)
        is AppModel.Connection.ComputerAway -> BannerLine(
            R.drawable.ic_computer, "Your computer is offline.",
            conn.since?.let { "Last seen ${shortAgo(it, now)} ago. The app reconnects when it is back." },
            c.textMuted,
        )
        // Said here once, for as long as it holds: a dialog at every try would come back for hours.
        is AppModel.Connection.Limited -> BannerLine(R.drawable.ic_alert_circle, limitText(conn.which), limitDetail(conn.which), c.textMuted) {
            OutlineButton("Try again", { model.reconnectNow() }, Modifier.padding(top = 8.dp))
        }
    }
}

private fun limitText(which: String): String = when (which) {
    "daily" -> "Today's relay limit for your account is used up."
    "phones" -> "Too many devices are connected to this computer."
    else -> "The relay is busy."
}

private fun limitDetail(which: String): String = when (which) {
    "daily" -> "It starts over at midnight UTC, and the app reconnects then."
    "phones" -> "Close Arakun on one of them. The app tries again in a few minutes."
    else -> "The app tries again in a few minutes."
}

@Composable
private fun BannerLine(icon: Int, text: String, detail: String?, tone: Color, action: (@Composable () -> Unit)? = null) {
    val c = Palette.current
    Row(
        Modifier.fillMaxWidth().card().padding(12.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        BrandIcon(icon, 16.dp, tone, Modifier.padding(top = 2.dp))
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(text, style = Type.callout, color = tone)
            detail?.let { Text(it, style = Type.small, color = c.textFaint) }
            action?.invoke()
        }
    }
}

/** "now", "4m", "3h", "2d": the inbox's short ages. */
fun shortAgo(at: Long, now: Long = System.currentTimeMillis()): String {
    val s = maxOf(0L, (now - at) / 1000)
    return when {
        s < 60 -> "now"
        s < 3600 -> "${s / 60}m"
        s < 86400 -> "${s / 3600}h"
        else -> "${s / 86400}d"
    }
}
