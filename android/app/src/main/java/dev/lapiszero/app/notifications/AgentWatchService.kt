package dev.lapiszero.app.notifications

import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import dev.lapiszero.app.LapisApp
import dev.lapiszero.app.MainActivity
import dev.lapiszero.app.R
import dev.lapiszero.app.data.remote.DeviceState
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch

/**
 * Keeps the process (and with it the device WebSocket) alive while an agent task is running,
 * so approval requests still reach the phone when the app is in the background. It stops
 * itself a few seconds after the agent goes idle and never runs otherwise.
 */
class AgentWatchService : Service() {
    private var job: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    @OptIn(ExperimentalCoroutinesApi::class)
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val container = (application as LapisApp).container
        if (!enterForeground(notification(container.sessions.current.value?.repo?.state?.value))) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (job == null) {
            job = container.appScope.launch {
                container.sessions.current
                    .flatMapLatest { it?.repo?.state ?: flowOf(null) }
                    .collectLatest { state ->
                        if (state?.isRunning == true) {
                            enterForeground(notification(state))
                        } else {
                            // Steps come in bursts; do not flap between two of them.
                            delay(IDLE_GRACE_MS)
                            stopSelf()
                        }
                    }
            }
        }
        return START_NOT_STICKY
    }

    /** Android 15 caps `dataSync` services at six hours a day; leave gracefully. */
    override fun onTimeout(startId: Int, fgsType: Int) {
        stopSelf()
    }

    override fun onDestroy() {
        job?.cancel()
        job = null
        super.onDestroy()
    }

    private fun enterForeground(notification: Notification): Boolean = try {
        ServiceCompat.startForeground(
            this, ApprovalNotifier.ID_AGENT, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
        )
        true
    } catch (e: IllegalStateException) {
        // ForegroundServiceStartNotAllowedException: the task started while we were in the background.
        false
    } catch (e: SecurityException) {
        false
    }

    private fun notification(state: DeviceState?): Notification {
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val kill = PendingIntent.getBroadcast(
            this,
            1,
            Intent(this, ApprovalActionReceiver::class.java).setAction(ApprovalNotifier.ACTION_KILL),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val task = state?.activeTask
        val text = when {
            task == null -> getString(R.string.agent_running)
            else -> getString(R.string.agent_running_step, state.activeHost, task.step, task.maxSteps)
        }
        return NotificationCompat.Builder(this, ApprovalNotifier.CHANNEL_AGENT)
            .setSmallIcon(R.drawable.ic_stat_lapis)
            .setContentTitle(task?.title?.takeIf { it.isNotBlank() } ?: getString(R.string.agent_running))
            .setContentText(text)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setContentIntent(open)
            .addAction(0, getString(R.string.kill_all), kill)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }

    companion object {
        private const val IDLE_GRACE_MS = 4_000L

        /** No-op when the system forbids it (the app is in the background). */
        fun start(context: Context) {
            try {
                ContextCompat.startForegroundService(context, Intent(context, AgentWatchService::class.java))
            } catch (e: IllegalStateException) {
                // Background start restriction: approvals will show when the app is opened.
            } catch (e: SecurityException) {
                // Same outcome.
            }
        }
    }
}
