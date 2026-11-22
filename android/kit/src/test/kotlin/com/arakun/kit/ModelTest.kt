package com.arakun.kit

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull

private fun json(s: String) = parseJson(s)!!

class ModelTest {
    @Test
    fun readsAnInbox() {
        val inbox = Inbox.of(
            json(
                """
                {"type":"inbox","host":"mac","sessions":[{"id":"s1","title":"Fix it","agent":"Claude","status":"needs_input",
                 "updated":1700000000,"added":3,"removed":1,"project":"app","env":"main","place":"/w/app","pr":null,
                 "asks":{"kind":"permission","text":"Run cargo test"},"new_field":[1,2]}],
                 "places":[{"path":"/w/app","project":"app","env":"main","branch":"main","agent":"claude"}]}
                """,
            ),
        )
        assertEquals("mac", inbox.host)
        assertEquals(SessionStatus.NEEDS_INPUT, inbox.sessions.first().status)
        assertEquals("Run cargo test", inbox.sessions.first().asks)
        assertEquals(1_700_000_000_000L, inbox.sessions.first().updated)
        assertNull(inbox.sessions.first().pr)
        assertEquals("main", inbox.places.first().branch)
    }

    @Test
    fun aNumberPastIntIsNullNotAWrap() {
        assertNull(json("1e20").int)
        assertNull(json("-1e20").int)
        assertEquals(42, json("42").int)
        assertEquals(2, json("2.9").int)
        assertEquals(-2, json("-2.9").int)
        assertNull(json("\"42\"").int)
    }

    @Test
    fun readsTheImagesAUserEntryNames() {
        val e = Entry.of(json("""{"type":"user","text":"look","at":0,"pastes":[],"images":[{"name":"shot.png","url":"/image?x"}]}"""))
        assertEquals(listOf("shot.png"), (e as Entry.User).images)
    }

    @Test
    fun aToolChangesWithItsExitCodeAndDiffs() {
        val a = Entry.of(json("""{"type":"tool","id":"t","title":"x","kind":"edit","status":"completed","diffs":[{"path":"a","added":1,"removed":0}]}"""))
        val b = Entry.of(json("""{"type":"tool","id":"t","title":"x","kind":"edit","status":"completed","diffs":[{"path":"a","added":2,"removed":0}]}"""))
        val c = Entry.of(json("""{"type":"tool","id":"t","title":"x","kind":"edit","status":"completed","exit_code":1,"diffs":[{"path":"a","added":1,"removed":0}]}"""))
        assertNotEquals(a, b)
        assertNotEquals(a, c)
        assertEquals(a, Entry.of(json("""{"type":"tool","id":"t","title":"x","kind":"edit","status":"completed","diffs":[{"path":"a","added":1,"removed":0}]}""")))
        assertEquals(Entry.Unknown, Entry.of(json("""{"type":"from_the_future"}""")))
    }

    @Test
    fun mergesTheTailAfterAReconnect() {
        var s = LiveSession("s1", "", "")
        s = s.applySession(json("""{"from":2,"count":4,"entries":[{"type":"agent","text":"c"},{"type":"agent","text":"d"}],"status":"working"}"""))
        assertEquals(2, s.from)
        assertEquals(2, s.entries.size)
        s = s.applyEntries(json("""{"from":0,"count":4,"older":true,"entries":[{"type":"agent","text":"a"},{"type":"agent","text":"b"}]}"""))
        assertEquals(0, s.from)
        assertEquals(listOf("a", "b", "c", "d").map(Entry::Agent), s.entries)
        s = s.applyEntries(json("""{"from":3,"count":5,"entries":[{"type":"agent","text":"D"},{"type":"agent","text":"e"}]}"""))
        assertEquals(listOf("a", "b", "c", "D", "e").map(Entry::Agent), s.entries)
        s = s.applySession(json("""{"from":3,"count":5,"entries":[{"type":"agent","text":"D"},{"type":"agent","text":"E"}],"status":"idle"}"""))
        assertEquals(0, s.from)
        assertEquals(5, s.entries.size)
        assertEquals(Entry.Agent("E"), s.entries.last())
        assertEquals(SessionStatus.IDLE, s.status)
    }

    @Test
    fun readsAPermissionAndAQuestion() {
        val s = LiveSession("s1", "", "").applyState(
            json(
                """
                {"status":"needs_input","permission":{"tool_call_id":"t","title":"Run ls","options":[{"id":"a","name":"Allow","kind":"allow_once"}]},
                 "question":{"tool_call_id":"q","questions":[{"index":1,"topic":"T","question":"Which?","options":["x","y"],"multi":false}]},
                 "usage":{"summary":"12k tokens"},"elapsed_ms":2000,"queue":["next"],"changes":{"files":1,"added":2,"removed":0}}
                """,
            ),
            now = 10_000,
        )
        assertEquals("allow_once", s.permission?.options?.first()?.kind)
        assertEquals(listOf("x", "y"), s.question?.questions?.first()?.options)
        assertEquals(listOf("next"), s.queue)
        assertEquals(2, s.changes.added)
        assertEquals(8_000L, s.turnStarted)
        assertNotNull(s.usage)
        assertNull(s.applyState(json("""{"status":"idle","permission":null}""")).permission)
    }
}
