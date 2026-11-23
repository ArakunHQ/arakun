package com.arakun.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
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
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import com.arakun.android.AppModel
import com.arakun.android.R
import com.arakun.kit.Machine
import kotlinx.coroutines.delay

/** The account's computers: pick one to open its inbox. */
@Composable
fun MachinesScreen(model: AppModel) {
    val c = Palette.current
    var account by rememberSaveable { mutableStateOf(false) }
    val insets = WindowInsets.safeDrawing.asPaddingValues()
    val now = rememberNow(30_000)

    // The list keeps its online states fresh while it is on screen, as the phone page's does.
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (true) {
                delay(15_000)
                model.loadAccount()
            }
        }
    }

    Column(Modifier.fillMaxSize().background(c.ground).statusBarsPadding()) {
        LazyColumn(
            Modifier.fillMaxSize(),
            contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 32.dp + insets.calculateBottomPadding()),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            item(key = "header") {
                Row(verticalAlignment = Alignment.Bottom, modifier = Modifier.fillMaxWidth()) {
                    Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text("Computers", style = geist(34f, FontWeight.SemiBold).copy(letterSpacing = (-0.03).em))
                        model.me?.let { Text("Signed in as ${it.login}", style = Type.small, color = c.textMuted) }
                    }
                    OutlineButton("Account", { account = true })
                }
            }
            items(model.me?.notices.orEmpty(), key = { "n:" + it.id }) { n ->
                NoticeCard(n) { model.dismissNotice(n) }
            }
            val blocked = model.blocked
            val said = model.accountSaid
            if (blocked != null) {
                item(key = "blocked") { Problem(blocked) }
            } else if (said != null) {
                item(key = "said") {
                    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        Problem(said)
                        if (!model.machinesLoaded) OutlineButton("Try again", { model.loadAccount(reopen = true) })
                    }
                }
            }
            if (blocked == null && said == null && !model.machinesLoaded) {
                item(key = "loading") {
                    Box(Modifier.fillMaxWidth().padding(vertical = 48.dp), contentAlignment = Alignment.Center) {
                        CircularProgressIndicator(Modifier.size(18.dp), color = c.textMuted, strokeWidth = 2.dp)
                    }
                }
            }
            if (blocked == null && model.machinesLoaded) {
                if (model.machines.isEmpty()) {
                    item(key = "empty") {
                        Column(
                            Modifier.fillMaxWidth().padding(vertical = 48.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(14.dp),
                        ) {
                            Mascot(Mood.IDLE, 110.dp)
                            Text(
                                "No computers yet. Sign in on a computer running Arakun.",
                                style = Type.callout,
                                color = c.textMuted,
                                textAlign = TextAlign.Center,
                            )
                        }
                    }
                } else {
                    item(key = "label") { Eyebrow("Computers") }
                    item(key = "list") {
                        Column(Modifier.fillMaxWidth().card()) {
                            model.machines.forEachIndexed { i, m ->
                                if (i > 0) Box(Modifier.padding(start = 48.dp).fillMaxWidth().height(1.dp).background(c.line))
                                MachineRow(m, now) { model.pick(m) }
                            }
                        }
                    }
                }
            }
        }
    }

    if (account) AccountSheet(model, onDismiss = { account = false })
}

@Composable
private fun MachineRow(m: Machine, now: Long, onClick: () -> Unit) {
    val c = Palette.current
    val seen = when {
        m.online -> "online"
        m.lastSeenAt > 0 -> shortAgo(m.lastSeenAt * 1000, now).let { if (it == "now") "last seen now" else "last seen $it ago" }
        else -> "offline"
    }
    Row(
        Modifier
            .fillMaxWidth()
            .tap(onClick = onClick)
            .heightIn(min = 56.dp)
            .padding(horizontal = 14.dp, vertical = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(14.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        BrandIcon(R.drawable.ic_computer, 20.dp, c.textMuted)
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(m.name.ifEmpty { m.label }, style = geist(16f, FontWeight.Medium), maxLines = 1)
            Text(if (m.kind == "server") "Server" else "Arakun app", style = Type.small, color = c.textFaint)
        }
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = Alignment.CenterVertically) {
            Box(Modifier.size(7.dp).background(if (m.online) c.success else c.textFaint, CircleShape))
            Text(seen, style = Type.caption, color = c.textMuted)
        }
    }
}

/** What went wrong, in the failure color, with its icon. */
@Composable
fun Problem(text: String) {
    val c = Palette.current
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
        BrandIcon(R.drawable.ic_alert_circle, 15.dp, c.failure, Modifier.padding(top = 2.dp))
        Text(text, style = Type.small, color = c.failure)
    }
}
