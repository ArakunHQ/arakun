package com.arakun.android

import android.content.Intent

/**
 * A computer played here, for screenshots and design work without one. Debug
 * builds only, launched with extras:
 *
 *     adb shell am start -n com.arakun.android/.MainActivity --ez demo true --es demoOpen s1
 *
 * `demoOpen <id>` opens a session, `demoNew true` the New session sheet,
 * `demoUnpaired true` the pairing screen and `demoMoods true` every mood of
 * the raccoon side by side.
 */
data class Demo(val unpaired: Boolean, val open: String?, val new: Boolean, val moods: Boolean) {
    companion object {
        fun from(intent: Intent?): Demo? {
            if (!BuildConfig.DEBUG || intent?.getBooleanExtra("demo", false) != true) return null
            return Demo(
                unpaired = intent.getBooleanExtra("demoUnpaired", false),
                open = intent.getStringExtra("demoOpen"),
                new = intent.getBooleanExtra("demoNew", false),
                moods = intent.getBooleanExtra("demoMoods", false),
            )
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

        fun session(at: Long, working: Boolean) = """
        {"from":0,"count":7,"status":"${if (working) "working" else "needs_input"}","usage":{"summary":"48k tokens · ${'$'}0.41"},
         "changes":{"files":3,"added":42,"removed":7},"elapsed_ms":95000,
         "entries":[
          {"type":"user","text":"Pairing links fail with limit:phones. Find out why and fix it.","at":$at,"elapsed_ms":null,"pastes":[]},
          {"type":"thought","text":"The relay spends the code before it checks the phone limit, so a refused phone can never retry."},
          {"type":"tool","id":"t1","title":"Read relay/src/relay.ts","kind":"read","status":"completed","diffs":[],"added":0,"removed":0},
          {"type":"tool","id":"t2","title":"rg -n admit relay/src","kind":"search","status":"completed","command":"rg -n admit relay/src","output":"relay/src/relay.ts:412:  admit(phone) {\nrelay/src/relay.ts:498:    this.admit(p)","diffs":[],"added":0,"removed":0},
          {"type":"agent","text":"Found it. `acceptPhone` deletes the pairing code in `admit()` **before** the limit check. When the check refuses the phone, the code is already gone, so every retry gets `no_pairing`."},
          {"type":"plan","entries":[{"content":"Check the limit before spending the code","status":"completed"},{"content":"Add a test for a refused phone that retries","status":"in_progress"},{"content":"Run the relay tests","status":"pending"}]},
          {"type":"tool","id":"t3","title":"Edit relay/src/relay.ts","kind":"edit","status":"completed","diffs":[{"path":"relay/src/relay.ts","added":12,"removed":5}],"added":12,"removed":5}],
         "permission":{"tool_call_id":"p1","title":"cargo test -p arakun relay","options":[{"id":"a","name":"Allow","kind":"allow_once"},{"id":"b","name":"Always allow","kind":"allow_always"},{"id":"c","name":"Deny","kind":"reject_once"}]}}
        """
    }
}
