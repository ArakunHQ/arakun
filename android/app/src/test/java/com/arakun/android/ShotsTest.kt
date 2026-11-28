package com.arakun.android

import kotlin.test.Test
import kotlin.test.assertEquals

class ShotsTest {
    @Test
    fun aPhotoIsScaledDownToTheLongestSideAndNeverUp() {
        assertEquals(1568 to 1176, Shots.fit(4000, 3000, 1568))
        assertEquals(1176 to 1568, Shots.fit(3000, 4000, 1568))
        assertEquals(800 to 600, Shots.fit(800, 600, 1568))
        assertEquals(1568 to 1, Shots.fit(100_000, 10, 1568))
    }

    @Test
    fun theSampleSizeLeavesAtLeastTheLongestSide() {
        assertEquals(2, Shots.sampleSize(4000, 3000, 1568))
        assertEquals(1, Shots.sampleSize(3000, 2000, 1568))
        assertEquals(4, Shots.sampleSize(8000, 6000, 1568))
    }

    @Test
    fun theNameSaysJpeg() {
        assertEquals("IMG_1.jpg", Shots.jpegName("IMG_1.HEIC"))
        assertEquals("photo.jpg", Shots.jpegName(null))
        assertEquals("photo.jpg", Shots.jpegName(".png"))
        assertEquals("scan.jpg", Shots.jpegName("scan"))
    }
}
