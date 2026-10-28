package com.matthewdovi.mgnotify

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

/**
 * One account's connection to the server's event stream (GET /events): the
 * server sends each change the moment it happens, so nothing is polled.
 * Reconnects by itself; `onEvent` runs on the main thread with the event's
 * name and data, plus these of its own:
 * - "unauthorized": the session is no longer valid (the stream stops);
 * - "disconnected": the connection dropped (data has "failures", the count in a row).
 * A "ready" event after a reconnect carries "reconnected": true.
 */
class EventStream(
    private val serverURL: String,
    private val sessionToken: String,
    private val onEvent: suspend (name: String, data: JSONObject) -> Unit,
) {
    private var job: Job? = null

    /** The open connection, closed by stop(): a blocking read doesn't notice cancellation. */
    @Volatile
    private var connection: HttpURLConnection? = null

    fun start(scope: CoroutineScope) {
        if (job?.isActive == true) return
        job = scope.launch { run() }
    }

    fun stop() {
        job?.cancel()
        job = null
        val open = connection
        connection = null
        if (open != null) Thread { open.disconnect() }.start()
    }

    private suspend fun run() {
        var failures = 0
        var connectedBefore = false
        while (currentCoroutineContextIsActive()) {
            val ended = withContext(Dispatchers.IO) {
                val connection = URL(serverURL.trimEnd('/') + "/events").openConnection() as HttpURLConnection
                this@EventStream.connection = connection
                try {
                    connection.connectTimeout = 15_000
                    // The server sends a keep-alive every 20 seconds.
                    connection.readTimeout = 60_000
                    connection.setRequestProperty("Authorization", "Bearer $sessionToken")
                    connection.setRequestProperty("Accept", "text/event-stream")
                    if (connection.responseCode == 401) {
                        withContext(Dispatchers.Main) { onEvent("unauthorized", JSONObject()) }
                        return@withContext true
                    }
                    if (connection.responseCode != 200) throw IOException("HTTP ${connection.responseCode}")
                    failures = 0
                    var name: String? = null
                    val data = StringBuilder()
                    connection.inputStream.bufferedReader(Charsets.UTF_8).use { reader ->
                        while (true) {
                            val line = reader.readLine() ?: break
                            when {
                                line.startsWith("event:") -> name = line.substring(6).trim()
                                line.startsWith("data:") -> data.append(line.substring(5).trim())
                                line.isEmpty() && name != null -> {
                                    val event = name
                                    val payload = runCatching { JSONObject(data.toString().ifEmpty { "{}" }) }.getOrDefault(JSONObject())
                                    if (event == "ready") payload.put("reconnected", connectedBefore)
                                    name = null
                                    data.setLength(0)
                                    withContext(Dispatchers.Main) { onEvent(event, payload) }
                                    if (event == "ready") connectedBefore = true
                                    if (event == "signed_out" || event == "account_deleted") return@withContext true
                                }
                            }
                        }
                    }
                    false
                } catch (error: IOException) {
                    false
                } finally {
                    if (this@EventStream.connection === connection) this@EventStream.connection = null
                    connection.disconnect()
                }
            }
            if (ended) return
            failures += 1
            onEvent("disconnected", JSONObject().put("failures", failures))
            delay(minOf(30_000L, 1_000L shl minOf(failures - 1, 5)))
        }
    }

    private suspend fun currentCoroutineContextIsActive(): Boolean =
        kotlin.coroutines.coroutineContext.isActive
}
