package dev.lapiszero.app.notifications

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import dev.lapiszero.app.LapisApp
import dev.lapiszero.app.data.remote.Decision
import dev.lapiszero.app.domain.DecideResult
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull

/** Executes Approve once / Deny (and the kill switch) straight from a notification. */
class ApprovalActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val container = (context.applicationContext as? LapisApp)?.container ?: return
        val pending = goAsync()
        container.appScope.launch {
            try {
                // A receiver has about ten seconds; stay well inside that.
                withTimeoutOrNull(8_000) {
                    when (intent.action) {
                        ApprovalNotifier.ACTION_DECIDE -> decide(container, intent)
                        ApprovalNotifier.ACTION_KILL ->
                            container.sessions.current.value?.repo?.killAll()

                        else -> Unit
                    }
                }
            } finally {
                pending.finish()
            }
        }
    }

    private suspend fun decide(container: dev.lapiszero.app.AppContainer, intent: Intent) {
        val deviceId = intent.getStringExtra(ApprovalNotifier.EXTRA_DEVICE_ID) ?: return
        val requestId = intent.getStringExtra(ApprovalNotifier.EXTRA_REQUEST_ID) ?: return
        val decision = intent.getStringExtra(ApprovalNotifier.EXTRA_DECISION)
            ?.takeIf { it == Decision.APPROVE_ONCE || it == Decision.DENY } ?: return
        when (val result = container.sessions.decide(deviceId, requestId, decision)) {
            DecideResult.Done, DecideResult.AlreadyResolved -> container.notifier.cancel(requestId)
            is DecideResult.Failed -> container.notifier.showFailure(requestId, result.message)
        }
    }
}
