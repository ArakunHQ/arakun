package com.arakun.android.ui

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.arakun.android.AppModel
import com.arakun.android.R

/**
 * The first screen: sign in with GitHub to reach the account's computers
 * through the relay. GitHub's device flow: the app shows a code, the person
 * enters it on GitHub, and the app waits for GitHub's yes.
 */
@Composable
fun SignInScreen(model: AppModel) {
    val c = Palette.current
    val step = model.signingIn
    val problem = model.accountSaid

    Box(Modifier.fillMaxSize().background(c.ground).safeDrawingPadding(), contentAlignment = Alignment.TopCenter) {
        Column(
            Modifier
                .widthIn(max = 520.dp)
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 24.dp)
                .padding(bottom = 32.dp),
            verticalArrangement = Arrangement.spacedBy(32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(16.dp)) {
                Mascot(
                    mood = when {
                        step != null -> Mood.THINKING
                        problem != null -> Mood.ERROR
                        else -> Mood.IDLE
                    },
                    width = 132.dp,
                    modifier = Modifier.padding(top = 24.dp),
                )
                Text("Arakun", style = geist(40f, FontWeight.SemiBold).copy(letterSpacing = (-0.04).em))
                Text(
                    "Your coding sessions, on this phone.",
                    style = Type.body,
                    color = c.textMuted,
                    textAlign = TextAlign.Center,
                )
            }

            when (step) {
                is AppModel.SignIn.Code -> CodeCard(model, step)
                AppModel.SignIn.Asking -> Waiting("Asking GitHub for a code…")
                AppModel.SignIn.Finishing -> Waiting("Signing in…")
                null -> Column(
                    Modifier.fillMaxWidth().card().padding(18.dp),
                    verticalArrangement = Arrangement.spacedBy(14.dp),
                ) {
                    Eyebrow("Sign in to reach your computers")
                    Text(
                        "Sign in with the GitHub account you use in Arakun on your computer.",
                        style = Type.callout,
                    )
                    PrimaryButton(onClick = model::signIn, modifier = Modifier.fillMaxWidth()) {
                        Text("Sign in with GitHub")
                    }
                }
            }

            if (problem != null) Problem(problem)

            // No account is needed on your own network: the computer's page pairs there.
            Column(
                Modifier.fillMaxWidth().card().padding(18.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Eyebrow("Pair on this network")
                Text(
                    "No account needed. In Arakun's Phone tab, choose Same Wi-Fi or Tailscale, " +
                        "then scan its code with this phone's camera. It opens in the browser.",
                    style = Type.callout,
                    color = c.textMuted,
                )
            }

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                BrandIcon(R.drawable.ic_link01, 14.dp, c.textFaint)
                Text(
                    "End to end encrypted. The relay only passes bytes along.",
                    style = mono(11.5f),
                    color = c.textFaint,
                    textAlign = TextAlign.Center,
                )
            }
        }
    }
}

/** The code to enter on GitHub, large, with Copy and Open GitHub. */
@Composable
private fun CodeCard(model: AppModel, step: AppModel.SignIn.Code) {
    val c = Palette.current
    val context = LocalContext.current
    val uris = LocalUriHandler.current
    Column(
        Modifier.fillMaxWidth().card().padding(18.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Eyebrow("Enter this code on GitHub")
        Text(
            step.userCode,
            style = mono(34f, FontWeight.Medium).copy(letterSpacing = 0.08.em),
            modifier = Modifier.semantics { contentDescription = "Code ${step.userCode.toList().joinToString(" ")}" },
        )
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), modifier = Modifier.fillMaxWidth()) {
            OutlineButton(
                "Copy",
                onClick = {
                    context.getSystemService(ClipboardManager::class.java)
                        ?.setPrimaryClip(ClipData.newPlainText("GitHub code", step.userCode))
                },
                modifier = Modifier.weight(1f),
            )
            // A phone with no browser throws; the code can still be typed by hand.
            PrimaryButton(onClick = { runCatching { uris.openUri(step.uri) } }, modifier = Modifier.weight(1f), compact = true) {
                Text("Open GitHub")
            }
        }
        Row(
            horizontalArrangement = Arrangement.spacedBy(10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            CircularProgressIndicator(Modifier.size(14.dp), color = c.textMuted, strokeWidth = 2.dp)
            Text("Waiting for GitHub…", style = Type.small, color = c.textMuted)
        }
        OutlineButton("Cancel", model::cancelSignIn)
    }
}

@Composable
private fun Waiting(text: String) {
    val c = Palette.current
    Row(
        Modifier.fillMaxWidth().heightIn(min = 48.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CircularProgressIndicator(Modifier.size(18.dp), color = c.textMuted, strokeWidth = 2.dp)
        Text(text, color = c.textMuted)
    }
}
