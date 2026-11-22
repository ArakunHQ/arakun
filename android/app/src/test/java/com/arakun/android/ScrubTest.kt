package com.arakun.android

import kotlin.test.Test
import kotlin.test.assertEquals

/** The desktop's cases (src/telemetry.rs), so the two scrub alike. */
class ScrubTest {
    @Test
    fun keepsPlainWords() {
        val s = "initialize failed: connection refused (os error 111)"
        assertEquals(s, scrub(s))
    }

    @Test
    fun removesUrlsAndTheirSecrets() {
        assertEquals(
            "Illegal character in fragment at index 40: <url>",
            scrub("Illegal character in fragment at index 40: https://abc.arakun.dev/#p=code&s=secret"),
        )
        assertEquals("auth failed: Bearer <secret>", scrub("auth failed: Bearer abc.def"))
    }

    @Test
    fun removesPathsEmailsAndValues() {
        assertEquals("cannot open <path>: denied", scrub("cannot open /data/user/0/com.arakun.android/x: denied"))
        assertEquals("no such file: '<path>'", scrub("no such file: '/srv/acme/repo/main.rs'"))
        assertEquals("from <email> failed", scrub("from ann@example.com failed"))
        assertEquals("token=<secret> rejected", scrub("token=ghp_abcdefghijklmnop rejected"))
        assertEquals("login password=<secret> failed", scrub("login password=hunter2 failed"))
        assertEquals("\"api_key\":\"<secret>\"", scrub("\"api_key\":\"abc\""))
        assertEquals("status=404", scrub("status=404"))
    }

    @Test
    fun removesSecretShapedWords() {
        assertEquals("key <secret> used", scrub("key 0123456789abcdef0123 used"))
        assertEquals("ticket <secret>", scrub("ticket aB3dE5gH7jK9mN1pQ3sT"))
    }

    @Test
    fun cutsLongMessages() {
        assertEquals(301, scrub("a ".repeat(400)).length)
    }
}
