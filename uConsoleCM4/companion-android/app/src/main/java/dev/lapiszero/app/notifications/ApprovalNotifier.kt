package dev.lapiszero.app.notifications

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import dev.lapiszero.app.MainActivity
import dev.lapiszero.app.R
import dev.lapiszero.app.data.remote.Approval
import dev.lapiszero.app.data.remote.Decision
import dev.lapiszero.app.theme.LapisPalette
import androidx.compose.ui.graphics.toArgb

/**
 * High-priority alerts for pending approvals while the app is in the background, with
 * Approve once / Deny actions handled by [ApprovalActionReceiver].
 */
class ApprovalNotifier(private val context: Context) {
    private val manager = NotificationManagerCompat.from(context)
    private val shown = HashSet<String>()

    fun ensureChannels() {
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_APPROVALS,
                context.getString(R.string.channel_approvals),
                NotificationManager.IMPORTANCE_HIGH,
            ).apply {
                description = context.getString(R.string.channel_approvals_desc)
                lockscreenVisibility = Notification.VISIBILITY_PRIVATE
            },
        )
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_AGENT,
                context.getString(R.string.channel_agent),
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = context.getString(R.string.channel_agent_desc) },
        )
    }

    fun canPost(): Boolean {
        val granted = Build.VERSION.SDK_INT < 33 || ContextCompat.checkSelfPermission(
            context, Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        return granted && manager.areNotificationsEnabled()
    }

    /**
     * Makes the posted alerts equal to `wanted`: new approvals are posted, resolved ones (here,
     * on the device or on another phone) disappear.
     */
    @Synchronized
    fun reconcile(deviceId: String?, deviceName: String, wanted: List<Approval>) {
        val ids = wanted.mapTo(HashSet()) { it.requestId }
        (shown - ids).forEach(::cancel)
        if (deviceId == null) return
        wanted.filter { it.requestId !in shown }.forEach { post(deviceId, deviceName, it) }
    }

    @Synchronized
    fun cancel(requestId: String) {
        shown.remove(requestId)
        manager.cancel(requestId, ID_APPROVAL)
    }

    private fun post(deviceId: String, deviceName: String, approval: Approval) {
        if (Build.VERSION.SDK_INT >= 33 && ContextCompat.checkSelfPermission(
                context, Manifest.permission.POST_NOTIFICATIONS,
            ) != PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        if (!manager.areNotificationsEnabled()) return
        val open = PendingIntent.getActivity(
            context,
            approval.requestId.hashCode(),
            Intent(context, MainActivity::class.java)
                .setAction(ACTION_OPEN)
                .putExtra(EXTRA_DEVICE_ID, deviceId)
                .putExtra(EXTRA_CHAT_ID, approval.chatId)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val approve = if (allowsQuickApprove(approval)) {
            NotificationCompat.Action.Builder(
                0,
                context.getString(R.string.approve_once),
                decisionIntent(deviceId, approval.requestId, Decision.APPROVE_ONCE),
            )
                // Letting an agent act on a remote machine needs an unlocked phone.
                .setAuthenticationRequired(true)
                .build()
        } else {
            // Tier 2: a notification button cannot be held, so it only opens the approval card.
            NotificationCompat.Action.Builder(0, context.getString(R.string.open_to_hold), open)
                .setAuthenticationRequired(true)
                .build()
        }
        val deny = NotificationCompat.Action.Builder(
            0,
            context.getString(R.string.deny),
            decisionIntent(deviceId, approval.requestId, Decision.DENY),
        ).build()
        val title = tierPrefix(approval) +
            context.getString(R.string.approval_title, actionLabel(approval.action), approval.host)
        val text = approval.summary + approval.reason.takeIf { it.isNotBlank() }?.let { "\n$it" }.orEmpty()
        val notification = NotificationCompat.Builder(context, CHANNEL_APPROVALS)
            .setSmallIcon(R.drawable.ic_stat_lapis)
            .setColor(LapisPalette.Amber.toArgb())
            .setContentTitle(title)
            .setContentText(approval.summary)
            .setSubText(deviceName)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_ALARM)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setOnlyAlertOnce(true)
            .setAutoCancel(true)
            .setContentIntent(open)
            .addAction(approve)
            .addAction(deny)
            .build()
        manager.notify(approval.requestId, ID_APPROVAL, notification)
        shown.add(approval.requestId)
    }

    /** Replaces an alert whose action could not be delivered, so the tap is not silently lost. */
    fun showFailure(requestId: String, message: String) {
        if (Build.VERSION.SDK_INT >= 33 && ContextCompat.checkSelfPermission(
                context, Manifest.permission.POST_NOTIFICATIONS,
            ) != PackageManager.PERMISSION_GRANTED
        ) {
            return
        }
        val open = PendingIntent.getActivity(
            context,
            requestId.hashCode(),
            Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(context, CHANNEL_APPROVALS)
            .setSmallIcon(R.drawable.ic_stat_lapis)
            .setContentTitle(context.getString(R.string.decision_failed))
            .setContentText(message)
            .setAutoCancel(true)
            .setContentIntent(open)
            .build()
        manager.notify(requestId, ID_APPROVAL, notification)
    }

    private fun decisionIntent(deviceId: String, requestId: String, decision: String): PendingIntent =
        PendingIntent.getBroadcast(
            context,
            (requestId + decision).hashCode(),
            Intent(context, ApprovalActionReceiver::class.java)
                .setAction(ACTION_DECIDE)
                .putExtra(EXTRA_DEVICE_ID, deviceId)
                .putExtra(EXTRA_REQUEST_ID, requestId)
                .putExtra(EXTRA_DECISION, decision),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    companion object {
        const val CHANNEL_APPROVALS = "approvals"
        const val CHANNEL_AGENT = "agent"
        const val ID_APPROVAL = 1
        const val ID_AGENT = 2
        const val ACTION_OPEN = "dev.lapiszero.app.action.OPEN"
        const val ACTION_DECIDE = "dev.lapiszero.app.action.DECIDE"
        const val ACTION_KILL = "dev.lapiszero.app.action.KILL"
        const val EXTRA_DEVICE_ID = "device_id"
        const val EXTRA_CHAT_ID = "chat_id"
        const val EXTRA_REQUEST_ID = "request_id"
        const val EXTRA_DECISION = "decision"

        /**
         * Only an approval that a tap may grant gets an "Approve once" button: never a Tier 2 one
         * (hold-to-confirm), whatever the notification would otherwise offer.
         */
        fun allowsQuickApprove(approval: Approval): Boolean = !approval.needsHold

        /** "Tier 2 · " for a daemon with tiers; nothing for one without. */
        fun tierPrefix(approval: Approval): String = approval.tier?.let { "Tier $it · " }.orEmpty()

        fun actionLabel(action: String): String = when (action) {
            "vnc_click" -> "Click"
            "vnc_type" -> "Type"
            "vnc_key" -> "Key press"
            "ssh_exec" -> "SSH command"
            else -> action
        }
    }
}
