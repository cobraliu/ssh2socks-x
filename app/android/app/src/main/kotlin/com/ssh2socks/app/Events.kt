package com.ssh2socks.app

import android.os.Handler
import android.os.Looper
import io.flutter.plugin.common.EventChannel
import org.json.JSONObject

/**
 * Process-wide bridge between the VpnService (running in its own component) and
 * the Flutter EventChannel sink (owned by MainActivity). The service posts JSON
 * events; whatever sink is currently registered receives them on the main thread.
 */
object Events : EventChannel.StreamHandler {
    private val main = Handler(Looper.getMainLooper())
    @Volatile private var sink: EventChannel.EventSink? = null

    @Volatile var lastState: String = "stopped"
        private set

    override fun onListen(args: Any?, events: EventChannel.EventSink?) {
        sink = events
    }

    override fun onCancel(args: Any?) {
        sink = null
    }

    private fun emit(obj: JSONObject) {
        val s = obj.toString()
        main.post { sink?.success(s) }
    }

    fun state(state: String, message: String) {
        lastState = state
        emit(JSONObject().put("type", "state").put("state", state).put("message", message))
    }

    fun log(line: String) =
        emit(JSONObject().put("type", "log").put("line", line))

    fun probe(ok: Boolean, latencyMs: Int, message: String) =
        emit(JSONObject().put("type", "probe").put("ok", ok)
            .put("latencyMs", latencyMs).put("message", message))
}
