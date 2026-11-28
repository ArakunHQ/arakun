package com.arakun.android

import android.graphics.BitmapFactory
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import okhttp3.Request
import java.util.concurrent.TimeUnit

/** A picture of the transcript, fetched and drawn: the bytes stay for sharing it. */
class Picture(val bitmap: ImageBitmap, val bytes: ByteArray, val mime: String)

/**
 * The pictures of the session on screen, as the phone page's PicStore keeps
 * them. The relay serves no images, so the computer's own `/…` links are
 * fetched inside the sealed channel; `https:` ones come from the web, only
 * once the person taps to load one (see ui/Pictures.kt). Each is
 * fetched once while it is kept, and a failure is tried again on the next
 * ask. Never log a link or what came back: both can name private files.
 */
class Pictures(
    private val scope: CoroutineScope,
    /** Asks the computer for `path` through the relay; null when no link is up. */
    private val relay: suspend (String) -> Pair<Int, ByteArray>?,
) {
    private val got = object : LinkedHashMap<String, Picture>(16, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, Picture>): Boolean =
            size > 1 && weight() > BUDGET
    }
    private val flying = HashMap<String, Deferred<Picture?>>()
    // Bumped by [clear]: an answer for a session that went is not kept.
    private var era = 0

    private fun weight(): Long = got.values.sumOf { it.bytes.size.toLong() + it.bitmap.width * it.bitmap.height * 4L }

    /** The picture already here, if any. Call on the main thread. */
    fun cached(url: String): Picture? = got[url]

    /** The picture at `url`, fetched unless it is here or on its way. Call on the main thread. */
    suspend fun load(url: String): Picture? {
        got[url]?.let { return it }
        val p = flying.getOrPut(url) {
            val mine = era
            scope.async {
                val bytes = try {
                    fetch(url)
                } catch (_: Exception) {
                    null
                }
                val pic = bytes?.let { withContext(Dispatchers.Default) { decode(it) } }
                if (pic != null && mine == era) got[url] = pic
                if (mine == era) flying.remove(url)
                pic
            }
        }
        return p.await()
    }

    /** The session went: its pictures go with it. */
    fun clear() {
        era++
        got.clear()
        flying.clear()
    }

    private suspend fun fetch(url: String): ByteArray? = when {
        url.startsWith("/") -> relay(url)?.takeIf { it.first == 200 }?.second
        url.startsWith("https://", ignoreCase = true) -> withContext(Dispatchers.IO) { web(url) }
        else -> null
    }?.takeIf { it.isNotEmpty() && it.size <= MAX_BYTES }

    private fun web(url: String): ByteArray? {
        val req = Request.Builder().url(url).build()
        return web.newCall(req).execute().use { res ->
            val body = res.body
            if (!res.isSuccessful || body.contentLength() > MAX_BYTES) return null
            // Read no more than a picture may be, whatever the server says.
            val source = body.source()
            if (source.request(MAX_BYTES + 1L)) return null
            source.buffer.readByteArray()
        }
    }

    companion object {
        /** The computer sends no image bigger than this (MAX_IMAGE_BYTES in core/src/agent/attach.rs). */
        const val MAX_BYTES = 5 * 1024 * 1024

        /** The longest side a picture is drawn at: sharp full screen, without a huge bitmap. */
        private const val MAX_SIDE = 2048

        /** About what the kept pictures may take, bitmaps and bytes. */
        private const val BUDGET = 96L * 1024 * 1024

        private val web: OkHttpClient by lazy {
            OkHttpClient.Builder()
                .callTimeout(30, TimeUnit.SECONDS)
                // Only the host the person agreed to load from: no hop elsewhere.
                .followRedirects(false)
                .followSslRedirects(false)
                .build()
        }

        /** The bitmap of `bytes`, scaled down by powers of two to [MAX_SIDE]; null when it is no picture. */
        fun decode(bytes: ByteArray): Picture? {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
            if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
            var sample = 1
            while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= MAX_SIDE) sample *= 2
            val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size, BitmapFactory.Options().apply { inSampleSize = sample })
                ?: return null
            return Picture(bitmap.asImageBitmap(), bytes, bounds.outMimeType ?: "image/*")
        }
    }
}
