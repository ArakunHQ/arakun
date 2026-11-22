// The brand book's tokens (.claude/skills/arakun-brand/reference.md): black
// and white grounds with no tint, one amber accent, Geist and Geist Mono.
// Every screen takes its colors, type and corners from here.
package com.arakun.android.ui

import androidx.annotation.DrawableRes
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.TextSelectionColors
import androidx.compose.material3.Icon
import androidx.compose.material3.LocalContentColor
import androidx.compose.material3.LocalTextStyle
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.material3.ripple
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.foundation.clickable
import androidx.compose.foundation.text.selection.LocalTextSelectionColors
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import com.arakun.android.R

/** The colors that differ between the dark and the light book. */
@Immutable
data class Palette(
    /** The page background. */
    val ground: Color,
    /** Cards, sheets and menus. */
    val raised: Color,
    /** Wells and code. */
    val deep: Color,
    val line: Color,
    val lineStrong: Color,
    val text: Color,
    val textMuted: Color,
    val textFaint: Color,
    /** Amber as text or a thin line: darker on white so it stays readable. */
    val amberInk: Color,
    /** Amber behind something, faintly. */
    val amberSoft: Color,
    // Status: state only, always with a word beside it.
    val success: Color,
    val failure: Color,
    val attention: Color,
    val dark: Boolean,
) {
    companion object {
        val current: Palette
            @Composable @ReadOnlyComposable get() = LocalPalette.current
    }
}

private val DarkBook = Palette(
    ground = Color(0xFF0B0B0B), raised = Color(0xFF161616), deep = Color(0xFF050505),
    line = Color(0xFF1F1F1F), lineStrong = Color(0xFF2B2B2B),
    text = Color(0xFFF2F2F2), textMuted = Color(0xFFA3A3A3), textFaint = Color(0xFF737373),
    amberInk = Color(0xFFFFB547), amberSoft = Color(0xFF2A1F0D),
    success = Color(0xFF3FB950), failure = Color(0xFFF85149), attention = Color(0xFFD29922),
    dark = true,
)

private val LightBook = Palette(
    ground = Color(0xFFFFFFFF), raised = Color(0xFFF7F7F7), deep = Color(0xFFF2F2F2),
    line = Color(0xFFEBEBEB), lineStrong = Color(0xFFDCDCDC),
    text = Color(0xFF0B0B0B), textMuted = Color(0xFF595959), textFaint = Color(0xFF757575),
    amberInk = Color(0xFF995C00), amberSoft = Color(0xFFFFF1DB),
    success = Color(0xFF1A7F37), failure = Color(0xFFCF222E), attention = Color(0xFF9A6700),
    dark = false,
)

/** The colors that are the same on every ground. */
object Brand {
    /** The one accent, as a fill or a mark. */
    val amber = Color(0xFFFFB547)

    /** Text on an amber fill. */
    val onAmber = Color(0xFF151515)

    /** The raccoon's paper fur and its ink, as the brand's marks draw it. */
    val fur = Color(0xFFF1EFE8)
    val furInk = Color(0xFF151515)
}

private val LocalPalette = staticCompositionLocalOf { DarkBook }

val Geist = FontFamily(
    Font(R.font.geist_regular, FontWeight.Normal),
    Font(R.font.geist_medium, FontWeight.Medium),
    Font(R.font.geist_semi_bold, FontWeight.SemiBold),
)

val GeistMono = FontFamily(
    Font(R.font.geist_mono_regular, FontWeight.Normal),
    Font(R.font.geist_mono_medium, FontWeight.Medium),
    // The brand ships no semibold mono; medium stands in.
    Font(R.font.geist_mono_medium, FontWeight.SemiBold),
)

/** Geist, for everything people read. In sp, so it follows the font size setting. */
fun geist(size: Float, weight: FontWeight = FontWeight.Normal) =
    TextStyle(fontFamily = Geist, fontSize = size.sp, fontWeight = weight)

/** Geist Mono, for commands, paths and eyebrows. */
fun mono(size: Float, weight: FontWeight = FontWeight.Normal) =
    TextStyle(fontFamily = GeistMono, fontSize = size.sp, fontWeight = weight)

object Type {
    val body = geist(16f).copy(lineHeight = 23.sp)
    val callout = geist(15f).copy(lineHeight = 21.sp)
    val small = geist(13f).copy(lineHeight = 18.sp)
    val caption = geist(12f)
    val code = mono(12.5f).copy(lineHeight = 18.sp)
}

/** Corners: chips, buttons and inputs, cards, dialogs. */
object Radius {
    val chip = 4.dp
    val control = 8.dp
    val card = 12.dp
    val dialog = 16.dp
}

@Composable
fun ArakunTheme(content: @Composable () -> Unit) {
    val p = if (isSystemInDarkTheme()) DarkBook else LightBook
    val scheme = if (p.dark) {
        darkColorScheme(
            primary = Brand.amber, onPrimary = Brand.onAmber, secondary = p.amberInk,
            background = p.ground, onBackground = p.text, surface = p.ground, onSurface = p.text,
            surfaceVariant = p.raised, onSurfaceVariant = p.textMuted,
            surfaceContainerLowest = p.deep, surfaceContainerLow = p.raised, surfaceContainer = p.raised,
            surfaceContainerHigh = p.raised, surfaceContainerHighest = p.lineStrong,
            outline = p.lineStrong, outlineVariant = p.line, error = p.failure, scrim = Color.Black.copy(alpha = 0.6f),
        )
    } else {
        lightColorScheme(
            primary = Brand.amber, onPrimary = Brand.onAmber, secondary = p.amberInk,
            background = p.ground, onBackground = p.text, surface = p.ground, onSurface = p.text,
            surfaceVariant = p.raised, onSurfaceVariant = p.textMuted,
            surfaceContainerLowest = p.ground, surfaceContainerLow = p.raised, surfaceContainer = p.raised,
            surfaceContainerHigh = p.raised, surfaceContainerHighest = p.lineStrong,
            outline = p.lineStrong, outlineVariant = p.line, error = p.failure, scrim = Color.Black.copy(alpha = 0.4f),
        )
    }
    val base = Typography()
    val typography = Typography(
        displayLarge = base.displayLarge.copy(fontFamily = Geist),
        displayMedium = base.displayMedium.copy(fontFamily = Geist),
        displaySmall = base.displaySmall.copy(fontFamily = Geist),
        headlineLarge = base.headlineLarge.copy(fontFamily = Geist),
        headlineMedium = base.headlineMedium.copy(fontFamily = Geist),
        headlineSmall = base.headlineSmall.copy(fontFamily = Geist, fontWeight = FontWeight.SemiBold),
        titleLarge = base.titleLarge.copy(fontFamily = Geist, fontWeight = FontWeight.SemiBold),
        titleMedium = base.titleMedium.copy(fontFamily = Geist, fontWeight = FontWeight.SemiBold),
        titleSmall = base.titleSmall.copy(fontFamily = Geist),
        bodyLarge = Type.body,
        bodyMedium = Type.callout,
        bodySmall = Type.small,
        labelLarge = geist(15f, FontWeight.Medium),
        labelMedium = geist(13f, FontWeight.Medium),
        labelSmall = mono(11f, FontWeight.Medium),
    )
    CompositionLocalProvider(
        LocalPalette provides p,
        LocalTextSelectionColors provides TextSelectionColors(p.amberInk, Brand.amber.copy(alpha = 0.3f)),
    ) {
        MaterialTheme(colorScheme = scheme, typography = typography) {
            CompositionLocalProvider(LocalContentColor provides p.text, LocalTextStyle provides Type.body, content = content)
        }
    }
}

/** One of the brand's line icons (assets/icons, made vectors by android/tools/vectors.py). */
@Composable
fun BrandIcon(@DrawableRes id: Int, size: Dp, tint: Color, modifier: Modifier = Modifier) {
    Icon(painterResource(id), contentDescription = null, tint = tint, modifier = modifier.size(size))
}

/** The uppercase mono label over a group: faint, tracked +8%. */
@Composable
fun Eyebrow(text: String, color: Color = Palette.current.textFaint) {
    Text(
        text.uppercase(),
        style = mono(11f, FontWeight.Medium).copy(letterSpacing = 0.08.em),
        color = color,
    )
}

/** A card on the ground: raised, with a hairline. */
fun Modifier.card(fill: Color, stroke: Color, radius: Dp = Radius.card): Modifier {
    val shape = RoundedCornerShape(radius)
    return this.clip(shape).background(fill, shape).border(BorderStroke(1.dp, stroke), shape)
}

@Composable
fun Modifier.card(): Modifier {
    val c = Palette.current
    return card(c.raised, c.line)
}

/** The one primary action of a screen: an amber fill with ink text. */
@Composable
fun PrimaryButton(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    /** As tall as an outline button, for a row of them. */
    compact: Boolean = false,
    content: @Composable RowScope.() -> Unit,
) {
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val shape = RoundedCornerShape(Radius.control)
    Row(
        modifier
            .defaultMinSize(minHeight = if (compact) 40.dp else 48.dp)
            .alpha(if (!enabled) 0.4f else if (pressed) 0.85f else 1f)
            .clip(shape)
            .background(Brand.amber, shape)
            .clickable(source, ripple(color = Brand.onAmber), enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = if (compact) 14.dp else 18.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CompositionLocalProvider(
            LocalContentColor provides Brand.onAmber,
            LocalTextStyle provides geist(if (compact) 15f else 16f, FontWeight.SemiBold),
        ) { content() }
    }
}

/** Everything else that is a button: an outline on the ground. */
@Composable
fun OutlineButton(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    tone: Color = Palette.current.text,
    content: @Composable RowScope.() -> Unit,
) {
    val c = Palette.current
    val source = remember { MutableInteractionSource() }
    val pressed by source.collectIsPressedAsState()
    val shape = RoundedCornerShape(Radius.control)
    Row(
        modifier
            .defaultMinSize(minHeight = 40.dp)
            .alpha(if (enabled) 1f else 0.4f)
            .clip(shape)
            .background(if (pressed) c.lineStrong else c.raised, shape)
            .border(1.dp, c.lineStrong, shape)
            .clickable(source, ripple(color = tone), enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 14.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        CompositionLocalProvider(LocalContentColor provides tone, LocalTextStyle provides geist(15f, FontWeight.Medium)) {
            content()
        }
    }
}

@Composable
fun OutlineButton(text: String, onClick: () -> Unit, modifier: Modifier = Modifier, enabled: Boolean = true, tone: Color = Palette.current.text) {
    OutlineButton(onClick, modifier, enabled, tone) { Text(text, maxLines = 1) }
}

/** A plain clickable with no ripple, for rows that show their own pressed state. */
@Composable
fun Modifier.tap(enabled: Boolean = true, role: Role = Role.Button, onClick: () -> Unit): Modifier =
    clickable(remember { MutableInteractionSource() }, ripple(), enabled = enabled, role = role, onClick = onClick)

val ZeroPadding = PaddingValues(0.dp)

@Composable
fun Centered(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Box(modifier, contentAlignment = Alignment.Center) { content() }
}

/**
 * A text field on the brand: the caller draws its box, this draws the
 * placeholder, the amber cursor and anything beside the text.
 */
@Composable
fun Field(
    state: androidx.compose.foundation.text.input.TextFieldState,
    placeholder: String,
    modifier: Modifier = Modifier,
    style: TextStyle = Type.callout,
    lines: androidx.compose.foundation.text.input.TextFieldLineLimits =
        androidx.compose.foundation.text.input.TextFieldLineLimits.SingleLine,
    keyboardOptions: androidx.compose.foundation.text.KeyboardOptions =
        androidx.compose.foundation.text.KeyboardOptions.Default,
    onAction: (() -> Unit)? = null,
    enabled: Boolean = true,
    padding: PaddingValues = PaddingValues(horizontal = 12.dp, vertical = 10.dp),
    leading: @Composable () -> Unit = {},
    trailing: @Composable () -> Unit = {},
) {
    val c = Palette.current
    androidx.compose.foundation.text.BasicTextField(
        state = state,
        modifier = modifier,
        enabled = enabled,
        textStyle = style.copy(color = c.text),
        keyboardOptions = keyboardOptions,
        onKeyboardAction = onAction?.let { act -> androidx.compose.foundation.text.input.KeyboardActionHandler { act() } },
        lineLimits = lines,
        cursorBrush = androidx.compose.ui.graphics.SolidColor(c.amberInk),
        decorator = { inner ->
            Row(
                Modifier.padding(padding),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = if (lines == androidx.compose.foundation.text.input.TextFieldLineLimits.SingleLine) {
                    Alignment.CenterVertically
                } else {
                    Alignment.Top
                },
            ) {
                leading()
                Box(Modifier.weight(1f)) {
                    if (state.text.isEmpty()) Text(placeholder, style = style, color = c.textFaint, maxLines = 1)
                    inner()
                }
                trailing()
            }
        },
    )
}
