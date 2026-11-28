package com.arakun.android.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import com.arakun.android.R
import com.arakun.android.Shot
import com.arakun.android.Shots
import kotlinx.coroutines.launch
import java.io.File

/** The most photos one prompt carries; the computer keeps few half-sent ones at once. */
const val MAX_SHOTS = 8

/**
 * The photo button beside a prompt: the system photo picker, which needs no
 * storage permission, or the camera. Each picture read comes to `add`; one
 * that does not read says so through `failed`.
 */
@Composable
fun PhotoButton(
    count: Int,
    add: (Shot) -> Unit,
    failed: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val c = Palette.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var menu by remember { mutableStateOf(false) }
    // Where the camera writes; kept across the activity being remade while it is open.
    var pending by rememberSaveable { mutableStateOf<Uri?>(null) }
    val room = MAX_SHOTS - count

    fun take(uris: List<Uri>, after: () -> Unit = {}) {
        if (uris.isEmpty()) return after()
        scope.launch {
            var bad = 0
            for (uri in uris) Shots.read(context, uri)?.let(add) ?: bad++
            after()
            if (bad > 0) failed(if (bad == 1) "That photo could not be read." else "$bad photos could not be read.")
        }
    }

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(MAX_SHOTS)) { uris ->
        // The picker can let more through than were left.
        take(uris.take(maxOf(0, MAX_SHOTS - count)))
    }
    val camera = rememberLauncherForActivityResult(ActivityResultContracts.TakePicture()) { took ->
        val uri = pending
        pending = null
        if (uri == null) return@rememberLauncherForActivityResult
        // Read, then the full-size file goes: the shot holds what is sent.
        take(if (took) listOf(uri) else emptyList()) { forgetCameraFile(context, uri) }
    }

    Box(modifier) {
        Box(
            Modifier
                .padding(6.dp)
                .size(34.dp)
                .alpha(if (room > 0) 1f else 0.5f)
                .clip(RoundedCornerShape(Radius.control))
                .tap(enabled = room > 0) { menu = true }
                .semantics { contentDescription = "Attach photos" },
            contentAlignment = Alignment.Center,
        ) {
            BrandIcon(R.drawable.ic_image_add01, 18.dp, c.textMuted)
        }
        DropdownMenu(
            expanded = menu,
            onDismissRequest = { menu = false },
            containerColor = c.raised,
            shape = RoundedCornerShape(Radius.card),
        ) {
            DropdownMenuItem(
                text = { Text("Choose photos", style = Type.callout) },
                onClick = {
                    menu = false
                    picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                },
            )
            DropdownMenuItem(
                text = { Text("Take a photo", style = Type.callout) },
                onClick = {
                    menu = false
                    val uri = newCameraUri(context)
                    pending = uri
                    try {
                        camera.launch(uri)
                    } catch (_: ActivityNotFoundException) {
                        pending = null
                        forgetCameraFile(context, uri)
                        failed("This phone has no camera app to take a photo with.")
                    }
                },
            )
        }
    }
}

/** The photos waiting to go, small, each with a button that takes it out. */
@Composable
fun ShotChips(shots: List<Shot>, remove: (Int) -> Unit, modifier: Modifier = Modifier) {
    if (shots.isEmpty()) return
    val c = Palette.current
    Row(
        modifier.horizontalScroll(rememberScrollState()),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        shots.forEachIndexed { i, shot ->
            Box(
                Modifier
                    .size(56.dp)
                    .clip(RoundedCornerShape(Radius.control))
                    .tap { remove(i) }
                    .semantics { contentDescription = "Remove photo ${i + 1}" },
            ) {
                Image(shot.thumb, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.size(56.dp))
                Box(
                    Modifier.align(Alignment.TopEnd).padding(3.dp).size(18.dp).background(c.ground.copy(alpha = 0.85f), CircleShape),
                    contentAlignment = Alignment.Center,
                ) {
                    BrandIcon(R.drawable.ic_cancel01, 11.dp, c.text)
                }
            }
        }
    }
}

/** A new file for the camera to write, in the cache folder FileProvider shares (xml/shared_pictures.xml). */
private fun newCameraUri(context: Context): Uri {
    val dir = File(context.cacheDir, "camera").apply { mkdirs() }
    // One photo is taken at a time: any file here is left from one that never came back.
    dir.listFiles()?.forEach { it.delete() }
    val file = File(dir, "shot-${System.currentTimeMillis()}.jpg")
    return FileProvider.getUriForFile(context, "${context.packageName}.pictures", file)
}

private fun forgetCameraFile(context: Context, uri: Uri) {
    val name = uri.lastPathSegment?.takeIf { it.isNotEmpty() && '/' !in it } ?: return
    File(File(context.cacheDir, "camera"), name).delete()
}
