package dev.lapiszero.app

import android.content.Context
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import dev.lapiszero.app.data.ble.BleProvisioner
import dev.lapiszero.app.data.local.AppSettings
import dev.lapiszero.app.data.local.ChatCache
import dev.lapiszero.app.data.local.KeystoreSecureStore
import dev.lapiszero.app.data.local.LapisDatabase
import dev.lapiszero.app.data.local.RoomChatCache
import dev.lapiszero.app.data.local.SecureStore
import dev.lapiszero.app.data.remote.Approval
import dev.lapiszero.app.domain.DeviceRepository
import dev.lapiszero.app.domain.HostBook
import dev.lapiszero.app.domain.LapisSession
import dev.lapiszero.app.domain.SessionManager
import dev.lapiszero.app.notifications.AgentWatchService
import dev.lapiszero.app.notifications.ApprovalNotifier
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/** A request to open a specific chat, raised by tapping an approval notification. */
data class NavRequest(val deviceId: String?, val chatId: String?)

/** Manual dependency container; one per process. */
class AppContainer(
    private val context: Context,
    /** Where hosts and tokens live; only tests replace the Keystore-backed vault. */
    secureStore: SecureStore = KeystoreSecureStore(context),
    /** KVM-host passwords typed into the app (see [HostBook]): its own Keystore-sealed entry. */
    hostBookStore: SecureStore = KeystoreSecureStore(context, "lapis_hostbook"),
) {
    val appScope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    val settings = AppSettings(context)
    val devices = DeviceRepository(secureStore)
    val hostBook = HostBook(hostBookStore)
    val chatCache: ChatCache by lazy {
        runCatching { RoomChatCache(LapisDatabase.open(context).chatCache()) }.getOrDefault(ChatCache.None)
    }
    val sessions by lazy { SessionManager(devices, chatCache, appScope) }
    val notifier = ApprovalNotifier(context)
    val ble by lazy { BleProvisioner(context) }

    /** True while any activity is at least started. */
    val foreground = MutableStateFlow(false)
    val navRequests = MutableStateFlow<NavRequest?>(null)

    @OptIn(ExperimentalCoroutinesApi::class)
    fun start() {
        notifier.ensureChannels()
        devices.purgeUnprovisioned()
        sessions.start()
        ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStart(owner: LifecycleOwner) {
                foreground.value = true
                sessions.current.value?.repo?.reconnectNow()
            }

            override fun onStop(owner: LifecycleOwner) {
                foreground.value = false
            }
        })

        // Approval alerts: posted only while backgrounded, withdrawn the moment they resolve.
        appScope.launch {
            sessions.current
                .flatMapLatest { session ->
                    session?.repo?.approvals?.map { session to it }
                        ?: flowOf<Pair<LapisSession?, List<Approval>>>(null to emptyList())
                }
                .combine(foreground) { (session, approvals), inForeground ->
                    Triple(session, approvals, inForeground)
                }
                .collect { (session, approvals, inForeground) ->
                    notifier.reconcile(
                        deviceId = session?.device?.id,
                        deviceName = session?.device?.name.orEmpty(),
                        wanted = if (inForeground) emptyList() else approvals,
                    )
                }
        }

        // The foreground service lives exactly as long as a task is running. Android only lets
        // an app start one while it is visible, so it is started when both are true (a task
        // begins while the app is open, or the app is opened while a task runs) and then
        // carries on in the background until the agent goes idle.
        appScope.launch {
            sessions.current
                .flatMapLatest { it?.repo?.state ?: flowOf(null) }
                .map { it?.isRunning == true }
                .combine(foreground) { running, inForeground -> running && inForeground }
                .distinctUntilChanged()
                .collect { start -> if (start) AgentWatchService.start(context) }
        }
    }
}
