package com.arakun.android.ui

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import com.arakun.android.AppModel
import com.arakun.android.Telemetry

/** Pairing until there are credentials, then the inbox, and a session over it. */
@Composable
fun Root(model: AppModel) {
    val c = Palette.current
    Box(Modifier.fillMaxSize().background(c.ground)) {
        val demo = model.demo
        when {
            demo?.moods == true -> MoodGallery()
            model.credentials == null -> PairScreen(model)
            else -> {
                AnimatedContent(
                    targetState = model.session?.id,
                    transitionSpec = {
                        if (targetState != null) {
                            (slideInHorizontally(tween(260)) { it / 3 } + fadeIn(tween(200))) togetherWith fadeOut(tween(160))
                        } else {
                            fadeIn(tween(200)) togetherWith (slideOutHorizontally(tween(260)) { it / 3 } + fadeOut(tween(200)))
                        }
                    },
                    label = "screen",
                ) { id ->
                    if (id == null) InboxScreen(model) else SessionScreen(model, id)
                }
                BackHandler(enabled = model.session != null) { model.close() }
            }
        }
        // Asked once, as the desktop asks on first launch; the inbox's menu changes it.
        if (Telemetry.asking && model.demo == null) {
            AlertDialog(
                onDismissRequest = {},
                title = { Text("Send crash reports?", style = geist(18f, FontWeight.SemiBold)) },
                text = {
                    Text(
                        "They tell us what breaks. A report holds the error and where it happened, never your " +
                            "sessions, prompts or code. Nothing is sent unless you say yes. Change it any time " +
                            "in the inbox's menu.",
                        style = Type.callout,
                        color = c.textMuted,
                    )
                },
                confirmButton = {
                    TextButton(onClick = { Telemetry.answer(true) }) {
                        Text("Send reports", color = c.amberInk, style = geist(15f, FontWeight.Medium))
                    }
                },
                dismissButton = {
                    TextButton(onClick = { Telemetry.answer(false) }) {
                        Text("Don't send", color = c.textMuted, style = geist(15f, FontWeight.Medium))
                    }
                },
                containerColor = c.raised,
            )
        }
        model.notice?.let { text ->
            AlertDialog(
                onDismissRequest = { model.notice = null },
                confirmButton = { TextButton(onClick = { model.notice = null }) { Text("OK", color = c.amberInk) } },
                title = { Text("Arakun", style = geist(18f, FontWeight.SemiBold)) },
                text = { Text(text, style = Type.callout, color = c.textMuted) },
                containerColor = c.raised,
            )
        }
    }
}
