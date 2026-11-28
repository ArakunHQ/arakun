package com.arakun.android.ui

import kotlin.test.Test
import kotlin.test.assertEquals

/** How the agent's words split around its pictures, and what a picture is called. */
class PicturesTest {
    @Test
    fun takesOutTheComputersAndTheWebsPictures() {
        val s = "Here it is:\n\n![the plot](/image?s=ab&p=%2Ftmp%2Fplot.png)\n\nand ![](https://e.com/a.png) too."
        assertEquals(
            listOf(
                Block.Words("Here it is:"),
                Block.Pic("/image?s=ab&p=%2Ftmp%2Fplot.png", "the plot"),
                Block.Words("and "),
                Block.Pic("https://e.com/a.png", ""),
                Block.Words(" too."),
            ),
            blocks(s),
        )
    }

    @Test
    fun leavesTheRestAsWritten() {
        // Plain words, code, a link that is no picture, a picture the app cannot fetch.
        for (s in listOf(
            "no pictures here",
            "see `![x](/image?a)` in code",
            "```\n![x](/image?a)\n```",
            "a [link](/image?a)",
            "![x](http://e.com/a.png) and ![y](//e.com/b.png) and ![z](plot.png)",
            "![open](/image?a",
        )) {
            assertEquals(listOf(Block.Words(s)), blocks(s), s)
        }
        assertEquals(emptyList(), blocks(""))
        assertEquals(listOf(Block.Pic("/image?a", "x")), blocks("![x](/image?a)"))
    }

    @Test
    fun namesAPictureByItsDescriptionOrFile() {
        assertEquals("the plot", pictureName("/image?s=ab&p=%2Ftmp%2Fplot.png", "the plot"))
        assertEquals("my shot.png", pictureName("/image?s=ab&p=%2Fhome%2Fme%2Fmy%20shot.png", ""))
        assertEquals("a.png", pictureName("https://e.com/x/a.png?v=2", " "))
        assertEquals("Picture", pictureName("/", ""))
    }

    @Test
    fun sharesUnderASafeNameThatEndsAsItsType() {
        assertEquals("shot.png", shareName("shot.png", "image/png"))
        assertEquals("shot.jpg", shareName("shot.png", "image/jpeg"))
        assertEquals("the plot.png", shareName("the plot", "image/png"))
        assertEquals("v1.2 plot.png", shareName("v1.2 plot", "image/png"))
        assertEquals("_etc_passwd.png", shareName("/etc/passwd", "image/png"))
        assertEquals("picture.png", shareName("..", "image/png"))
        assertEquals("odd", shareName("odd", "image/x-unknown"))
    }
}
