package dev.lapiszero.app.data.local

import android.content.Context
import com.google.crypto.tink.Aead
import com.google.crypto.tink.KeyTemplates
import com.google.crypto.tink.RegistryConfiguration
import com.google.crypto.tink.aead.AeadConfig
import com.google.crypto.tink.integration.android.AndroidKeysetManager
import java.io.File
import java.io.IOException
import java.security.GeneralSecurityException

/** A single encrypted-at-rest text blob. The only place device hosts and bearer tokens live. */
interface SecureStore {
    /** Null when nothing is stored or the stored blob can no longer be decrypted. */
    fun read(): String?

    fun write(value: String)

    fun clear()
}

/**
 * Tink AES-256-GCM AEAD whose keyset is wrapped by a master key held in the Android Keystore
 * (`android-keystore://`). The ciphertext lives in `noBackupFilesDir`, so it is neither part of
 * cloud backups nor of device-to-device transfers; without the Keystore key it is useless anyway.
 *
 * Nothing in here logs.
 */
class KeystoreSecureStore(context: Context, name: String = "lapis_vault") : SecureStore {
    private val appContext = context.applicationContext
    private val file = File(appContext.noBackupFilesDir, "$name.bin")
    private val associatedData = name.toByteArray(Charsets.UTF_8)
    private val keysetName = "${name}_keyset"
    private val prefFile = "${name}_keyset_prefs"
    private val masterKeyUri = "android-keystore://${name}_master"

    @Volatile
    private var cached: Aead? = null

    @Synchronized
    private fun aead(): Aead {
        cached?.let { return it }
        AeadConfig.register()
        val handle = AndroidKeysetManager.Builder()
            .withSharedPref(appContext, keysetName, prefFile)
            .withKeyTemplate(KeyTemplates.get("AES256_GCM"))
            .withMasterKeyUri(masterKeyUri)
            .build()
            .keysetHandle
        return handle.getPrimitive(RegistryConfiguration.get(), Aead::class.java).also { cached = it }
    }

    @Synchronized
    override fun read(): String? {
        if (!file.exists()) return null
        return try {
            aead().decrypt(file.readBytes(), associatedData).toString(Charsets.UTF_8)
        } catch (e: GeneralSecurityException) {
            null // Keystore key lost (restore to a new phone, cleared credentials): pair again.
        } catch (e: IOException) {
            null
        } catch (e: RuntimeException) {
            null // Some vendor Keystore implementations fail with unchecked exceptions.
        }
    }

    /** Throws [IOException] when the Keystore refuses to work; nothing is written in clear. */
    @Synchronized
    override fun write(value: String) {
        val sealed = try {
            aead().encrypt(value.toByteArray(Charsets.UTF_8), associatedData)
        } catch (e: GeneralSecurityException) {
            throw IOException("Secure storage is unavailable on this phone", e)
        } catch (e: RuntimeException) {
            throw IOException("Secure storage is unavailable on this phone", e)
        }
        val tmp = File(file.parentFile, file.name + ".tmp")
        tmp.writeBytes(sealed)
        if (!tmp.renameTo(file)) {
            file.writeBytes(sealed)
            tmp.delete()
        }
    }

    @Synchronized
    override fun clear() {
        file.delete()
    }
}
