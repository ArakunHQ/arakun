package com.arakun.android.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.text.input.clearText
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import com.arakun.android.AppModel
import com.arakun.android.R

/** The first screen: scan the code in Arakun's Phone tab, or paste its link. */
@Composable
fun PairScreen(model: AppModel) {
    val c = Palette.current
    var scanning by rememberSaveable { mutableStateOf(false) }
    val pasted = rememberTextFieldState()
    var pasting by remember { mutableStateOf(false) }
    val pairing = model.connection == AppModel.Connection.Pairing
    // What went wrong, from pairing or from a computer that did not prove its key.
    val problem = model.pairError ?: (model.connection as? AppModel.Connection.Failed)?.why
    fun pairPasted() {
        // A typo stays in the field to fix; only a link that pairs goes.
        if (model.pair(pasted.text.toString())) pasted.clearText()
    }

    Box(Modifier.fillMaxSize().background(c.ground).safeDrawingPadding().imePadding(), contentAlignment = Alignment.TopCenter) {
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
                        pairing -> Mood.THINKING
                        problem != null -> Mood.ERROR
                        else -> Mood.IDLE
                    },
                    width = 132.dp,
                    modifier = Modifier.padding(top = 24.dp),
                )
                Text("Arakun", style = geist(40f, FontWeight.SemiBold).copy(letterSpacing = (-0.04).em))
                Text(
                    "Your coding sessions, on this phone.",
                    style = geist(17f),
                    color = c.textMuted,
                    textAlign = TextAlign.Center,
                )
            }

            Column(
                Modifier.fillMaxWidth().card().padding(18.dp),
                verticalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                Eyebrow("Pair this phone")
                Step(1, "Open Arakun on your computer.")
                Step(2, "Open the Phone tab and choose the relay.")
                Step(3, "Scan the code it shows.")
            }

            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (pairing) {
                    Row(
                        Modifier.fillMaxWidth().heightIn(min = 48.dp),
                        horizontalArrangement = Arrangement.spacedBy(10.dp, Alignment.CenterHorizontally),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        CircularProgressIndicator(Modifier.size(18.dp), color = c.textMuted, strokeWidth = 2.dp)
                        Text("Pairing with your computer…", color = c.textMuted)
                    }
                } else {
                    PrimaryButton(onClick = { scanning = true }, modifier = Modifier.fillMaxWidth()) {
                        BrandIcon(R.drawable.ic_search01, 18.dp, Brand.onAmber)
                        Text("Scan the code")
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                        val shape = RoundedCornerShape(Radius.control)
                        Field(
                            state = pasted,
                            placeholder = "Or paste the pairing link",
                            style = mono(14f),
                            keyboardOptions = KeyboardOptions(
                                capitalization = KeyboardCapitalization.None,
                                autoCorrectEnabled = false,
                                keyboardType = KeyboardType.Uri,
                                imeAction = ImeAction.Go,
                            ),
                            onAction = { if (pasted.text.isNotBlank()) pairPasted() },
                            padding = PaddingValues(12.dp),
                            modifier = Modifier
                                .weight(1f)
                                .heightIn(min = 44.dp)
                                .onFocusChanged { pasting = it.isFocused }
                                .background(c.deep, shape)
                                .border(1.dp, if (pasting) c.amberInk else c.lineStrong, shape),
                        )
                        OutlineButton("Pair", ::pairPasted, enabled = pasted.text.isNotBlank())
                    }
                }
                if (problem != null) {
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                        BrandIcon(R.drawable.ic_alert_circle, 15.dp, c.failure, Modifier.padding(top = 2.dp))
                        Text(problem, style = Type.small, color = c.failure)
                    }
                }
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

    if (scanning) {
        ScanSheet(onClose = { scanning = false }) { code ->
            scanning = false
            model.pair(code)
        }
    }
}

@Composable
private fun Step(n: Int, text: String) {
    val c = Palette.current
    Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.size(22.dp).border(1.dp, c.lineStrong, CircleShape), contentAlignment = Alignment.Center) {
            Text("$n", style = mono(12f, FontWeight.Medium), color = c.textMuted)
        }
        Text(text, style = Type.callout)
    }
}
