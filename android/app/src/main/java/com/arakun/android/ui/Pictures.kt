package com.arakun.android.ui

import android.content.ClipData
import android.content.Context
import android.content.Intent
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.gestures.detectTransformGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.compositionLocalOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.FileProvider
import com.arakun.android.Picture
import com.arakun.android.Pictures
import com.arakun.android.R
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

/**
 * Where the transcript's pictures come from. `online` is part of it so a
 * picture that failed while the link was down is asked for again once it is up.
 */
class PictureSource(val pictures: Pictures?, val online: Boolean)

val LocalPictures = compositionLocalOf { PictureSource(null, false) }

/** The picture at `url` once it is here; null while it loads or when it failed. */
@Composable
private fun rememberPicture(url: String?): Picture? {
    val source = LocalPictures.current
    val pictures = source.pictures
    var pic by remember(url, pictures) { mutableStateOf(url?.let { pictures?.cached(it) }) }
    LaunchedEffect(url, pictures, source.online) {
        if (pic == null && url != null && pictures != null) pic = pictures.load(url)
    }
    return pic
}

/** A picture of the agent's, as wide as the screen at most; tap it to see it whole. */
@Composable
fun AgentPicture(url: String, alt: String) {
    val pic = rememberPicture(url)
    val name = pictureName(url, alt)
    if (pic == null) {
        Placeholder(name, Modifier.fillMaxWidth())
        return
    }
    var open by remember { mutableStateOf(false) }
    val c = Palette.current
    val shape = RoundedCornerShape(Radius.control)
    // A small picture keeps its size; a big one fits the width.
    val natural = with(LocalDensity.current) { pic.bitmap.width.toDp() }
    Image(
        pic.bitmap,
        contentDescription = null,
        contentScale = ContentScale.Fit,
        modifier = Modifier
            .widthIn(max = natural)
            .fillMaxWidth()
            .aspectRatio(pic.bitmap.width.toFloat() / pic.bitmap.height)
            .clip(shape)
            .border(1.dp, c.line, shape)
            .tap { open = true }
            .semantics { contentDescription = "Picture $name. Open full screen" },
    )
    if (open) PictureView(pic, name) { open = false }
}

/** A picture a prompt mentions, as a thumbnail; its name until it is here. */
@Composable
fun PromptPicture(name: String, url: String?, size: Dp = 112.dp) {
    val pic = rememberPicture(url)
    if (pic == null) {
        Placeholder(name)
        return
    }
    var open by remember { mutableStateOf(false) }
    val c = Palette.current
    val shape = RoundedCornerShape(Radius.control)
    Image(
        pic.bitmap,
        contentDescription = null,
        contentScale = ContentScale.Crop,
        modifier = Modifier
            .size(size)
            .clip(shape)
            .border(1.dp, c.line, shape)
            .tap { open = true }
            .semantics { contentDescription = "Picture $name. Open full screen" },
    )
    if (open) PictureView(pic, name) { open = false }
}

/** A picture's name in a well: while it loads, when it failed, or when there is nothing to load. */
@Composable
private fun Placeholder(name: String, modifier: Modifier = Modifier) {
    val c = Palette.current
    Row(
        modifier.background(c.deep, RoundedCornerShape(Radius.control)).padding(horizontal = 10.dp, vertical = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        BrandIcon(R.drawable.ic_image01, 13.dp, c.textMuted)
        Text(name, style = mono(11.5f), color = c.textMuted, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
    }
}

/** A picture over everything: pinch or double-tap to zoom, share it, or close. */
@Composable
private fun PictureView(pic: Picture, name: String, onClose: () -> Unit) {
    val c = Palette.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var zoom by remember { mutableFloatStateOf(1f) }
    var pan by remember { mutableStateOf(Offset.Zero) }
    Dialog(onClose, DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Box(Modifier.fillMaxSize().background(c.ground)) {
            Image(
                pic.bitmap,
                contentDescription = name,
                contentScale = ContentScale.Fit,
                modifier = Modifier
                    .fillMaxSize()
                    .pointerInput(Unit) {
                        detectTransformGestures { _, move, scale, _ ->
                            zoom = (zoom * scale).coerceIn(1f, 8f)
                            // Back to the middle once it is its own size again.
                            pan = if (zoom == 1f) Offset.Zero else pan + move
                        }
                    }
                    .pointerInput(Unit) {
                        detectTapGestures(onDoubleTap = {
                            zoom = if (zoom > 1f) 1f else 2.5f
                            pan = Offset.Zero
                        })
                    }
                    .graphicsLayer {
                        scaleX = zoom
                        scaleY = zoom
                        translationX = pan.x
                        translationY = pan.y
                    },
            )
            Row(
                Modifier.fillMaxWidth().safeDrawingPadding().padding(horizontal = 12.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RoundButton(R.drawable.ic_cancel01, "Close", onClose)
                Spacer(Modifier.weight(1f))
                RoundButton(R.drawable.ic_share02, "Share") {
                    scope.launch { share(context, pic, name) }
                }
            }
        }
    }
}

@Composable
private fun RoundButton(icon: Int, label: String, onClick: () -> Unit) {
    val c = Palette.current
    Box(
        Modifier
            .size(44.dp)
            .clip(CircleShape)
            .background(c.raised.copy(alpha = 0.85f))
            .tap(onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        BrandIcon(icon, 20.dp, c.text)
    }
}

/** Hands the picture to another app, from a copy in the cache that only the share can read. */
private suspend fun share(context: Context, pic: Picture, name: String) {
    val file = withContext(Dispatchers.IO) {
        val dir = File(context.cacheDir, "pictures").apply { mkdirs() }
        // Only the last share is kept.
        dir.listFiles()?.forEach { it.delete() }
        File(dir, shareName(name, pic.mime)).apply { writeBytes(pic.bytes) }
    }
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.pictures", file)
    val send = Intent(Intent.ACTION_SEND)
        .setType(pic.mime)
        .putExtra(Intent.EXTRA_STREAM, uri)
        .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
    send.clipData = ClipData.newRawUri(name, uri)
    context.startActivity(Intent.createChooser(send, null))
}

/** A file name for `name` that is safe on disk and ends as its type does. */
fun shareName(name: String, mime: String): String {
    val ext = when (mime) {
        "image/png" -> "png"
        "image/jpeg" -> "jpg"
        "image/gif" -> "gif"
        "image/webp" -> "webp"
        "image/heif", "image/heic" -> "heic"
        else -> null
    }
    val clean = name.replace(Regex("[^A-Za-z0-9._ -]"), "_").trim().trimStart('.').take(80).ifEmpty { "picture" }
    if (ext == null || clean.endsWith(".$ext", ignoreCase = true)) return clean
    // Another picture type's ending would mislead the app it goes to.
    val stem = clean.replace(Regex("\\.(png|jpe?g|gif|webp|heic|heif|bmp)$", RegexOption.IGNORE_CASE), "").ifEmpty { "picture" }
    return "$stem.$ext"
}
