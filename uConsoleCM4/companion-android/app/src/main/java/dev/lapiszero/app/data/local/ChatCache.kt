package dev.lapiszero.app.data.local

import android.content.Context
import androidx.room.ColumnInfo
import androidx.room.Dao
import androidx.room.Database
import androidx.room.Entity
import androidx.room.Insert
import androidx.room.OnConflictStrategy
import androidx.room.Query
import androidx.room.Room
import androidx.room.RoomDatabase
import androidx.room.Transaction
import dev.lapiszero.app.data.remote.Chat
import dev.lapiszero.app.data.remote.LapisJson
import dev.lapiszero.app.data.remote.Message

/**
 * Offline cache of chat lists and recent messages, keyed by the local paired-device id.
 * Holds conversation content only: hosts and bearer tokens never enter this database.
 */
interface ChatCache {
    suspend fun chats(deviceId: String): List<Chat>

    suspend fun saveChats(deviceId: String, chats: List<Chat>)

    suspend fun messages(deviceId: String, chatId: String): List<Message>

    suspend fun saveMessages(deviceId: String, chatId: String, messages: List<Message>)

    suspend fun removeChat(deviceId: String, chatId: String)

    suspend fun clearDevice(deviceId: String)

    /** No persistence (unit tests, or when the database cannot be opened). */
    object None : ChatCache {
        override suspend fun chats(deviceId: String): List<Chat> = emptyList()
        override suspend fun saveChats(deviceId: String, chats: List<Chat>) = Unit
        override suspend fun messages(deviceId: String, chatId: String): List<Message> = emptyList()
        override suspend fun saveMessages(deviceId: String, chatId: String, messages: List<Message>) = Unit
        override suspend fun removeChat(deviceId: String, chatId: String) = Unit
        override suspend fun clearDevice(deviceId: String) = Unit
    }
}

@Entity(tableName = "cached_chats", primaryKeys = ["device_id", "chat_id"])
data class CachedChat(
    @ColumnInfo(name = "device_id") val deviceId: String,
    @ColumnInfo(name = "chat_id") val chatId: String,
    @ColumnInfo(name = "updated_at") val updatedAt: Long,
    val json: String,
)

@Entity(tableName = "cached_messages", primaryKeys = ["device_id", "chat_id", "message_id"])
data class CachedMessage(
    @ColumnInfo(name = "device_id") val deviceId: String,
    @ColumnInfo(name = "chat_id") val chatId: String,
    @ColumnInfo(name = "message_id") val messageId: Long,
    val json: String,
)

@Dao
abstract class ChatCacheDao {
    @Query("SELECT * FROM cached_chats WHERE device_id = :deviceId ORDER BY updated_at DESC")
    abstract suspend fun chats(deviceId: String): List<CachedChat>

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    abstract suspend fun insertChats(rows: List<CachedChat>)

    @Query("DELETE FROM cached_chats WHERE device_id = :deviceId")
    abstract suspend fun deleteChats(deviceId: String)

    @Query("DELETE FROM cached_chats WHERE device_id = :deviceId AND chat_id = :chatId")
    abstract suspend fun deleteChat(deviceId: String, chatId: String)

    @Query(
        "SELECT * FROM cached_messages WHERE device_id = :deviceId AND chat_id = :chatId " +
            "ORDER BY message_id ASC",
    )
    abstract suspend fun messages(deviceId: String, chatId: String): List<CachedMessage>

    @Insert(onConflict = OnConflictStrategy.REPLACE)
    abstract suspend fun insertMessages(rows: List<CachedMessage>)

    @Query("DELETE FROM cached_messages WHERE device_id = :deviceId AND chat_id = :chatId")
    abstract suspend fun deleteMessages(deviceId: String, chatId: String)

    @Query("DELETE FROM cached_messages WHERE device_id = :deviceId")
    abstract suspend fun deleteAllMessages(deviceId: String)

    @Transaction
    open suspend fun replaceChats(deviceId: String, rows: List<CachedChat>) {
        deleteChats(deviceId)
        insertChats(rows)
    }

    @Transaction
    open suspend fun replaceMessages(deviceId: String, chatId: String, rows: List<CachedMessage>) {
        deleteMessages(deviceId, chatId)
        insertMessages(rows)
    }

    @Transaction
    open suspend fun clearDevice(deviceId: String) {
        deleteChats(deviceId)
        deleteAllMessages(deviceId)
    }
}

@Database(entities = [CachedChat::class, CachedMessage::class], version = 1, exportSchema = false)
abstract class LapisDatabase : RoomDatabase() {
    abstract fun chatCache(): ChatCacheDao

    companion object {
        fun open(context: Context): LapisDatabase =
            Room.databaseBuilder(context.applicationContext, LapisDatabase::class.java, "lapis_cache.db")
                .fallbackToDestructiveMigration(dropAllTables = true) // it is only a cache
                .build()
    }
}

/** Room-backed [ChatCache]. A broken cache must never break the app, so failures are swallowed. */
class RoomChatCache(private val dao: ChatCacheDao) : ChatCache {
    private val json = LapisJson

    override suspend fun chats(deviceId: String): List<Chat> = safe(emptyList()) {
        dao.chats(deviceId).mapNotNull { runCatching { json.decodeFromString<Chat>(it.json) }.getOrNull() }
    }

    override suspend fun saveChats(deviceId: String, chats: List<Chat>) = safe(Unit) {
        dao.replaceChats(
            deviceId,
            // Live fields are meaningless offline; store the durable part only.
            chats.map {
                val durable = it.copy(active = false, step = null, pendingApprovals = 0)
                CachedChat(deviceId, it.id, it.updatedAt, json.encodeToString(durable))
            },
        )
    }

    override suspend fun messages(deviceId: String, chatId: String): List<Message> = safe(emptyList()) {
        dao.messages(deviceId, chatId)
            .mapNotNull { runCatching { json.decodeFromString<Message>(it.json) }.getOrNull() }
    }

    override suspend fun saveMessages(deviceId: String, chatId: String, messages: List<Message>) =
        safe(Unit) {
            dao.replaceMessages(
                deviceId,
                chatId,
                messages.takeLast(MAX_MESSAGES_PER_CHAT)
                    .map { CachedMessage(deviceId, chatId, it.id, json.encodeToString(it)) },
            )
        }

    override suspend fun removeChat(deviceId: String, chatId: String) = safe(Unit) {
        dao.deleteChat(deviceId, chatId)
        dao.deleteMessages(deviceId, chatId)
    }

    override suspend fun clearDevice(deviceId: String) = safe(Unit) { dao.clearDevice(deviceId) }

    private inline fun <T> safe(fallback: T, block: () -> T): T = try {
        block()
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (e: Exception) {
        fallback
    }

    private companion object {
        const val MAX_MESSAGES_PER_CHAT = 200
    }
}
