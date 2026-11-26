package com.arakun.android.ui

import android.Manifest
import android.content.Intent
import android.os.Build
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
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
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import com.arakun.android.AppModel
import com.arakun.android.Push
import com.arakun.android.R
import com.arakun.kit.Notice
import com.arakun.kit.SignedInDevice
import com.arakun.kit.WebPush

/**
 * The account on this phone: who is signed in, the operator's notices,
 * notifications, the account's signed-in devices (any can be signed out) and
 * Sign out.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AccountSheet(model: AppModel, onDismiss: () -> Unit) {
    val c = Palette.current
    val sheet = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    var confirmSignOut by remember { mutableStateOf(false) }
    val now = rememberNow(30_000)
    LaunchedEffect(Unit) { model.loadDevices() }
    // Signed out, here or by the account host: nothing left to show.
    LaunchedEffect(model.account) { if (model.account == null) onDismiss() }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        sheetState = sheet,
        containerColor = c.ground,
        shape = RoundedCornerShape(topStart = Radius.dialog, topEnd = Radius.dialog),
        dragHandle = {
            Box(Modifier.padding(top = 10.dp, bottom = 4.dp).size(36.dp, 4.dp).background(c.lineStrong, CircleShape))
        },
    ) {
        Column(
            Modifier.navigationBarsPadding().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(24.dp),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text("Account", style = geist(22f, FontWeight.SemiBold))
                val login = model.me?.login ?: model.account?.login
                if (login != null) Text("Signed in with GitHub as $login", style = Type.callout, color = c.textMuted)
            }

            val notices = model.me?.notices.orEmpty()
            if (notices.isNotEmpty()) {
                Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Eyebrow("Notices")
                    notices.forEach { n -> NoticeCard(n) { model.dismissNotice(n) } }
                }
            }

            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Eyebrow("Notifications")
                Notifications(model)
            }

            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Eyebrow("Signed-in devices")
                val devices = model.devices
                when {
                    devices == null && model.devicesSaid == null -> Row(
                        Modifier.fillMaxWidth().heightIn(min = 48.dp),
                        horizontalArrangement = Arrangement.spacedBy(10.dp, Alignment.CenterHorizontally),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        CircularProgressIndicator(Modifier.size(16.dp), color = c.textMuted, strokeWidth = 2.dp)
                    }
                    devices != null -> Column(Modifier.fillMaxWidth().card()) {
                        devices.forEachIndexed { i, d ->
                            if (i > 0) Box(Modifier.padding(start = 14.dp).fillMaxWidth().height(1.dp).background(c.line))
                            DeviceRow(d, now) { model.signOutDevice(d) }
                        }
                    }
                }
                model.devicesSaid?.let { Text(it, style = Type.small, color = c.failure) }
                Text(
                    "Every signed-in device can reach all your computers. Sign out any you do not know.",
                    style = Type.small,
                    color = c.textFaint,
                )
            }

            OutlineButton("Sign out", { confirmSignOut = true }, Modifier.fillMaxWidth(), tone = c.failure)
        }
    }

    if (confirmSignOut) {
        AlertDialog(
            onDismissRequest = { confirmSignOut = false },
            title = {
                Text("Sign out of ${model.me?.login ?: "GitHub"} on this phone?", style = geist(18f, FontWeight.SemiBold))
            },
            text = { Text("To reach your computers again, sign in with GitHub.", style = Type.callout, color = c.textMuted) },
            confirmButton = {
                TextButton(onClick = {
                    confirmSignOut = false
                    model.signOut()
                }) { Text("Sign out", color = c.failure, style = geist(15f, FontWeight.Medium)) }
            },
            dismissButton = {
                TextButton(onClick = { confirmSignOut = false }) { Text("Cancel", color = c.textMuted, style = geist(15f, FontWeight.Medium)) }
            },
            containerColor = c.raised,
        )
    }
}

/**
 * Notifications on or off, and which notices, as the phone page has them.
 * Every computer this phone links to gets the same choice.
 */
@Composable
private fun Notifications(model: AppModel) {
    val c = Palette.current
    val context = LocalContext.current
    if (!Push.available) {
        Text("This build of the app cannot get notifications.", style = Type.small, color = c.textFaint)
        return
    }
    // Android's own switch for the app can change while the app is away.
    var allowed by remember { mutableStateOf(Push.allowed(context)) }
    var refused by remember { mutableStateOf(false) }
    LifecycleResumeEffect(Unit) {
        allowed = Push.allowed(context)
        onPauseOrDispose {}
    }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { yes ->
        allowed = Push.allowed(context)
        refused = !yes
        if (yes) model.notifications(true)
    }
    val w = Push.wanted
    Column(Modifier.fillMaxWidth().card()) {
        SwitchRow("Notifications", "For permissions, questions, finished turns and errors.", Push.on) { on ->
            when {
                !on -> model.notifications(false)
                Build.VERSION.SDK_INT >= 33 && !allowed -> ask.launch(Manifest.permission.POST_NOTIFICATIONS)
                else -> model.notifications(true)
            }
        }
        if (Push.on) {
            listOf(
                NoticeSwitch("Turn finished", "When an agent is done and waits for you.", w.done) { w.copy(done = it) },
                NoticeSwitch("Needs permission or answer", "When an agent asks to run a tool or asks you a question.", w.input) { w.copy(input = it) },
                NoticeSwitch("Something went wrong", "When a turn stops on an error.", w.error) { w.copy(error = it) },
            ).forEach { n ->
                Box(Modifier.padding(start = 14.dp).fillMaxWidth().height(1.dp).background(c.line))
                SwitchRow(n.title, n.detail, n.on) { model.want(n.change(it)) }
            }
            Box(Modifier.padding(start = 14.dp).fillMaxWidth().height(1.dp).background(c.line))
            ChoiceRow("Only when I'm away for", WebPush.AWAY_CHOICES, w.awayAfterSecs) { model.want(w.copy(awayAfterSecs = it)) }
            Box(Modifier.padding(start = 14.dp).fillMaxWidth().height(1.dp).background(c.line))
            ChoiceRow("Turns shorter than this stay quiet", WebPush.SHORTEST_CHOICES, w.minTurnSecs) {
                model.want(w.copy(minTurnSecs = it))
            }
        }
    }
    if (!allowed && (Push.on || refused)) {
        Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text("Android blocks notifications from Arakun. Allow them in its settings.", style = Type.small, color = c.attention)
            TextButton(
                onClick = {
                    runCatching {
                        context.startActivity(
                            Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName),
                        )
                    }
                },
                contentPadding = ZeroPadding,
            ) { Text("Open settings", color = c.amberInk, style = geist(14f, FontWeight.Medium)) }
        }
    }
    Text(
        "A computer sends them once this phone has opened it with notifications on.",
        style = Type.small,
        color = c.textFaint,
    )
}

/** A row with one of a few times to pick; 0 turns the rule off. A value set elsewhere stays on the list. */
@Composable
private fun ChoiceRow(title: String, choices: List<WebPush.Choice>, value: Int, change: (Int) -> Unit) {
    val c = Palette.current
    var open by remember { mutableStateOf(false) }
    val list = WebPush.choicesKeeping(choices, value)
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .tap(role = Role.DropdownList) { open = true }
            .padding(start = 14.dp, end = 14.dp, top = 10.dp, bottom = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(title, style = geist(15f, FontWeight.Medium), modifier = Modifier.weight(1f))
        Box {
            Text(list.firstOrNull { it.secs == value }?.label ?: "$value s", style = Type.callout, color = c.textMuted)
            DropdownMenu(
                expanded = open,
                onDismissRequest = { open = false },
                containerColor = c.raised,
                shape = RoundedCornerShape(Radius.card),
            ) {
                list.forEach { choice ->
                    DropdownMenuItem(
                        text = { Text(choice.label, style = Type.callout) },
                        onClick = {
                            open = false
                            change(choice.secs)
                        },
                    )
                }
            }
        }
    }
}

/** One kind of notice: what it is called, when it comes, and the choice with it flipped. */
private class NoticeSwitch(val title: String, val detail: String, val on: Boolean, val change: (Boolean) -> WebPush.Wanted)

/** A row that is all one switch, so the whole row is the target. */
@Composable
private fun SwitchRow(title: String, detail: String?, on: Boolean, change: (Boolean) -> Unit) {
    val c = Palette.current
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 56.dp)
            .toggleable(value = on, role = Role.Switch, onValueChange = change)
            .padding(start = 14.dp, end = 14.dp, top = 10.dp, bottom = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(title, style = geist(15f, FontWeight.Medium))
            if (detail != null) Text(detail, style = Type.small, color = c.textFaint)
        }
        Switch(
            checked = on,
            onCheckedChange = null,
            colors = SwitchDefaults.colors(
                checkedThumbColor = Brand.onAmber,
                checkedTrackColor = Brand.amber,
                checkedBorderColor = Brand.amber,
                uncheckedThumbColor = c.textFaint,
                uncheckedTrackColor = c.deep,
                uncheckedBorderColor = c.lineStrong,
            ),
        )
    }
}

@Composable
private fun DeviceRow(d: SignedInDevice, now: Long, signOut: () -> Unit) {
    val c = Palette.current
    val used = d.lastUsedAt.takeIf { it > 0 }?.let { shortAgo(it * 1000, now) }
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).padding(start = 14.dp, end = 6.dp, top = 10.dp, bottom = 10.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Text(d.name.ifEmpty { kindName(d.kind) }, style = geist(15f, FontWeight.Medium), maxLines = 1)
            Text(
                listOfNotNull(kindName(d.kind), used?.let { if (it == "now") "used now" else "used $it ago" }).joinToString(" · "),
                style = Type.small,
                color = c.textFaint,
            )
        }
        if (d.current) {
            Text("This phone", style = Type.caption, color = c.textMuted, modifier = Modifier.padding(end = 8.dp))
        } else {
            TextButton(onClick = signOut) { Text("Sign out", color = c.failure, style = geist(14f, FontWeight.Medium)) }
        }
    }
}

/** What a signed-in device is, in words. */
private fun kindName(kind: String): String = when (kind) {
    "app" -> "Arakun app"
    "server" -> "Server"
    "phone" -> "Phone page"
    "ios" -> "iPhone app"
    "android" -> "Android app"
    else -> "Device"
}

/** A message from the operator to this account, until dismissed. */
@Composable
fun NoticeCard(n: Notice, onDismiss: () -> Unit) {
    val c = Palette.current
    val uris = LocalUriHandler.current
    Row(
        Modifier.fillMaxWidth().card().padding(start = 12.dp, top = 12.dp, bottom = 12.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        BrandIcon(R.drawable.ic_alert_circle, 16.dp, c.textMuted, Modifier.padding(top = 2.dp))
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(n.text, style = Type.callout)
            n.link?.let { url ->
                Text(
                    url,
                    style = Type.small,
                    color = c.amberInk,
                    maxLines = 1,
                    modifier = Modifier.tap { runCatching { uris.openUri(url) } },
                )
            }
        }
        Box(
            Modifier
                .size(40.dp)
                .clip(CircleShape)
                .tap(onClick = onDismiss)
                .semantics { contentDescription = "Dismiss" },
            contentAlignment = Alignment.Center,
        ) { BrandIcon(R.drawable.ic_cancel01, 16.dp, c.textFaint) }
    }
}
