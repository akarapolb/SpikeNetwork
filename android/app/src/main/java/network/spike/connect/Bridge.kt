package network.spike.connect

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject

/** The answer was an error: its text is meant for the person. */
class BridgeError(message: String) : Exception(message)

/**
 * The one door to the Rust side. Every command is the same one the desktop app has (see
 * `app/src/bridge.rs`); arguments and answers are JSON. Keys never come through here.
 */
object Bridge {
    private var handle: Long = 0

    @Synchronized
    fun ensureOpen(context: Context) {
        if (handle == 0L) {
            val dir = context.filesDir.resolve("spike").also { it.mkdirs() }
            handle = Native.open(dir.absolutePath)
            check(handle != 0L) { "could not open the engine" }
        }
    }

    /** Run a command off the main thread. Throws [BridgeError] for an error answer. */
    suspend fun call(command: String, args: JSONObject = JSONObject()): Any? =
        withContext(Dispatchers.IO) { callBlocking(command, args) }

    fun callBlocking(command: String, args: JSONObject = JSONObject()): Any? {
        val answer = JSONObject(Native.call(handle, command, args.toString()))
        if (answer.has("err")) throw BridgeError(answer.getString("err"))
        return if (answer.isNull("ok")) null else answer.get("ok")
    }

    suspend fun obj(command: String, args: JSONObject = JSONObject()): JSONObject =
        call(command, args) as JSONObject

    suspend fun arr(command: String, args: JSONObject = JSONObject()): JSONArray =
        call(command, args) as JSONArray
}

fun args(vararg pairs: Pair<String, Any?>): JSONObject {
    val o = JSONObject()
    for ((k, v) in pairs) {
        o.put(k, if (v is List<*>) JSONArray(v) else v)
    }
    return o
}
