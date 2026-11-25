package dev.lapiszero.app.ui

import android.content.ClipboardManager
import android.content.ComponentName
import android.content.Context
import android.os.Bundle
import android.os.Parcel
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.ComposeTimeoutException
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsOff
import androidx.compose.ui.test.assertIsOn
import androidx.compose.ui.test.isToggleable
import androidx.compose.ui.test.isRoot
import androidx.compose.ui.test.printToString
import androidx.compose.ui.test.hasScrollToNodeAction
import androidx.compose.ui.test.hasSetTextAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createEmptyComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onFirst
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performScrollToNode
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performTextReplacement
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.platform.app.InstrumentationRegistry
import dev.lapiszero.app.AppContainer
import dev.lapiszero.app.Fixtures
import dev.lapiszero.app.MemorySecureStore
import dev.lapiszero.app.data.remote.Endpoint
import dev.lapiszero.app.data.remote.Host
import dev.lapiszero.app.data.remote.LapisJson
import dev.lapiszero.app.domain.ChatRepository
import dev.lapiszero.app.errorResponse
import dev.lapiszero.app.jsonResponse
import dev.lapiszero.app.theme.LapisTheme
import dev.lapiszero.app.theme.ThemeMode
import dev.lapiszero.app.ui.navigation.LapisRoot
import java.awt.image.BufferedImage
import java.io.ByteArrayOutputStream
import java.util.concurrent.CopyOnWriteArrayList
import javax.imageio.ImageIO
import kotlinx.coroutines.cancel
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf

/**
 * Drives the real Compose UI (every tab, the approval sheet, the pairing paths) on the JVM
 * against a scripted daemon, so a screen that crashes or cannot be reached fails the build
 * before the app ever gets near a phone.
 */
@RunWith(RobolectricTestRunner::class)
class AppSmokeTest {
    @get:Rule
    val compose = createEmptyComposeRule()

    /** A bare host activity, registered for the test only (nothing extra ships in the APK). */
    private lateinit var scenario: ActivityScenario<ComponentActivity>

    private val server = MockWebServer()
    private val sockets = CopyOnWriteArrayList<WebSocket>()
    private val requests = CopyOnWriteArrayList<String>()
    private val bodies = java.util.concurrent.ConcurrentHashMap<String, String>()

    /** Every body in order, for "a password is in the vault write and in no other request". */
    private val sent = CopyOnWriteArrayList<Pair<String, String>>()

    /** The secrets vault answers with an error while this is set. */
    @Volatile
    private var vaultDown = false

    /** False: device software from before the SSH features, like a device not reflashed yet. */
    @Volatile
    private var sshFeatures = true

    /** The scripted daemon keeps what it is sent, as the real one does: hosts, secret names, chat fields. */
    @Volatile
    private var hosts = """{"macbook-pro": {"hostname": "macbook-pro", "tailscale_ip": "100.64.10.21", "os": "macos", "policy": "balanced",
        "vnc": {"port": 5900, "username": "alice", "password_env": "LAPIS_VNC_PASSWORD_macbook_pro"},
        "ssh": {"port": 22, "user": "alice", "identity_file": null}}}"""
    private val vault = CopyOnWriteArrayList(listOf("ANTHROPIC_API_KEY"))
    private val chatPatch = java.util.concurrent.ConcurrentHashMap<String, JsonElement>()

    @Volatile
    private var probe = """{"host_id": "macbook-pro", "tailscale": {"ok": true, "latency_ms": 12, "detail": "mock"},
        "vnc": {"ok": true, "latency_ms": 85, "detail": "mock"}, "ssh": {"ok": false, "latency_ms": null, "detail": "TimeoutError"}}"""

    @Volatile
    private var authorize = jsonResponse("""{"ok": true, "already": false, "detail": "key added for alice"}""")

    @Volatile
    private var agents = jsonResponse(Fixtures.AGENTS)
    private lateinit var container: AppContainer
    private val chatId = "c_9e838c02"
    private val nonce = "GneYMjL9l4v_GZvPP4A8TukmQs978gPNuFg-W3Q3ZGc"

    @Before
    fun setUp() {
        server.dispatcher = object : Dispatcher() {
            override fun dispatch(request: RecordedRequest): MockResponse {
                val key = "${request.method} ${request.url.encodedPath}"
                requests += key
                request.body?.let {
                    bodies[key] = it.utf8()
                    sent += key to it.utf8()
                }
                return route(key)
            }
        }
        server.start()
        val context = ApplicationProvider.getApplicationContext<Context>()
        shadowOf(context.packageManager)
            .addActivityIfNotPresent(ComponentName(context, ComponentActivity::class.java))
        scenario = ActivityScenario.launch(ComponentActivity::class.java)
        container = AppContainer(context, MemorySecureStore())
    }

    @After
    fun tearDown() {
        scenario.close()
        container.sessions.current.value?.close()
        container.appScope.cancel()
        server.close()
    }

    private val host: String get() = "127.0.0.1:${server.port}"

    private fun jpeg(): ByteArray = ByteArrayOutputStream().also {
        ImageIO.write(BufferedImage(64, 36, BufferedImage.TYPE_INT_RGB), "jpg", it)
    }.toByteArray()

    private fun parse(text: String): JsonObject = LapisJson.parseToJsonElement(text).jsonObject

    /** The chat as the daemon has it now; older software knows neither `agent` nor `workdir`. */
    private fun chat(lastMessage: String = "null"): String =
        JsonObject(parse(Fixtures.chat(lastMessage = lastMessage, agent = "lapis".takeIf { sshFeatures })) + chatPatch).toString()

    private fun secrets(): String = (
        vault.map { """{"name": "$it", "is_set": true, "masked": "${if (it == "ANTHROPIC_API_KEY") "...a9f2" else "...c0de"}"}""" } +
            """{"name": "OPENAI_API_KEY", "is_set": false, "masked": null}"""
        ).joinToString(prefix = """{"secrets": [""", postfix = "]}")

    private fun route(key: String): MockResponse = when (key) {
        "GET /v1/ws" -> MockResponse.Builder().webSocketUpgrade(
            object : WebSocketListener() {
                override fun onOpen(webSocket: WebSocket, response: Response) {
                    sockets += webSocket
                    webSocket.send(Fixtures.stateSync())
                }

                override fun onMessage(webSocket: WebSocket, text: String) {
                    if (text.contains("\"ping\"")) webSocket.send("""{"type": "pong", "ts": 1}""")
                }
            },
        ).build()

        "GET /v1/health" -> jsonResponse("""{"ok": true, "version": "0.2.0", "hostname": "devbox", "uptime_sec": 5, "mock": true, "device_id": "dev_local"}""")
        "POST /v1/pair/exchange" -> jsonResponse("""{"device_id": "dev_qr", "api_token": "qr-issued-token-0123456789abcdef", "hostname": "lapis-zero"}""")
        "GET /v1/state" -> jsonResponse(Fixtures.stateSync())
        "GET /v1/chats" -> jsonResponse("""{"chats": [${chat(lastMessage = Fixtures.assistantMessage())}]}""")
        "GET /v1/chats/$chatId" -> jsonResponse(
            """{"chat": ${chat()}, "messages": [${Fixtures.userMessage()}, ${Fixtures.toolMessage()}, ${Fixtures.assistantMessage()}], "has_more": false}""",
        )
        "PATCH /v1/chats/$chatId" -> {
            chatPatch.putAll(parse(bodies.getValue(key)))
            jsonResponse(chat())
        }
        // A chat started from the phone: nothing in it yet.
        "POST /v1/chats" -> jsonResponse(Fixtures.chat(id = "c_new", title = "", updatedAt = 1795541300))
        "GET /v1/chats/c_new" -> jsonResponse(
            """{"chat": ${Fixtures.chat(id = "c_new", title = "", updatedAt = 1795541300)}, "messages": [], "has_more": false}""",
        )
        "GET /v1/screenshots/1.jpg", "GET /v1/screenshots/2.jpg" -> MockResponse.Builder().code(200)
            .setHeader("Content-Type", "image/jpeg").setHeader("X-Lapis-Width", "64").setHeader("X-Lapis-Height", "36")
            .body(Buffer().write(jpeg())).build()
        "GET /v1/device/telemetry" -> jsonResponse(Fixtures.TELEMETRY)
        "GET /v1/device/screen.png" -> errorResponse(504, "ui_unavailable", "lapis-ui did not answer the screen request")
        "PATCH /v1/device/ui" -> jsonResponse("""{"theme": "paper", "brightness": 100, "mute_audio": false, "timezone": "UTC"}""")
        "GET /v1/hosts" -> jsonResponse("""{"hosts": $hosts, "active_host": "macbook-pro"}""")
        "PUT /v1/hosts" -> {
            hosts = parse(bodies.getValue(key)).getValue("hosts").toString()
            jsonResponse("""{"hosts": $hosts, "active_host": "macbook-pro"}""")
        }
        "POST /v1/hosts/macbook-pro/probe" -> jsonResponse(probe)
        "GET /v1/device/ssh-key" -> if (sshFeatures) jsonResponse(Fixtures.SSH_KEY) else errorResponse(404, "not_found", "404: Not Found")
        "POST /v1/hosts/macbook-pro/ssh/authorize" -> if (sshFeatures) authorize else errorResponse(404, "not_found", "404: Not Found")
        "GET /v1/hosts/macbook-pro/agents" -> if (sshFeatures) agents else errorResponse(404, "not_found", "404: Not Found")
        "GET /v1/config" -> jsonResponse(
            """{"config": {"llm": {"provider": "anthropic", "model": "claude-sonnet-5-5", "api_key_env": "ANTHROPIC_API_KEY", "effort": "medium",
                "base_url": "", "max_steps": 25, "request_timeout_sec": 120}, "kvm": {"stuck_threshold": 3, "vnc_idle_disconnect_sec": 60},
                "security": {"approval_default_tier": "balanced"}}}""",
        )
        "GET /v1/secrets/status" -> if (vaultDown) errorResponse(500, "internal", "vault unavailable") else jsonResponse(secrets())
        "PATCH /v1/secrets" -> {
            vault.addAllAbsent(parse(bodies.getValue(key)).getValue("set").jsonObject.keys)
            jsonResponse(secrets())
        }
        "PUT /v1/config" -> jsonResponse(bodies.getValue(key)) // the daemon answers with what it stored
        "GET /v1/schedules" -> jsonResponse(
            """{"schedules": [{"name": "Daily Digest", "schedule": "0 8 * * *", "host": "macbook-pro",
                "prompt": "Check overnight CI runs and summarize any failures.", "enabled": false, "next_run": null, "last_run_at": null, "last_chat_id": null}]}""",
        )
        "GET /v1/devices" -> jsonResponse("""{"devices": [{"id": "dev_local", "name": "Dev token", "model": "dev", "created_at": 1795541234, "last_seen_at": 1795541240, "revoked": false, "current": true}]}""")
        "POST /v1/approvals/req_1" -> jsonResponse("""{"ok": true}""")
        "POST /v1/agent/kill-all" -> jsonResponse("""{"ok": true}""")
        else -> errorResponse(404, "not_found", "no route for $key")
    }

    private fun pairWithMock() {
        container.devices.add("devbox", Endpoint(host, tls = false), "dev-local-token", "dev_local")
        container.sessions.start()
    }

    private fun show(mode: ThemeMode = ThemeMode.PAPER) {
        scenario.onActivity { activity ->
            activity.setContent { LapisTheme(mode) { LapisRoot(container) } }
        }
    }

    /** A real 120x68 1-bit preview: a bright field with a dark box, like the daemon sends. */
    private fun previewB64(): String {
        val packed = ByteArray(1020) { 0xFF.toByte() }
        for (y in 24 until 44) for (x in 40 until 80) {
            packed[y * 15 + x / 8] = (packed[y * 15 + x / 8].toInt() and (0x80 ushr (x % 8)).inv()).toByte()
        }
        return java.util.Base64.getEncoder().encodeToString(packed)
    }

    private fun dump(): String = compose.onAllNodes(isRoot(), useUnmergedTree = true).printToString(maxDepth = 40)

    private fun waitUntilOrDump(what: String, timeoutMs: Long, condition: () -> Boolean) {
        try {
            compose.waitUntil(timeoutMs, condition)
        } catch (e: ComposeTimeoutException) {
            throw AssertionError("timed out waiting for $what; requests=$requests\n${dump()}", e)
        }
    }

    private fun waitFor(text: String, timeoutMs: Long = 15_000) = waitUntilOrDump("'$text'", timeoutMs) {
        compose.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isNotEmpty()
    }

    /**
     * With the head's contact sheets asked for (`-Plapis.head.sheets=<directory>`), also saves
     * the window as it is now, to see the head where the app puts it. Nothing otherwise.
     */
    private fun snap(name: String) {
        val sheets = System.getProperty("lapis.head.sheets")?.takeIf { it.isNotBlank() } ?: return
        compose.waitForIdle()
        scenario.onActivity { activity ->
            val view = activity.window.decorView
            val bitmap = android.graphics.Bitmap.createBitmap(view.width, view.height, android.graphics.Bitmap.Config.ARGB_8888)
            view.draw(android.graphics.Canvas(bitmap))
            java.io.File(sheets, "app-screen-$name.png").outputStream().use {
                bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it)
            }
        }
    }

    /** How many heads on screen act out `state` ("idle", "fetching", ...; see `headDescription`). */
    private fun heads(state: String): Int =
        compose.onAllNodesWithContentDescription("Lapis agent: $state").fetchSemanticsNodes().size

    private fun waitForHead(state: String) = waitUntilOrDump("the agent's head to be $state", 15_000) { heads(state) > 0 }

    /** The chat list is up: its "New chat" button exists (an extended FAB is named by its icon). */
    private fun waitForWorkspace() = waitUntilOrDump("the workspace", 15_000) {
        compose.onAllNodesWithContentDescription("New chat").fetchSemanticsNodes().isNotEmpty()
    }

    private fun waitGone(text: String, timeoutMs: Long = 15_000) = waitUntilOrDump("'$text' to go away", timeoutMs) {
        compose.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().isEmpty()
    }

    /** Scrolls the element into view when it lives in a scrolling container, then clicks it. */
    private fun tap(text: String) {
        val node = compose.onAllNodesWithText(text).onFirst()
        try {
            node.performScrollTo()
        } catch (e: AssertionError) {
            // Not inside a scrollable: it is on screen already.
        }
        node.performClick()
    }

    private fun push(frame: String) {
        compose.waitUntil(10_000) { sockets.isNotEmpty() }
        sockets.last().send(frame)
    }

    /** Clicks through the semantics action: a snackbar lying over the element does not take the tap. */
    private fun press(text: String, index: Int = 0) {
        compose.onAllNodesWithText(text)[index].performSemanticsAction(SemanticsActions.OnClick)
    }

    private fun field(label: String, exact: Boolean = false) =
        compose.onNode(hasSetTextAction() and hasText(label, substring = !exact))

    private fun shown(text: String): Int = compose.onAllNodesWithText(text, substring = true).fetchSemanticsNodes().size

    /** What was typed is held by exactly one node: a field that is marked, and so drawn, as a password. */
    private fun assertMasked(text: String) {
        compose.onNode(hasText(text, substring = true))
            .assert(hasSetTextAction() and SemanticsMatcher.keyIsDefined(SemanticsProperties.Password))
    }

    private fun bodiesOf(key: String): List<String> = sent.filter { it.first == key }.map { it.second }

    private fun lastHost(id: String): JsonObject = parse(bodiesOf("PUT /v1/hosts").last()).getValue("hosts").jsonObject.getValue(id).jsonObject

    /**
     * What the activity would write into its saved instance state right now, as text. Saving
     * takes the activity's lifecycle out of RESUMED, so this is the last thing a test does.
     */
    private fun savedState(): String {
        val bundle = Bundle()
        scenario.onActivity { InstrumentationRegistry.getInstrumentation().callActivityOnSaveInstanceState(it, bundle) }
        val parcel = Parcel.obtain()
        return try {
            parcel.writeBundle(bundle)
            val bytes = parcel.marshall()
            // Parcels hold strings as UTF-16; the 8-bit reading is there in case that ever changes.
            String(bytes, Charsets.UTF_16LE) + String(bytes, Charsets.ISO_8859_1)
        } finally {
            parcel.recycle()
        }
    }

    private fun openHosts() {
        waitFor("open settings")
        tap("Config")
        waitFor("Secrets vault")
        tap("KVM hosts")
        waitFor("Test connection")
    }

    @Test
    fun `every workspace tab and config screen renders against a daemon`() {
        pairWithMock()
        show()

        // Chats: list, host filter, active host, running indicator.
        waitFor("open settings")
        compose.onNodeWithContentDescription("New chat").assertIsDisplayed()
        compose.onNodeWithText("All").assertIsDisplayed()
        compose.onNodeWithText("ACTIVE HOST").assertIsDisplayed()
        compose.onNodeWithText("devbox").assertIsDisplayed() // device switcher pill
        compose.onNodeWithContentDescription("Lapis agent: idle").assertIsDisplayed() // the agent's head, at rest
        snap("chats")

        // A chat thread: bubbles, tool card, composer with host and policy badges.
        tap("open settings")
        waitFor("Mock run complete: 6 scripted steps.")
        compose.onNodeWithText("click(160, 24) left").assertIsDisplayed()
        compose.onNodeWithText("VNC").assertIsDisplayed()
        compose.onNodeWithText("Ask the agent…").assertIsDisplayed()
        compose.onNodeWithText("HOST DEFAULT ▾").assertIsDisplayed()
        compose.onNodeWithContentDescription("Send").assertExists()

        // The agent starts working here: STOP / KILL replace the composer, KILL is in the top bar.
        waitUntilOrDump("no head in a thread with nothing running", 15_000) { heads("idle") == 0 } // the list's own has left
        push(Fixtures.stateSync("ACTING", activeChatId = chatId, step = 2, action = "screenshot"))
        waitFor("STOP TASK")
        // Its head acts out the step the device names, and the line beside it says the same.
        waitForHead("fetching")
        waitFor("FETCHING · step 2/25")
        snap("thread-running")
        compose.onNodeWithText("KILL ALL").assertIsDisplayed()
        compose.onNodeWithText("KILL").assertIsDisplayed()
        tap("KILL")
        compose.waitUntil(10_000) { "POST /v1/agent/kill-all" in requests }
        push(Fixtures.stateSync("IDLE"))
        waitGone("STOP TASK")
        compose.onNodeWithContentDescription("Back").performClick()
        waitForWorkspace()

        // A new chat opens on the agent at rest, larger, over the line that says what to do.
        compose.onNodeWithContentDescription("New chat").performClick()
        waitFor("Run on which host?")
        tap("macbook-pro  (active)")
        waitFor("Describe what the agent should do on macbook-pro.")
        waitUntilOrDump("the new chat's head, alone", 15_000) { heads("idle") == 1 } // the list's own left with the list
        compose.onNodeWithContentDescription("Lapis agent: idle").assertIsDisplayed()
        snap("thread-new")
        compose.onNodeWithContentDescription("Back").performClick()
        waitForWorkspace()

        // Live KVM.
        tap("Live KVM")
        waitFor("Manual assist")
        compose.onNodeWithText("Click here").assertExists()
        compose.onNodeWithText("Take control").assertDoesNotExist() // nothing is running
        // The ticker is further down the (lazy) page; it was seeded from the latest chat.
        compose.onNode(hasScrollToNodeAction()).performScrollToNode(hasText("STEP TICKER"))
        compose.onNodeWithText("STEP TICKER").assertExists()
        compose.onNode(hasScrollToNodeAction()).performScrollToNode(hasText("click(160, 24) left"))

        // Device cockpit: telemetry, mirror 504, controls.
        tap("Device")
        waitFor("Battery (BQ27220)")
        waitFor("Screen mirror unavailable")
        // The agent itself is the hero of the tab, with its state named under it.
        compose.onNodeWithContentDescription("Lapis agent: idle").assertIsDisplayed()
        compose.onNodeWithText("Idle").assertIsDisplayed()
        push(Fixtures.stateSync("THINKING", activeChatId = chatId, step = 3, action = ""))
        waitForHead("thinking")
        compose.onNodeWithText("Thinking").assertIsDisplayed()
        compose.onNodeWithText("open settings").assertIsDisplayed() // what it is busy with
        snap("device")
        compose.onNodeWithText("Dark Ink").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Wake screen").performScrollTo().assertIsDisplayed()
        compose.onNodeWithText("Shutdown").performScrollTo().assertIsDisplayed()
        tap("Shutdown")
        waitFor("Powers the CardputerZero off")
        tap("Cancel")
        // The task ends well: one beat of "done", then at rest again with no frame to say so.
        push(Fixtures.stateSync("IDLE"))
        push(Fixtures.taskFinished("done"))
        waitForHead("done")
        waitForHead("idle")

        // Config and each of its screens.
        tap("Config")
        waitFor("Secrets vault")
        tap("KVM hosts")
        waitFor("Test connection")
        compose.onNodeWithText("100.64.10.21").assertExists()
        tap("Test connection")
        waitFor("SSH access")
        waitFor("TimeoutError")
        compose.onNodeWithContentDescription("Back").performClick()

        tap("Agent & LLM")
        waitFor("Save to lapis.toml")
        compose.onNodeWithText("claude-sonnet-5-5").assertExists()
        // One tap switches provider, endpoint, key name and model together, and the key can be added right here.
        tap("MINIMAX") // badges render their label in capitals
        waitFor("https://api.minimax.io/v1")
        compose.onNodeWithText("MINIMAX_API_KEY").assertExists()
        compose.onNodeWithText("MiniMax-M3.1-Flash").assertExists()
        waitFor("KEY NOT STORED")
        waitFor("Not saved yet")
        tap("Add key")
        waitFor("Set MINIMAX_API_KEY")
        // Storing the key for the provider just chosen also saves that choice: nothing is left
        // half done behind a Save button further down the page.
        compose.onNode(hasSetTextAction() and hasText("Value", substring = true)).performTextReplacement("mm-test-key-000000c0de")
        tap("Store")
        // Waiting on what the screen shows keeps the main thread turning, which the answers need.
        waitFor("KEY STORED")
        waitGone("Not saved yet")
        assertTrue("$requests", "PATCH /v1/secrets" in requests && "PUT /v1/config" in requests)
        assertTrue(bodies.getValue("PATCH /v1/secrets"), bodies.getValue("PATCH /v1/secrets").contains(""""MINIMAX_API_KEY":"mm-test-key-000000c0de""""))
        val put = bodies.getValue("PUT /v1/config")
        for (part in listOf(""""provider":"openai"""", """"base_url":"https://api.minimax.io/v1"""", """"api_key_env":"MINIMAX_API_KEY"""", """"model":"MiniMax-M3.1-Flash"""")) {
            assertTrue("$part in $put", put.contains(part))
        }
        assertTrue("the key is not part of the settings", !put.contains("mm-test-key"))
        compose.onNodeWithContentDescription("Back").performClick()

        tap("Secrets vault")
        waitFor("ANTHROPIC_API_KEY")
        compose.onNodeWithText("...a9f2").assertExists()
        compose.onNodeWithText("NOT SET").assertExists()
        compose.onNodeWithContentDescription("Back").performClick()

        tap("Scheduled jobs")
        waitFor("Daily Digest")
        compose.onNodeWithText("Run now").assertExists()
        compose.onNodeWithContentDescription("Back").performClick()

        tap("Paired phones")
        waitFor("Dev token")
        compose.onNodeWithText("THIS PHONE").assertExists()
        compose.onNodeWithContentDescription("Back").performClick()

        tap("App")
        waitFor("APPEARANCE")
        compose.onNodeWithText("Notification settings").assertExists()
        compose.onNodeWithText("Ink").performClick()
        compose.onNodeWithContentDescription("Back").performClick()
        waitFor("Secrets vault")
    }

    @Test
    fun `dialogs sheets and secondary surfaces open without crashing in the ink theme`() {
        pairWithMock()
        show(ThemeMode.INK)
        waitFor("open settings")

        // Chat list: row menu, rename dialog, host picker (two hosts are configured).
        compose.onNodeWithContentDescription("Chat actions").performClick()
        waitFor("Rename")
        tap("Rename")
        waitFor("Rename chat")
        tap("Cancel")
        compose.onNodeWithContentDescription("New chat").performClick()
        waitFor("Run on which host?")
        tap("Cancel")
        tap("arch-dev") // host filter chip
        waitFor("No chats yet")
        tap("All")
        waitFor("open settings")

        // Device switcher.
        tap("devbox")
        waitFor("Paired devices")
        compose.onNodeWithText("Pair another device").assertExists()
        tap("Rename")
        waitFor("Rename device")
        tap("Cancel")
        compose.onNodeWithContentDescription("Close sheet").performClick()
        waitGone("Paired devices")

        // Thread: expand a tool card, open its frame full screen.
        tap("open settings")
        waitFor("click(160, 24) left")
        tap("click(160, 24) left")
        waitFor("Click header")
        compose.onNodeWithText("HOST DEFAULT \u25BE").performClick()
        waitFor("Balanced")
        tap("Balanced")
        waitUntilOrDump("the policy change", 10_000) { "PATCH /v1/chats/$chatId" in requests }
        waitFor("BALANCED \u25BE")

        // An approval with the 1-bit preview and the crosshair frame, shown inline and in the sheet.
        push(Fixtures.stateSync("WAITING_APPROVAL", chatId, 5, approvals = Fixtures.approval("req_1", preview = previewB64())))
        waitFor("Approval needed")
        waitFor("device preview 120x68")
        waitFor("target screen")
        waitForHead("waiting for approval") // behind the sheet, beside the thread's running line
        tap("Later")
        waitGone("Approval needed")
        waitFor("1 APPROVAL") // the top bar keeps reminding
        compose.onAllNodesWithText("Approve once").onFirst().assertExists() // sticky card in the thread
        push(Fixtures.approvalResolved("req_1", "deny", "kill_switch"))
        push(Fixtures.stateSync("PAUSED", chatId, 5))
        waitGone("1 APPROVAL")
        waitForHead("paused")
        waitFor("PAUSED · step 5/25")
        snap("thread-paused-ink")
        // Resolution by someone else is announced; let the snackbar leave before tapping under it.
        waitFor("Denied by the kill switch")
        waitGone("Denied by the kill switch", timeoutMs = 30_000)
        compose.onNodeWithContentDescription("Back").performClick()
        waitForWorkspace()

        // Live KVM while paused: manual assist is enabled.
        tap("Live KVM")
        waitFor("Hand back to agent")
        waitFor("The agent is paused")
        push(Fixtures.stateSync("IDLE"))
        waitGone("Hand back to agent")

        // Config dialogs.
        tap("Config")
        tap("KVM hosts")
        waitFor("Add host")
        tap("Add host")
        waitFor("Display hostname")
        tap("Cancel")
        tap("Edit")
        waitFor("Edit macbook-pro")
        tap("Cancel")
        compose.onNodeWithContentDescription("Back").performClick()
        tap("Secrets vault")
        waitFor("Add a key or secret")
        tap("Add a key or secret")
        waitFor("Name, e.g. OPENROUTER_API_KEY")
        // A provider chip fills in the conventional key name; any other name can be typed.
        tap("MINIMAX")
        waitFor("MINIMAX_API_KEY")
        val nameField = hasSetTextAction() and hasText("Name, e.g. OPENROUTER_API_KEY", substring = true)
        compose.onNode(nameField).performTextReplacement("groq api-key")
        waitFor("Stored as GROQ_API_KEY.")
        // A key pasted into the name is refused: Store stays off even with a value entered.
        compose.onNode(nameField).performTextReplacement("gsk_" + "aB3dE5gH".repeat(6))
        waitFor("looks like a key or password")
        compose.onNode(hasSetTextAction() and hasText("Value", substring = true)).performTextReplacement("x")
        compose.onNodeWithText("Store").assertIsNotEnabled()
        tap("Cancel")
        compose.onNodeWithContentDescription("Back").performClick()
        tap("Scheduled jobs")
        waitFor("Add job")
        tap("Add job")
        waitFor("Cron: minute hour day month weekday")
        tap("Cancel")
        compose.onNodeWithContentDescription("Back").performClick()
        tap("Paired phones")
        waitFor("Revoke access")
        tap("Revoke access")
        waitFor("This is the phone in your hand")
        tap("Cancel")
    }

    @Test
    fun `agent screen says what it knows about the key and refuses a key as its name`() {
        vaultDown = true
        pairWithMock()
        show()
        waitFor("open settings")
        tap("Config")
        waitFor("Secrets vault")
        tap("Agent & LLM")
        waitFor("Save to lapis.toml")
        // The vault could not be read: that is not "the key is missing".
        waitFor("KEY STATUS UNKNOWN")
        compose.onNodeWithText("KEY NOT STORED").assertDoesNotExist()
        compose.onNodeWithText("not on the device yet", substring = true).assertDoesNotExist()
        vaultDown = false
        tap("Retry")
        waitFor("KEY STORED")

        // A key pasted where its name belongs: refused, Save off, nothing sent, nothing repeated.
        val pasted = "gsk_" + "aB3dE5gH".repeat(6)
        compose.onNode(hasSetTextAction() and hasText("Key name (not the key itself)", substring = true)).performTextReplacement(pasted)
        waitFor("looks like a key or password")
        compose.onNodeWithText("Save to lapis.toml").assertIsNotEnabled()
        compose.onNodeWithText("Add key").assertIsNotEnabled()
        assertEquals("only the field itself shows what was typed", 1, compose.onAllNodesWithText(pasted, substring = true).fetchSemanticsNodes().size)
        // Another provider's endpoint with the Anthropic key name still in place is pointed out.
        compose.onNode(hasSetTextAction() and hasText("Key name (not the key itself)", substring = true)).performTextReplacement("ANTHROPIC_API_KEY")
        compose.onNodeWithText("ANTHROPIC \u25BE").performScrollTo().performClick() // the protocol menu
        tap("openai")
        waitFor("OPENAI_API_KEY") // choosing a protocol by hand does not carry the Anthropic key name along
        compose.onNode(hasSetTextAction() and hasText("Key name (not the key itself)", substring = true)).performTextReplacement("ANTHROPIC_API_KEY")
        waitFor("is normally the Anthropic key, but these settings send it to api.openai.com")
        assertTrue("nothing was saved", "PUT /v1/config" !in requests)
    }

    @Test
    fun `approval sheet appears decides and clears when resolved on the device`() {
        pairWithMock()
        show()
        waitFor("open settings")

        // The agent stops at a gate: the sheet pops up over whatever tab is open.
        push(Fixtures.stateSync("WAITING_APPROVAL", chatId, 5, approvals = Fixtures.approval("req_1")))
        push(Fixtures.approval("req_1"))
        waitFor("Approval needed")
        compose.onNodeWithText("Click Retry Build at (840, 412)").assertExists()
        compose.onNodeWithText("Approve once").assertExists()
        compose.onNodeWithText("Approve session").assertExists()
        compose.onNodeWithText("Deny").assertExists()
        compose.onNodeWithText("target (840, 412)").assertExists()

        // Somebody approves on the CardputerZero: the sheet goes away by itself.
        push(Fixtures.approvalResolved("req_1", "approve_once", "device"))
        push(Fixtures.stateSync("ACTING", chatId, 5))
        waitGone("Approval needed")
        assertTrue("nothing was sent from the phone", requests.none { it.startsWith("POST /v1/approvals") })

        // A second gate, decided here.
        push(Fixtures.stateSync("WAITING_APPROVAL", chatId, 6, approvals = Fixtures.approval("req_1", action = "ssh_exec")))
        waitFor("Approval needed")
        compose.onNodeWithText("systemctl status worker").assertExists()
        tap("Deny")
        waitUntilOrDump("the decision request", 10_000) { "POST /v1/approvals/req_1" in requests }
        waitGone("Approval needed")
    }

    @Test
    fun `dev connect pairs from the welcome screen and opens the workspace`() {
        container.sessions.start()
        show()
        waitFor("Pair a new CardputerZero")
        compose.onNodeWithText("Pair an existing device").assertExists()
        tap("Dev connect")
        waitFor("Pairing token")
        compose.onNode(hasSetTextAction() and hasText("Daemon address (host:port)", substring = true)).performTextReplacement(host)
        compose.onNode(hasSetTextAction() and hasText("Pairing token", substring = true)).performTextReplacement("dev-local-token")
        tap("Connect")
        waitFor("Connected to devbox (mock)")
        assertEquals("http://$host", container.devices.state.value.active?.endpoint?.baseUrl)
        tap("Continue")
        waitForWorkspace()
        waitFor("open settings")
    }

    @Test
    fun `qr pairing by pasted code confirms the pin and exchanges the nonce`() {
        container.sessions.start()
        show()
        waitFor("Pair an existing device")
        tap("Pair an existing device")
        waitFor("Or paste a lapis://pair code")
        compose.onNode(hasSetTextAction() and hasText("Or paste a lapis://pair code", substring = true))
            .performTextReplacement("lapis://pair?host=$host&nonce=$nonce&pin=114773&tls=0")
        compose.onNodeWithText("Use pasted code").performScrollTo().performClick()
        waitFor("Pair with this device?")
        compose.onNodeWithText("114773").assertExists()
        assertTrue("nothing is exchanged before the user confirms", "POST /v1/pair/exchange" !in requests)
        tap("Pair")
        waitFor("Connected to lapis-zero")
        assertTrue("POST /v1/pair/exchange" in requests)
        assertEquals("qr-issued-token-0123456789abcdef", container.devices.state.value.active?.token)
        tap("Continue")
        waitForWorkspace()
    }

    @Test
    fun `bluetooth wizard opens and explains what it needs`() {
        container.sessions.start()
        show()
        waitFor("Pair a new CardputerZero")
        tap("Pair a new CardputerZero")
        waitFor("Pair over Bluetooth")
        compose.onNodeWithText("Find").assertExists()
        compose.onNodeWithText("Verify").assertExists()
        // Robolectric has no BLE radio: the wizard says so instead of crashing or scanning.
        waitFor("Bluetooth")
        tap("‹ Back")
        waitFor("Dev connect")
    }

    @Test
    fun `host form shares the screen login with ssh or takes one of its own`() {
        // As device software with the SSH features sends it: `ssh.password_env` is always there.
        hosts = hosts.replace("\"identity_file\": null}", "\"identity_file\": null, \"password_env\": \"\"}")
        vault += "LAPIS_VNC_PASSWORD_macbook_pro"
        pairWithMock()
        show()
        openHosts()
        waitFor("Authorise device")

        // A new host. Nothing about SSH logins is asked until there is a screen login to share.
        press("Add host")
        waitFor("Display hostname")
        field("Host id").performTextReplacement("mac-mini")
        field("Display hostname").performTextReplacement("Mac mini")
        field("Tailscale IP").performTextReplacement("100.64.0.9")
        compose.onNodeWithText("Same login as the screen").assertDoesNotExist()
        field("Username", exact = true).performTextReplacement("alice")
        waitFor("Same login as the screen")
        compose.onNode(isToggleable()).assertIsOn() // the default for a new host
        compose.onNodeWithText("SSH password (optional)").assertDoesNotExist()
        compose.onNodeWithText("SSH user").assertExists() // shown, not editable
        waitFor("SSH uses the screen's username and password")
        waitFor("Remote Login must be on (System Settings > General > Sharing)")
        compose.onNodeWithText("Advanced: identity file on the device (optional)").assertExists()
        field("Password", exact = true).performTextReplacement("screen pass 1")
        assertMasked("screen pass 1")
        press("Save")
        waitGone("Display hostname")
        waitFor("Host mac-mini saved")
        // One vault write, under the VNC name; the SSH part of the host points at the same name.
        assertEquals("""{"set":{"LAPIS_VNC_PASSWORD_mac_mini":"screen pass 1"},"unset":[]}""", bodiesOf("PATCH /v1/secrets").single())
        assertEquals("""{"port":22,"user":"alice","password_env":"LAPIS_VNC_PASSWORD_mac_mini"}""", lastHost("mac-mini").getValue("ssh").toString())
        assertEquals("LAPIS_VNC_PASSWORD_mac_mini", lastHost("mac-mini").getValue("vnc").jsonObject.getValue("password_env").jsonPrimitive.content)

        // Opened again it shows what is stored, and saving it unchanged changes nothing.
        val shared = lastHost("mac-mini")
        press("Edit", index = 1)
        waitFor("Edit mac-mini")
        compose.onNode(isToggleable()).assertIsOn()
        press("Save")
        waitGone("Edit mac-mini")
        assertEquals(shared, lastHost("mac-mini"))
        assertEquals(1, bodiesOf("PATCH /v1/secrets").size)

        // Switched off: a user and a password of its own, under a name of its own. Both
        // passwords typed now still go to the vault in one write.
        press("Edit", index = 1)
        waitFor("Edit mac-mini")
        compose.onNode(isToggleable()).performSemanticsAction(SemanticsActions.OnClick)
        waitFor("SSH password (optional)")
        waitFor("Without a password the device logs in with its own key only")
        compose.onNodeWithText("SSH user").assertDoesNotExist()
        field("User", exact = true).assert(hasText("alice")) // starts from the screen's user
        field("User", exact = true).performTextReplacement("deploy")
        field("SSH password (optional)").performTextReplacement("shell pass 2")
        field("Password (stored").performTextReplacement("screen pass 3")
        assertMasked("shell pass 2")
        assertMasked("screen pass 3")
        press("Save")
        waitGone("Edit mac-mini")
        assertEquals(2, bodiesOf("PATCH /v1/secrets").size)
        assertEquals(
            """{"set":{"LAPIS_VNC_PASSWORD_mac_mini":"screen pass 3","LAPIS_SSH_PASSWORD_mac_mini":"shell pass 2"},"unset":[]}""",
            bodiesOf("PATCH /v1/secrets").last(),
        )
        assertEquals("""{"port":22,"user":"deploy","password_env":"LAPIS_SSH_PASSWORD_mac_mini"}""", lastHost("mac-mini").getValue("ssh").toString())
        assertTrue("the vault write comes before the host", requests.lastIndexOf("PATCH /v1/secrets") < requests.lastIndexOf("PUT /v1/hosts"))

        // That one round-trips too, with its password left where it is.
        val own = lastHost("mac-mini")
        press("Edit", index = 1)
        waitFor("Edit mac-mini")
        compose.onNode(isToggleable()).assertIsOff()
        waitFor("SSH password (stored; type to replace)")
        press("Save")
        waitGone("Edit mac-mini")
        assertEquals(own, lastHost("mac-mini"))
        // ...and so does a host that logs in with the key only.
        val before = LapisJson.decodeFromString<Host>(parse(hosts).getValue("macbook-pro").toString())
        press("Edit", index = 0)
        waitFor("Edit macbook-pro")
        compose.onNode(isToggleable()).assertIsOff()
        press("Save")
        waitGone("Edit macbook-pro")
        assertEquals(before, LapisJson.decodeFromString<Host>(lastHost("macbook-pro").toString()))
        assertEquals("", lastHost("macbook-pro").getValue("ssh").jsonObject.getValue("password_env").jsonPrimitive.content)
        assertEquals(2, bodiesOf("PATCH /v1/secrets").size)

        // A password left the phone in the vault write and in no other request.
        val passwords = listOf("pass 1", "pass 2", "pass 3")
        assertEquals(setOf("PATCH /v1/secrets"), sent.filter { (_, body) -> passwords.any(body::contains) }.map { it.first }.toSet())
    }

    @Test
    fun `passwords typed into the host form stay out of the saved state`() {
        pairWithMock()
        show()
        openHosts()
        waitFor("Authorise device")
        press("Add host")
        waitFor("Display hostname")
        field("Host id").performTextReplacement("mac-mini")
        field("Username", exact = true).performTextReplacement("alice")
        waitFor("Same login as the screen")
        compose.onNode(isToggleable()).performSemanticsAction(SemanticsActions.OnClick)
        waitFor("SSH password (optional)")
        field("User", exact = true).performTextReplacement("deploy")
        field("Password", exact = true).performTextReplacement("screen pass 4")
        field("SSH password (optional)").performTextReplacement("shell pass 5")
        compose.waitForIdle()
        // What Android keeps across process death: the form's text, and neither password.
        val state = savedState()
        assertTrue("the saved state can be read", state.contains("mac-mini") && state.contains("deploy"))
        assertFalse(state.contains("screen pass 4") || state.contains("shell pass 5"))
    }

    @Test
    fun `host card lists agent tools authorises the device and shows its key`() {
        probe = """{"host_id": "macbook-pro", "tailscale": {"ok": true, "latency_ms": 12, "detail": "pong"},
            "vnc": {"ok": true, "latency_ms": 85, "detail": "ok"}, "ssh": {"ok": true, "latency_ms": 140, "detail": "ok (device key)"},
            "agents": [{"id": "claude", "name": "Claude Code", "path": "/opt/homebrew/bin/claude"},
                       {"id": "opencode", "name": "opencode", "path": "/usr/local/bin/opencode"}, {"id": "freya", "name": "Freya", "path": "/usr/local/bin/freya"}]}"""
        pairWithMock()
        show()
        openHosts()
        press("Test connection")
        waitFor("ok (device key)")
        compose.onNodeWithText("Agent tools: Claude Code, opencode, Freya").assertExists()

        // The device's own key, and the one line that authorises it by hand.
        press("Device key")
        waitFor(Fixtures.FINGERPRINT)
        compose.onNodeWithText(Fixtures.PUBLIC_KEY).assertExists()
        press("Copy command")
        waitFor("Copied")
        val clipboard = ApplicationProvider.getApplicationContext<Context>().getSystemService(ClipboardManager::class.java)
        assertEquals(
            "mkdir -p ~/.ssh && chmod 700 ~/.ssh && echo '${Fixtures.PUBLIC_KEY}' >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys",
            clipboard.primaryClip?.getItemAt(0)?.text?.toString(),
        )
        press("Close")
        waitGone(Fixtures.FINGERPRINT)

        // Authorising says in plain words what it does and asks first.
        press("Authorise device")
        waitFor("Authorise the device on macbook-pro?")
        assertEquals(1, shown("adds the device's own SSH key to macbook-pro"))
        assertEquals(1, shown("afterwards SSH works without the password"))
        assertTrue("nothing is sent before the owner agrees", requests.none { it.endsWith("/ssh/authorize") })
        // No login works: the reason stays in the dialog, where it can be read.
        authorize = errorResponse(422, "ssh_login_failed", "No login works for alice@100.64.10.21: turn on Remote Login or store its password")
        press("Authorise")
        waitFor("Could not authorise the device: No login works for alice@100.64.10.21")
        compose.onNodeWithText("Authorise the device on macbook-pro?").assertExists()
        // Tried again once that is put right.
        authorize = jsonResponse("""{"ok": true, "already": false, "detail": "key added for alice"}""")
        press("Authorise")
        waitGone("Authorise the device on macbook-pro?")
        waitFor("Device authorised on macbook-pro", timeoutMs = 30_000)
        authorize = jsonResponse("""{"ok": true, "already": true, "detail": "already authorised"}""")
        press("Authorise device")
        waitFor("Authorise the device on macbook-pro?")
        press("Authorise")
        waitGone("Authorise the device on macbook-pro?")
        waitFor("The device was already authorised on macbook-pro", timeoutMs = 30_000)
        assertEquals(3, requests.count { it == "POST /v1/hosts/macbook-pro/ssh/authorize" })
    }

    @Test
    fun `a thread runs with a tool found on the host in a folder of its own`() {
        val reason = "SSH login failed for alice@100.64.10.21: permission denied"
        agents = errorResponse(409, "ssh_unavailable", reason)
        pairWithMock()
        show()
        waitFor("open settings")
        tap("open settings")
        waitFor("Mock run complete: 6 scripted steps.")
        compose.onNodeWithText("RUN WITH").assertExists()

        // SSH does not work for the host yet: the reason, and the Lapis agent as the only choice.
        waitFor(reason)
        compose.onNodeWithText("LAPIS AGENT ▾").performClick()
        waitFor("Look for tools again")
        compose.onNodeWithText("Lapis agent").assertExists()
        compose.onNodeWithText("Claude Code").assertDoesNotExist()
        // Put right (the device authorised, say) and looked for again: the tools on the host.
        agents = jsonResponse(Fixtures.AGENTS)
        tap("Look for tools again")
        waitGone(reason)
        compose.onNodeWithText("LAPIS AGENT ▾").performClick()
        waitFor("Claude Code")
        compose.onNodeWithText("opencode").assertExists()
        compose.onNodeWithText("Freya").assertExists()
        compose.onNodeWithText("FOLDER").assertDoesNotExist() // the device's own agent has no folder
        tap("Claude Code")
        waitFor("CLAUDE CODE ▾")
        assertEquals("""{"agent":"claude"}""", bodies.getValue("PATCH /v1/chats/$chatId"))

        // The folder the tool works in: the home folder until one is given.
        compose.onNodeWithText("~").assertExists()
        compose.onNodeWithText("FOLDER").performClick()
        waitFor("Working folder")
        field("Folder on macbook-pro").performTextReplacement("  ~/Projects/Lapis-Zero ")
        tap("Save")
        waitFor("~/Projects/Lapis-Zero") // as typed: a path is not set in capitals
        assertEquals("""{"workdir":"~/Projects/Lapis-Zero"}""", bodies.getValue("PATCH /v1/chats/$chatId"))

        // A tool that is gone the next time stays on the chat that is bound to it.
        agents = jsonResponse("""{"agents": [{"id": "opencode", "name": "opencode", "path": "/usr/local/bin/opencode"}]}""")
        compose.onNodeWithText("CLAUDE CODE ▾").performClick()
        waitFor("Look for tools again")
        tap("Look for tools again")
        waitFor("CLAUDE ▾") // by its id: the name went with the list
        compose.onNodeWithText("CLAUDE ▾").performClick()
        waitFor("Lapis agent")
        compose.onNodeWithText("opencode").assertExists()
        compose.onNodeWithText("claude").assertExists()
        compose.onNodeWithText("Freya").assertDoesNotExist()
        tap("claude") // the current choice: nothing to change
        waitGone("Lapis agent")

        // While it works, the thread shows the tool and its latest progress line.
        // This sync names no step (device software from before `action`): the head then goes by
        // the kind of the activity that last reported back, here the tool's own progress line.
        push(Fixtures.stateSync("ACTING", activeChatId = chatId, step = 0))
        push(Fixtures.activity(step = 0, kind = "agent_run", summary = "Reading ui/src/main.rs", screenshotId = null, status = "running"))
        waitFor("RUNNING A COMMAND · claude · Reading ui/src/main.rs")
        waitForHead("running a command")
        push(Fixtures.stateSync("IDLE"))
        waitGone("STOP TASK")

        // The chat list says which chats are run by a tool.
        compose.onNodeWithContentDescription("Back").performClick()
        waitForWorkspace()
        waitFor("CLAUDE")
        assertEquals(2, requests.count { it == "PATCH /v1/chats/$chatId" })
    }

    @Test
    fun `older device software keeps working and is sent nothing new`() {
        sshFeatures = false
        pairWithMock()
        show()
        waitFor("open settings")

        // A thread: the menu is there, closed, with the one line that says why.
        tap("open settings")
        waitFor("Mock run complete: 6 scripted steps.")
        waitFor(ChatRepository.NO_SSH_FEATURES)
        compose.onNodeWithText("LAPIS AGENT ▾").performClick()
        compose.waitForIdle()
        compose.onNodeWithText("Look for tools again").assertDoesNotExist()
        compose.onNodeWithText("FOLDER").assertDoesNotExist()
        // The policy menu next to it is what it was.
        compose.onNodeWithText("HOST DEFAULT ▾").performClick()
        waitFor("Balanced")
        tap("Balanced")
        waitFor("BALANCED ▾")
        assertEquals("""{"policy_override":"balanced"}""", bodies.getValue("PATCH /v1/chats/$chatId"))
        compose.onNodeWithContentDescription("Back").performClick()
        waitForWorkspace()

        // Hosts: no authorising, no device key, the note once.
        tap("Config")
        waitFor("Secrets vault")
        tap("KVM hosts")
        waitFor("Test connection")
        waitFor(ChatRepository.NO_SSH_FEATURES)
        compose.onNodeWithText("Authorise device").assertDoesNotExist()
        compose.onNodeWithText("Device key").assertDoesNotExist()
        press("Test connection")
        waitFor("TimeoutError")
        assertEquals(0, shown("Agent tools"))

        // The form is the one from before, and a host goes back without the new field.
        press("Edit")
        waitFor("Edit macbook-pro")
        compose.onNodeWithText("Same login as the screen").assertDoesNotExist()
        assertEquals(0, shown("SSH password"))
        assertEquals("on the page and in the form", 2, shown(ChatRepository.NO_SSH_FEATURES))
        field("User", exact = true).performTextReplacement("bob")
        field("Password", exact = true).performTextReplacement("screen pass 9")
        press("Save")
        waitGone("Edit macbook-pro")
        assertEquals("""{"port":22,"user":"bob"}""", lastHost("macbook-pro").getValue("ssh").toString())
        assertEquals("""{"set":{"LAPIS_VNC_PASSWORD_macbook_pro":"screen pass 9"},"unset":[]}""", bodiesOf("PATCH /v1/secrets").single())

        assertTrue("$requests", requests.none { it.endsWith("/agents") || it.endsWith("/ssh/authorize") })
        val newFields = listOf("\"agent\"", "\"workdir\"", "\"ssh\":{\"port\":22,\"user\":\"bob\",\"password_env\"")
        assertTrue("$sent", sent.none { (_, body) -> newFields.any(body::contains) })
    }
}
