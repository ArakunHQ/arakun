// The platform ExifInterface is only wrong before Android 7, and the app needs 8.
@file:SuppressLint("ExifInterface")

package com.arakun.android

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.graphics.Matrix
import android.media.ExifInterface
import android.net.Uri
import android.os.Build
import android.provider.OpenableColumns
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.core.graphics.scale
import com.arakun.kit.Photos
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream

/** A photo waiting in the composer: the JPEG that goes out, and a small picture for its chip. */
class Shot(val name: String, val jpeg: ByteArray, val thumb: ImageBitmap)

/**
 * Photos picked or taken on the phone, made small enough to send, as the
 * phone page's lib/shots.ts does: drawn at most [LONGEST_SIDE] on the long
 * side and re-encoded as JPEG, so a few hundred kilobytes go over the relay
 * rather than a camera's full file. Never log a name or the bytes.
 */
object Shots {
    /** As the phone page: about what the agent looks at, no more. */
    const val LONGEST_SIDE = 1568
    private const val THUMB_SIDE = 160
    private const val QUALITY = 82

    /** `uri` as a [Shot], or null when it is not a picture that decodes. */
    suspend fun read(context: Context, uri: Uri): Shot? = withContext(Dispatchers.IO) {
        try {
            val bitmap = decode(context, uri) ?: return@withContext null
            val jpeg = encode(bitmap) ?: return@withContext null
            val (tw, th) = fit(bitmap.width, bitmap.height, THUMB_SIDE)
            val thumb = bitmap.scale(tw, th).asImageBitmap()
            Shot(jpegName(displayName(context, uri)), jpeg, thumb)
        } catch (_: Exception) {
            null
        } catch (_: OutOfMemoryError) {
            null
        }
    }

    /** The size `w` by `h` scaled down to fit `longest` on its long side; never scaled up. */
    fun fit(w: Int, h: Int, longest: Int): Pair<Int, Int> {
        val scale = minOf(1.0, longest.toDouble() / maxOf(w, h, 1))
        return maxOf(1, Math.round(w * scale).toInt()) to maxOf(1, Math.round(h * scale).toInt())
    }

    /** The largest power of two that leaves `w` by `h` at least `longest` on its long side. */
    fun sampleSize(w: Int, h: Int, longest: Int): Int {
        var sample = 1
        while (maxOf(w, h) / (sample * 2) >= longest) sample *= 2
        return sample
    }

    /** `name` with a .jpg end, since the bytes are JPEG whatever it was before. */
    fun jpegName(name: String?): String {
        val base = name?.substringBeforeLast('.')?.trim().orEmpty()
        return if (base.isEmpty()) "photo.jpg" else "$base.jpg"
    }

    private fun displayName(context: Context, uri: Uri): String? = try {
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }
    } catch (_: Exception) {
        null
    }

    /** The picture at `uri`, turned the way it was taken and scaled to [LONGEST_SIDE]. */
    private fun decode(context: Context, uri: Uri): Bitmap? {
        val resolver = context.contentResolver
        val bitmap = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            // ImageDecoder turns the picture as its EXIF says.
            ImageDecoder.decodeBitmap(ImageDecoder.createSource(resolver, uri)) { decoder, info, _ ->
                val (w, h) = fit(info.size.width, info.size.height, LONGEST_SIDE)
                decoder.setTargetSize(w, h)
                decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            }
        } else {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            resolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
            if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
            val opts = BitmapFactory.Options().apply { inSampleSize = sampleSize(bounds.outWidth, bounds.outHeight, LONGEST_SIDE) }
            val raw = resolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, opts) } ?: return null
            val degrees = resolver.openInputStream(uri)?.use { s ->
                when (ExifInterface(s).getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)) {
                    ExifInterface.ORIENTATION_ROTATE_90 -> 90f
                    ExifInterface.ORIENTATION_ROTATE_180 -> 180f
                    ExifInterface.ORIENTATION_ROTATE_270 -> 270f
                    else -> 0f
                }
            } ?: 0f
            val (w, h) = fit(raw.width, raw.height, LONGEST_SIDE)
            val m = Matrix().apply {
                postScale(w.toFloat() / raw.width, h.toFloat() / raw.height)
                postRotate(degrees)
            }
            Bitmap.createBitmap(raw, 0, 0, raw.width, raw.height, m, true)
        }
        return bitmap.takeIf { it.width > 0 && it.height > 0 }
    }

    /** `bitmap` as JPEG under [Photos.MAX_BYTES]: lower quality first, then smaller. */
    private fun encode(bitmap: Bitmap): ByteArray? {
        var b = bitmap
        repeat(6) {
            for (quality in intArrayOf(QUALITY, 65, 50)) {
                val out = ByteArrayOutputStream()
                if (!b.compress(Bitmap.CompressFormat.JPEG, quality, out)) return null
                if (out.size() <= Photos.MAX_BYTES) return out.toByteArray()
            }
            val (w, h) = fit(b.width, b.height, maxOf(b.width, b.height) * 3 / 4)
            b = b.scale(w, h)
        }
        return null
    }
}
