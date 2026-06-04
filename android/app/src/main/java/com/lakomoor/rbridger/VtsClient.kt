package com.lakomoor.rbridger

import android.os.Handler
import android.os.Looper
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID
import java.util.concurrent.TimeUnit

enum class ConnState { DISCONNECTED, CONNECTING, AUTHENTICATING, AWAITING_APPROVAL, CONNECTED, ERROR }

class VtsClient(
    private val onState: (ConnState) -> Unit,
    private val onHint: (String) -> Unit = {},
) {
    private val http = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .build()

    private var ws: WebSocket? = null
    private var token: String? = null
    private val mainHandler = Handler(Looper.getMainLooper())
    private var authRetryRunnable: Runnable? = null

    var state = ConnState.DISCONNECTED
        private set(v) { field = v; mainHandler.post { onState(v) } }

    fun connect(host: String, port: Int = 8001) {
        if (state != ConnState.DISCONNECTED && state != ConnState.ERROR) return
        cancelRetries()
        state = ConnState.CONNECTING
        val req = Request.Builder().url("ws://$host:$port").build()
        ws = http.newWebSocket(req, Listener())
    }

    fun disconnect() {
        cancelRetries()
        ws?.close(1000, null)
        ws = null
        token = null
        state = ConnState.DISCONNECTED
    }

    fun injectFace(data: FaceData) {
        if (state != ConnState.CONNECTED) return
        val params = JSONArray()
        data.blendShapes.forEach { (name, value) ->
            params.put(JSONObject().put("id", name).put("value", value.toDouble()))
        }
        mapOf("FaceAngleX" to data.pitch, "FaceAngleY" to data.yaw, "FaceAngleZ" to data.roll)
            .forEach { (id, v) -> params.put(JSONObject().put("id", id).put("value", v.toDouble())) }

        send(vtsMsg("InjectParameterDataRequest", JSONObject()
            .put("faceFound", true)
            .put("mode", "set")
            .put("parameterValues", params)))
    }

    private inner class Listener : WebSocketListener() {
        override fun onOpen(ws: WebSocket, response: Response) {
            state = ConnState.AUTHENTICATING
            if (token != null) authenticate(token!!) else requestToken()
        }

        override fun onMessage(ws: WebSocket, text: String) {
            val json = runCatching { JSONObject(text) }.getOrNull() ?: return
            when (json.optString("messageType")) {
                "AuthenticationTokenResponse" -> {
                    token = json.optJSONObject("data")?.optString("authenticationToken")
                    // VTS just showed the approval popup — keep retrying until user clicks Allow
                    state = ConnState.AWAITING_APPROVAL
                    mainHandler.post { onHint("Approve RBridger in VTube Studio popup") }
                    scheduleAuthRetry()
                }
                "AuthenticationResponse" -> {
                    val ok = json.optJSONObject("data")?.optBoolean("authenticated") ?: false
                    if (ok) {
                        cancelRetries()
                        state = ConnState.CONNECTED
                    } else {
                        // User hasn't approved yet — keep retrying every 2s
                        if (state != ConnState.DISCONNECTED) scheduleAuthRetry()
                    }
                }
                "APIError" -> {
                    val msg = json.optJSONObject("data")?.optString("message") ?: "API error"
                    mainHandler.post { onHint(msg) }
                    state = ConnState.ERROR
                }
            }
        }

        override fun onFailure(ws: WebSocket, t: Throwable, r: Response?) {
            cancelRetries()
            mainHandler.post { onHint(t.message ?: "Connection failed") }
            state = ConnState.ERROR
        }

        override fun onClosed(ws: WebSocket, code: Int, reason: String) {
            cancelRetries()
            state = ConnState.DISCONNECTED
        }
    }

    private fun scheduleAuthRetry() {
        cancelRetries()
        val r = Runnable {
            if (token != null && state != ConnState.CONNECTED && state != ConnState.DISCONNECTED) {
                authenticate(token!!)
            }
        }
        authRetryRunnable = r
        mainHandler.postDelayed(r, 2000)
    }

    private fun cancelRetries() {
        authRetryRunnable?.let { mainHandler.removeCallbacks(it) }
        authRetryRunnable = null
    }

    private fun requestToken() = send(vtsMsg("AuthenticationTokenRequest", JSONObject()
        .put("pluginName", "RBridger Android")
        .put("pluginDeveloper", "LakoMoor")
        .put("pluginIcon", "")))

    private fun authenticate(tok: String) = send(vtsMsg("AuthenticationRequest", JSONObject()
        .put("pluginName", "RBridger Android")
        .put("pluginDeveloper", "LakoMoor")
        .put("authenticationToken", tok)))

    private fun vtsMsg(type: String, data: JSONObject) = JSONObject()
        .put("apiName", "VTubeStudioPublicAPI")
        .put("apiVersion", "1.0")
        .put("requestID", UUID.randomUUID().toString())
        .put("messageType", type)
        .put("data", data)
        .toString()

    private fun send(text: String) = ws?.send(text)
}
