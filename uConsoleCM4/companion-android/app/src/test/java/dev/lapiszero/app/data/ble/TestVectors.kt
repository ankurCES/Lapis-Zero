package dev.lapiszero.app.data.ble

import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/** Loads the frozen cross-language vectors from `<repo>/tests/vectors/ble_crypto_vectors.json`. */
object TestVectors {
    val file: File by lazy {
        val candidates = listOfNotNull(
            System.getProperty("lapis.vectors")?.let(::File),
            File("../../tests/vectors/ble_crypto_vectors.json"), // cwd = android/app
            File("../tests/vectors/ble_crypto_vectors.json"), // cwd = android
        )
        candidates.firstOrNull { it.isFile }
            ?: error("ble_crypto_vectors.json not found; tried ${candidates.map { it.absolutePath }}")
    }

    val root: JsonObject by lazy { Json.parseToJsonElement(file.readText()).jsonObject }
    val crypto: JsonObject get() = root.getValue("crypto_vector").jsonObject
    val derived: JsonObject get() = crypto.getValue("derived").jsonObject
    val framing: JsonObject get() = root.getValue("chunk_framing_vector").jsonObject

    fun JsonObject.hex(key: String): ByteArray = str(key).hexToBytesOrNull() ?: error("bad hex in $key")
    fun JsonObject.str(key: String): String = getValue(key).jsonPrimitive.content
    fun JsonObject.int(key: String): Int = getValue(key).jsonPrimitive.int
    fun JsonObject.strings(key: String): List<String> = getValue(key).jsonArray.map { it.jsonPrimitive.content }
}
