package com.arakun.android

import android.content.Intent
import com.arakun.kit.Machine
import com.arakun.kit.Notice
import com.arakun.kit.SessionStatus
import com.arakun.kit.SignedInDevice

/**
 * A computer played here, for screenshots and design work without one. Debug
 * builds only, launched with extras:
 *
 *     adb shell am start -n com.arakun.app/com.arakun.android.MainActivity --ez demo true --es demoOpen s1
 *
 * It is signed in to a sample account with two computers and opens the
 * first. `demoOpen <id>` opens a session, `demoNew true` the New session
 * sheet, `demoMachines true` the list of computers, `demoSignIn true` the
 * sign-in screen and `demoMoods true` every mood of the raccoon side by side.
 */
data class Demo(val signIn: Boolean, val machines: Boolean, val open: String?, val new: Boolean, val moods: Boolean) {
    companion object {
        fun from(intent: Intent?): Demo? {
            if (!BuildConfig.DEBUG || intent?.getBooleanExtra("demo", false) != true) return null
            return Demo(
                signIn = intent.getBooleanExtra("demoSignIn", false),
                machines = intent.getBooleanExtra("demoMachines", false),
                open = intent.getStringExtra("demoOpen"),
                new = intent.getBooleanExtra("demoNew", false),
                moods = intent.getBooleanExtra("demoMoods", false),
            )
        }

        val notice = Notice("n1", "The relay restarts tonight at 22:00 UTC. It is back within a few minutes.", null)

        fun machines(now: Long) = listOf(
            Machine("lmx5lt63vsxoyes4", "", "studio", "app", true, now),
            Machine("q3ndb7xwz5k2mfaa", "", "build-box", "server", false, now - 5400),
        )

        fun devices(now: Long) = listOf(
            SignedInDevice("d1", "Pixel 9", "android", now - 86400 * 3, now, true),
            SignedInDevice("d2", "studio", "app", now - 86400 * 40, now - 60, false),
            SignedInDevice("d3", "iPhone", "phone", now - 86400 * 12, now - 7200, false),
        )

        private fun wire(status: SessionStatus) = when (status) {
            SessionStatus.IDLE -> "idle"
            SessionStatus.WORKING -> "working"
            SessionStatus.NEEDS_INPUT -> "needs_input"
            SessionStatus.ERROR -> "error"
        }

        fun inbox(now: Long) = """
        {"host":"studio","sessions":[
         {"id":"s1","title":"Fix the relay pairing limit","agent":"Opus 5.5","status":"needs_input","updated":${now - 40},"added":42,"removed":7,"project":"arakun","env":"env/brave-otter","place":"/w/a","pr":"#631","asks":{"kind":"permission","text":"cargo test -p arakun relay"}},
         {"id":"s2","title":"Port the phone page to Compose","agent":"Opus 5.5","status":"working","updated":${now - 120},"added":1204,"removed":0,"project":"arakun","env":"env/quick-dune","place":"/w/b"},
         {"id":"s3","title":"Speed up the inbox query","agent":"Droid","status":"idle","updated":${now - 3600},"added":18,"removed":31,"project":"arakun","env":"main","place":"/w/c","pr":"#627"},
         {"id":"s4","title":"Landing page hero copy","agent":"Sonnet 5.5","status":"error","updated":${now - 7200},"added":0,"removed":0,"project":"site","env":"env/calm-heron","place":"/w/d"},
         {"id":"s5","title":"Release notes for 0.13","agent":"Haiku 5.5","status":"idle","updated":${now - 86400},"added":64,"removed":2,"project":"site","env":"main","place":"/w/e"}],
         "places":[{"path":"/w/a","project":"arakun","env":"env/brave-otter","branch":"env/brave-otter","agent":"claude"},{"path":"/w/e","project":"site","env":"main","branch":"main","agent":"claude"}]}
        """

        /** A session in `status`, as the computer would send it. */
        fun session(at: Long, status: SessionStatus) = """
        {"from":0,"count":7,"status":"${wire(status)}","usage":{"summary":"48k tokens · ${'$'}0.41"},
         "changes":{"files":3,"added":42,"removed":7},"elapsed_ms":95000,
         "entries":[
          {"type":"user","text":"Pairing links fail with limit:phones. Find out why and fix it.","at":$at,"elapsed_ms":null,"pastes":[]},
          {"type":"thought","text":"The relay spends the code before it checks the phone limit, so a refused phone can never retry."},
          {"type":"tool","id":"t1","title":"Read cloud/src/relay/relay.ts","kind":"read","status":"completed","diffs":[],"added":0,"removed":0},
          {"type":"tool","id":"t2","title":"rg -n admit cloud/src/relay","kind":"search","status":"completed","command":"rg -n admit cloud/src/relay","output":"cloud/src/relay/relay.ts:412:  admit(phone) {\ncloud/src/relay/relay.ts:498:    this.admit(p)","diffs":[],"added":0,"removed":0},
          {"type":"agent","text":"Found it. `acceptPhone` deletes the pairing code in `admit()` **before** the limit check. When the check refuses the phone, the code is already gone, so every retry gets `no_pairing`."},
          {"type":"plan","entries":[{"content":"Check the limit before spending the code","status":"completed"},{"content":"Add a test for a refused phone that retries","status":"in_progress"},{"content":"Run the relay tests","status":"pending"}]},
          {"type":"tool","id":"t3","title":"Edit cloud/src/relay/relay.ts","kind":"edit","status":"completed","diffs":[{"path":"cloud/src/relay/relay.ts","added":12,"removed":5}],"added":12,"removed":5}],
         "permission":{"tool_call_id":"p1","title":"cargo test -p arakun relay","options":[{"id":"a","name":"Allow","kind":"allow_once"},{"id":"b","name":"Always allow","kind":"allow_always"},{"id":"c","name":"Deny","kind":"reject_once"}]}}
        """
    }
}
