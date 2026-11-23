package com.arakun.kit

import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The account host and GitHub played by a local server, as cloud/ and GitHub answer. */
class AccountTest {
    private val server = MockWebServer().apply { start() }
    private val base = "http://localhost:${server.port}"
    private val api = AccountClient(base, github = base)

    @AfterTest
    fun done() = server.close()

    private fun answer(status: Int, body: String) =
        server.enqueue(MockResponse.Builder().code(status).body(body).build())

    @Test
    fun readsMeWithTheSessionInTheHeader() {
        answer(
            200,
            """{"github_id": 42, "login": "rik", "flags": ["beta"], "plan": "free", "extra": 1,
               "notices": [{"id": "n1", "text": "Maintenance tonight", "url": "https://status.arakun.dev"},
                           {"id": 7, "text": "No link", "url": null}]}""",
        )
        val me = api.me("tok-secret")
        assertEquals(Me(42, "rik", listOf("beta"), "free", listOf(Notice("n1", "Maintenance tonight", "https://status.arakun.dev"), Notice("7", "No link", null))), me)
        val req = server.takeRequest()
        assertEquals("/me", req.target)
        assertEquals("Bearer tok-secret", req.headers["Authorization"])
        assertEquals("https://status.arakun.dev", me.notices[0].link)
        assertNull(Notice("x", "t", "javascript:alert(1)").link)
    }

    @Test
    fun readsMachines() {
        answer(
            200,
            """[{"label": "lmx5lt63vsxoyes4", "pub": "BPub", "name": "studio", "kind": "app", "online": true, "last_seen_at": 1700000000},
                {"label": "aaaaaaaaaaaaaaaa", "pub": "BOther", "name": "build box", "kind": "server", "online": false, "last_seen_at": 1690000000},
                {"name": "no label, no key"}]""",
        )
        assertEquals(
            listOf(
                Machine("lmx5lt63vsxoyes4", "BPub", "studio", "app", true, 1_700_000_000),
                Machine("aaaaaaaaaaaaaaaa", "BOther", "build box", "server", false, 1_690_000_000),
            ),
            api.machines("tok"),
        )
        assertEquals("/machines", server.takeRequest().target)
    }

    @Test
    fun readsSignedInDevicesAndSignsOneOut() {
        answer(200, """[{"id": "s1", "name": "Pixel 9", "kind": "android", "created_at": 1, "last_used_at": 2, "current": true}]""")
        assertEquals(listOf(SignedInDevice("s1", "Pixel 9", "android", 1, 2, true)), api.sessions("tok"))
        server.takeRequest()
        answer(204, "")
        api.signOutSession("tok", "s1")
        val req = server.takeRequest()
        assertEquals("DELETE", req.method)
        assertEquals("/sessions/s1", req.target)
        // An id that could reach another route never leaves the phone.
        assertFailsWith<AccountError.Refused> { api.signOutSession("tok", "../machines") }
        assertEquals(2, server.requestCount)
    }

    @Test
    fun mapsTheAnswersThatMeanSomething() {
        answer(401, """{"error": "signed out"}""")
        assertFailsWith<AccountError.SignedOut> { api.me("tok") }
        answer(403, """{"error": "blocked", "reason": "Too many machines."}""")
        assertEquals("Too many machines.", assertFailsWith<AccountError.Blocked> { api.machines("tok") }.reason)
        answer(500, """{"error": "boom"}""")
        assertEquals("The server answered: boom.", assertFailsWith<AccountError.Refused> { api.me("tok") }.message)
        answer(200, "not json")
        assertFailsWith<AccountError.Refused> { api.me("tok") }
    }

    @Test
    fun signsInWithTheDeviceFlow() {
        answer(200, """{"device_code": "dev-secret-77", "user_code": "ABCD-1234", "verification_uri": "https://github.com/login/device", "expires_in": 900, "interval": 5}""")
        val code = api.startDeviceFlow("Ov23x")
        assertEquals("ABCD-1234", code.userCode)
        assertEquals(5_000, code.intervalMs)
        assertEquals(900_000, code.expiresInMs)
        val asked = server.takeRequest()
        assertEquals("/login/device/code", asked.target)
        assertEquals("client_id=Ov23x&scope=user%3Aemail", asked.body?.utf8())

        answer(200, """{"error": "authorization_pending"}""")
        assertEquals(Poll.Wait(5_000), api.pollOnce("Ov23x", "dev-secret-77", 5_000))
        answer(200, """{"error": "slow_down", "interval": 10}""")
        assertEquals(Poll.Wait(10_000), api.pollOnce("Ov23x", "dev-secret-77", 5_000))
        answer(502, "bad gateway")
        assertEquals(Poll.Wait(10_000), api.pollOnce("Ov23x", "dev-secret-77", 10_000))
        answer(200, """{"access_token": "gho_x"}""")
        assertEquals("gho_x", assertIs<Poll.Token>(api.pollOnce("Ov23x", "dev-secret-77", 10_000)).token)
        repeat(4) { server.takeRequest() }

        answer(200, """{"session": "s3cret", "session_id": "s1", "expires_at": 9, "github_id": 42, "login": "rik", "flags": [], "plan": "free"}""")
        val signed = api.signIn("gho_x", "Pixel 9")
        assertEquals("s3cret", signed.session)
        assertEquals("rik", signed.login)
        val body = parseJson(server.takeRequest().body!!.utf8())!!
        assertEquals("gho_x", body["access_token"].str)
        assertEquals("Pixel 9", body["name"].str)
        assertEquals("android", body["kind"].str)
        // Nothing that prints a sign-in shows its secrets.
        assertFalse("s3cret" in signed.toString())
        assertFalse("dev-secret-77" in code.toString())
    }

    @Test
    fun slowDownWithoutAnIntervalAddsFiveSeconds() {
        assertEquals(Poll.Wait(10_000), Poll.next(parseJson("""{"error": "slow_down"}""")!!, 5_000))
        assertEquals(Poll.Expired, Poll.next(parseJson("""{"error": "expired_token"}""")!!, 5_000))
        assertEquals(Poll.Denied, Poll.next(parseJson("""{"error": "access_denied"}""")!!, 5_000))
    }

    @Test
    fun theKnownHostNeedsNoConfigAndAnotherIsAsked() {
        assertEquals(Account.CLIENT_ID, AccountClient().clientId())
        answer(200, """{"github_client_id": "Ov23other"}""")
        assertEquals("Ov23other", api.clientId())
        assertEquals("/auth/config", server.takeRequest().target)
    }

    @Test
    fun aSessionNeverGoesOverPlainHttpToAnotherMachine() {
        assertTrue(Account.safeBase("https://app.arakun.dev"))
        assertTrue(Account.safeBase("http://localhost:8787"))
        assertTrue(Account.safeBase("http://[::1]:8787"))
        assertFalse(Account.safeBase("http://192.168.1.2:8787"))
        assertFalse(Account.safeBase("http://localhost@evil.example"))
        assertFailsWith<AccountError.Refused> { AccountClient("http://example.com").me("tok") }
    }

    @Test
    fun aMachineIsTrustedOnlyWhenItsKeyNamesItsLabel() {
        val key = P256.x963(P256.generate().public as java.security.interfaces.ECPublicKey)
        val label = E2E.label(key)
        assertTrue(Account.keyMatches(Machine(label, Base64Url.encode(key), "studio", "app", true, 0)))
        assertFalse(Account.keyMatches(Machine("aaaaaaaaaaaaaaaa", Base64Url.encode(key), "studio", "app", true, 0)))
        assertFalse(Account.keyMatches(Machine(label, "not a key", "studio", "app", true, 0)))
    }
}

/** The sealed store is the only place the session goes, and nothing prints it. */
class AccountKeeperTest {
    private class Recorder : SealedStore {
        val values = mutableMapOf<String, String>()
        val writes = mutableListOf<String>()
        override fun read(key: String) = values[key]
        override fun has(key: String) = key in values
        override fun write(key: String, value: String?): Boolean {
            writes += key
            if (value == null) values.remove(key) else values[key] = value
            return true
        }
    }

    @Test
    fun theSessionIsKeptInTheSealedStoreOnly() {
        val store = Recorder()
        val keeper = AccountKeeper(store)
        val s = AccountSession(Account.HOST, "s3cret", "s1", "rik")
        assertTrue(keeper.save(s))
        keeper.machine = "lmx5lt63vsxoyes4"
        assertEquals(s, AccountKeeper(store).load())
        assertEquals("lmx5lt63vsxoyes4", AccountKeeper(store).machine)
        assertEquals(listOf(AccountKeeper.SESSION, AccountKeeper.MACHINE), store.writes)
        assertTrue("s3cret" in store.values.getValue(AccountKeeper.SESSION))
        assertFalse("s3cret" in s.toString())

        // Signing out drops both.
        keeper.save(null)
        assertTrue(store.values.isEmpty())
        assertNull(keeper.load())
    }

    @Test
    fun theOldRelayPairingIsForgotten() {
        val store = Recorder()
        store.values[AccountKeeper.OLD_CREDENTIALS] = """{"label":"x","ticket":"t","token":"k"}"""
        store.values[AccountKeeper.SESSION] = "kept"
        AccountKeeper(store).forgetOldPairing()
        assertEquals(setOf(AccountKeeper.SESSION), store.values.keys)
        // Once gone, a launch writes nothing.
        store.writes.clear()
        AccountKeeper(store).forgetOldPairing()
        assertTrue(store.writes.isEmpty())
    }
}
