package com.arakun.android.ui

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.clearText
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.arakun.android.AppModel
import com.arakun.android.R
import com.arakun.kit.LinkKind

/**
 * The first screen: sign in to reach the account's computers through the
 * relay. With GitHub's device flow, the app shows a code, the person enters it
 * on GitHub, and the app waits for GitHub's yes. By email, the account host
 * mails a code and the person types it here.
 */
@Composable
fun SignInScreen(model: AppModel) {
    val c = Palette.current
    val step = model.signingIn
    val problem = model.accountSaid
    // Here, not in the cards: the address stays when the person goes back to it.
    val email = rememberTextFieldState()
    val code = rememberTextFieldState()

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
                        problem != null -> Mood.ERROR
                        step != null -> Mood.THINKING
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
                is AppModel.SignIn.Mailing -> Waiting("Sending a code to ${step.email}…")
                is AppModel.SignIn.Mailed -> MailedCard(model, step, code)
                null -> Column(
                    Modifier.fillMaxWidth().card().padding(18.dp),
                    verticalArrangement = Arrangement.spacedBy(14.dp),
                ) {
                    Eyebrow("Sign in to reach your computers")
                    Text(
                        "Use the account you use in Arakun on your computer: GitHub, or a code we email you.",
                        style = Type.callout,
                    )
                    if (model.pairedByLink) {
                        Text(LinkKind.PAIRED_BY_LINK, style = Type.small, color = c.textMuted)
                    }
                    PrimaryButton(onClick = model::signIn, modifier = Modifier.fillMaxWidth()) {
                        Text("Sign in with GitHub")
                    }
                    EmailEntry(model, email)
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

/** The address to mail a code to, under GitHub's button. */
@Composable
private fun EmailEntry(model: AppModel, email: TextFieldState) {
    val c = Palette.current
    val ready = '@' in email.text
    val send = { if ('@' in email.text) model.emailCode(email.text.toString()) }
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.weight(1f).height(1.dp).background(c.line))
        Text("or with your email", style = Type.small, color = c.textFaint)
        Box(Modifier.weight(1f).height(1.dp).background(c.line))
    }
    val shape = RoundedCornerShape(Radius.control)
    Field(
        state = email,
        placeholder = "Your email address",
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Email, imeAction = ImeAction.Send),
        onAction = send,
        modifier = Modifier.fillMaxWidth().heightIn(min = 44.dp).background(c.deep, shape).border(1.dp, c.lineStrong, shape)
            .semantics { contentDescription = "Your email address" },
    )
    OutlineButton("Email me a code", send, modifier = Modifier.fillMaxWidth(), enabled = ready)
    Text("No account yet? Either way makes one.", style = Type.small, color = c.textMuted)
}

/** The box for the code mailed to the address, with a new code and another address. */
@Composable
private fun MailedCard(model: AppModel, step: AppModel.SignIn.Mailed, code: TextFieldState) {
    val c = Palette.current
    val focus = remember { FocusRequester() }
    LaunchedEffect(step.email) {
        code.clearText()
        runCatching { focus.requestFocus() }
    }
    val ready = step.busy == null && code.text.count { it.isDigit() } == 6
    val check = { if (ready) model.checkCode(code.text.toString()) }
    Column(
        Modifier.fillMaxWidth().card().padding(18.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Eyebrow("Check your email")
        Text("We sent a six-digit code to ${step.email}. Type it here.", style = Type.callout)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            val shape = RoundedCornerShape(Radius.control)
            Field(
                state = code,
                placeholder = "123456",
                style = mono(18f, FontWeight.Medium),
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword, imeAction = ImeAction.Go),
                onAction = check,
                enabled = step.busy != AppModel.EmailBusy.CHECKING,
                modifier = Modifier.weight(1f).heightIn(min = 44.dp).background(c.deep, shape).border(1.dp, c.lineStrong, shape)
                    .focusRequester(focus)
                    .semantics { contentDescription = "Code" },
            )
            PrimaryButton(onClick = check, enabled = ready, compact = true) {
                Text(if (step.busy == AppModel.EmailBusy.CHECKING) "Signing in…" else "Sign in")
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
            if (step.busy != null) CircularProgressIndicator(Modifier.size(14.dp), color = c.textMuted, strokeWidth = 2.dp)
            Text(
                when (step.busy) {
                    AppModel.EmailBusy.CHECKING -> "Checking the code…"
                    AppModel.EmailBusy.RESENDING -> "Sending a new code…"
                    null -> "It works once, for 10 minutes. Not there? Look in spam."
                },
                style = Type.small,
                color = c.textMuted,
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), modifier = Modifier.fillMaxWidth()) {
            OutlineButton(
                "Use another address",
                model::cancelSignIn,
                modifier = Modifier.weight(1f),
                enabled = step.busy != AppModel.EmailBusy.CHECKING,
            )
            OutlineButton(
                "Send a new code",
                {
                    code.clearText()
                    model.emailCode(step.email)
                },
                modifier = Modifier.weight(1f),
                enabled = step.busy == null,
            )
        }
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
