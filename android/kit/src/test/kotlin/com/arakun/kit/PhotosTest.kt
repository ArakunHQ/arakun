package com.arakun.kit

import kotlinx.serialization.json.JsonElement
import java.util.Base64
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotEquals
import kotlin.test.assertTrue

class PhotosTest {
    @Test
    fun aPhotoGoesInPiecesThatEachDecodeAlone() {
        val bytes = ByteArray(Photos.PIECE) { it.toByte() } // base64 a third longer: two pieces
        val pieces = Photos.pieces("s1", "u1", "photo.jpg", bytes)
        assertEquals(2, pieces.size)
        val out = pieces.mapIndexed { i, piece ->
            val p: JsonElement = piece
            assertEquals("upload", p["type"].str)
            assertEquals("s1", p["session"].str)
            assertEquals("u1", p["upload"].str)
            assertEquals("photo.jpg", p["name"].str)
            assertEquals(i, p["part"].int)
            assertEquals(2, p["of"].int)
            val data = p["data"].str
            assertTrue(data.length <= Photos.PIECE)
            // The computer decodes each piece on its own, padding and all.
            Base64.getDecoder().decode(data)
        }
        assertTrue(bytes.contentEquals(out.reduce { a, b -> a + b }))
    }

    @Test
    fun anEmptyPhotoIsStillOnePiece() {
        val pieces = Photos.pieces("", "u", "a.jpg", ByteArray(0))
        assertEquals(1, pieces.size)
        assertEquals("", (pieces[0] as JsonElement)["data"].str)
    }

    @Test
    fun idsDifferAndAPromptOfPhotosSaysSo() {
        assertNotEquals(Photos.newId(0, 1), Photos.newId(1, 1))
        assertEquals("look", Photos.textFor("look", 2))
        assertEquals("(photo)", Photos.textFor("  ", 1))
        assertEquals("(3 photos)", Photos.textFor("", 3))
        assertEquals("", Photos.textFor("", 0))
    }
}
