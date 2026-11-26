package dev.lapiszero.app.data.local

import android.content.Context
import androidx.core.content.edit
import dev.lapiszero.app.theme.ThemeMode
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Non-secret UI preferences. Hosts and tokens are never stored here (see [SecureStore]). */
class AppSettings(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences("lapis_ui", Context.MODE_PRIVATE)

    private val _themeMode = MutableStateFlow(
        runCatching { ThemeMode.valueOf(prefs.getString(KEY_THEME, null).orEmpty()) }
            .getOrDefault(ThemeMode.SYSTEM),
    )
    val themeMode: StateFlow<ThemeMode> = _themeMode.asStateFlow()

    fun setThemeMode(mode: ThemeMode) {
        prefs.edit { putString(KEY_THEME, mode.name) }
        _themeMode.value = mode
    }

    /** Whether the one-time POST_NOTIFICATIONS request was already shown. */
    val notificationsAsked: Boolean get() = prefs.getBoolean(KEY_NOTIFICATIONS_ASKED, false)

    fun markNotificationsAsked() = prefs.edit { putBoolean(KEY_NOTIFICATIONS_ASKED, true) }

    private companion object {
        const val KEY_THEME = "theme_mode"
        const val KEY_NOTIFICATIONS_ASKED = "notifications_asked"
    }
}
