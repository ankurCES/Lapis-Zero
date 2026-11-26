package dev.lapiszero.app

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.lapiszero.app.notifications.ApprovalNotifier
import dev.lapiszero.app.theme.LapisTheme
import dev.lapiszero.app.ui.navigation.LapisRoot

class MainActivity : ComponentActivity() {
    private val container: AppContainer get() = (application as LapisApp).container

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) handleIntent(intent)
        setContent {
            val mode by container.settings.themeMode.collectAsStateWithLifecycle()
            LapisTheme(mode) { LapisRoot(container) }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleIntent(intent)
    }

    private fun handleIntent(intent: Intent?) {
        if (intent?.action != ApprovalNotifier.ACTION_OPEN) return
        container.navRequests.value = NavRequest(
            deviceId = intent.getStringExtra(ApprovalNotifier.EXTRA_DEVICE_ID),
            chatId = intent.getStringExtra(ApprovalNotifier.EXTRA_CHAT_ID),
        )
    }
}
