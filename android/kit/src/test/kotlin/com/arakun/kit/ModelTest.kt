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

    @Test
    fun readsTheSecretCard() {
        val s = LiveSession("s1", "", "").applyState(
            json(
                """
                {"status":"waiting","secret":{"id":7,"label":"OpenAI key","reason":"To call the API","deliver":"handle",
                 "url":"https://platform.openai.com/api-keys","fields":[{"name":"key","label":"API key"}],
                 "timeout_secs":600,"private":true}}
                """,
            ),
        )
        val r = s.secret
        assertNotNull(r)
        assertEquals(7L, r!!.id)
        assertEquals("OpenAI key", r.label)
        assertEquals(listOf(SecretAsk.Field("key", "API key")), r.fields)
        assertEquals("https://platform.openai.com/api-keys", r.url)
        assertEquals(true, r.private)
        // Plain http: the card is there, but not private.
        val clear = SecretAsk.of(json("""{"id":1,"label":"x","fields":[],"url":"javascript:alert(1)"}"""))
        assertEquals(false, clear?.private)
        assertNull(clear?.url)
        // No id, nothing to answer.
        assertNull(SecretAsk.of(json("""{"label":"x"}""")))
        assertNull(s.applyState(json("""{"status":"idle","secret":null}""")).secret)
    }

    @Test
    fun readsTheChangesTurnByTurn() {
        val turns = ChangedTurn.list(
            json(
                """
                {"type":"changes","id":"s1","turns":[
                 {"entry":4,"text":"Fix the limit","at":1700000000,"files":[
                  {"path":"cloud/src/relay.ts","name":"relay.ts","added":12,"removed":5,"diffs":[[6,0],[8,1]]},
                  {"path":"README.md","name":"README.md","added":1,"removed":0,"diffs":[[9,0],["x",1],[3],[1e20,0]]}]},
                 {"entry":0,"text":"","at":0,"files":[]}]}
                """,
            ),
        )
        assertEquals(2, turns.size)
        assertEquals("Fix the limit", turns[0].text)
        assertEquals(1_700_000_000_000L, turns[0].at)
        val relay = turns[0].files[0]
        assertEquals(listOf(DiffRef(6, 0), DiffRef(8, 1)), relay.diffs)
        assertEquals("cloud/src", relay.dir)
        assertEquals(12, relay.added)
        // A pair that names no diff is left out, the file stays.
        assertEquals(listOf(DiffRef(9, 0)), turns[0].files[1].diffs)
        assertEquals("", turns[0].files[1].dir)
        assertEquals(emptyList(), ChangedTurn.list(json("""{"type":"changes"}""")))
    }

    @Test
    fun readsADiffWithItsHunksAndLines() {
        val d = FileDiff.of(
            json(
                """
                {"type":"diff","id":"s1","entry":6,"index":0,"path":"src/a.rs","name":"a.rs","added":1,"removed":1,
                 "new_file":false,"deleted":false,"cut":40,
                 "lines":["@@ -1,3 +1,3 @@ fn main",[" ",1,"fn main() {"],["-",2,"    old();"],["+",2,"    new();"],["+",null,""],[1,2],{"x":1}]}
                """,
            ),
        )!!
        assertEquals(DiffRef(6, 0), d.ref)
        assertEquals("src/a.rs", d.path)
        assertEquals(40, d.cut)
        assertEquals(
            listOf(
                DiffLine.Hunk("@@ -1,3 +1,3 @@ fn main"),
                DiffLine.Text(' ', 1, "fn main() {"),
                DiffLine.Text('-', 2, "    old();"),
                DiffLine.Text('+', 2, "    new();"),
                DiffLine.Text('+', null, ""),
            ),
            d.lines,
        )
        val new = FileDiff.of(json("""{"entry":1,"index":2,"new_file":true,"lines":[]}"""))!!
        assertEquals(true, new.newFile)
        assertEquals(0, new.cut)
        // Without its place there is nowhere to show it.
        assertNull(FileDiff.of(json("""{"type":"diff","path":"a"}""")))
    }
}
