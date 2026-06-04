package com.lakomoor.rbridger

import android.content.Context
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
    context: Context,
    private val onState: (ConnState) -> Unit,
    private val onHint: (String) -> Unit = {},
) {
    private val prefs = context.getSharedPreferences("rbridger_vts", Context.MODE_PRIVATE)
    private val http = OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.SECONDS)   // no read timeout — VTS can be slow
        .build()

    private var ws: WebSocket? = null
    private var token: String? = prefs.getString("auth_token", null)  // survives restarts
    private val mainHandler = Handler(Looper.getMainLooper())
    private var retryRunnable: Runnable? = null

    var state = ConnState.DISCONNECTED
        private set(v) { field = v; mainHandler.post { onState(v) } }

    fun connect(host: String, port: Int = 8001) {
        if (state != ConnState.DISCONNECTED && state != ConnState.ERROR) return
        cancelRetry()
        state = ConnState.CONNECTING
        ws = http.newWebSocket(Request.Builder().url("ws://$host:$port").build(), Listener())
    }

    fun disconnect() {
        cancelRetry()
        ws?.close(1000, null)
        ws = null
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
            if (token != null) {
                mainHandler.post { onHint("Authenticating with saved token…") }
                authenticate(token!!)
            } else {
                mainHandler.post { onHint("Requesting approval from VTube Studio…") }
                requestToken()
            }
        }

        override fun onMessage(ws: WebSocket, text: String) {
            val json = runCatching { JSONObject(text) }.getOrNull() ?: return
            when (json.optString("messageType")) {
                "AuthenticationTokenResponse" -> handleTokenResponse(json)
                "AuthenticationResponse"      -> handleAuthResponse(json)
                "APIError"                    -> handleApiError(json)
            }
        }

        override fun onFailure(ws: WebSocket, t: Throwable, r: Response?) {
            cancelRetry()
            mainHandler.post { onHint(t.message ?: "Connection failed") }
            state = ConnState.ERROR
        }

        override fun onClosed(ws: WebSocket, code: Int, reason: String) {
            cancelRetry()
            state = ConnState.DISCONNECTED
        }
    }

    private fun handleTokenResponse(json: JSONObject) {
        val data = json.optJSONObject("data")
        val tok = data?.optString("authenticationToken", null)
        if (tok.isNullOrEmpty()) {
            // User denied the request
            mainHandler.post { onHint("Plugin access denied in VTube Studio") }
            state = ConnState.ERROR
            return
        }
        token = tok
        prefs.edit().putString("auth_token", tok).apply()
        // Token arrived after user clicked Allow — authenticate immediately
        state = ConnState.AWAITING_APPROVAL
        mainHandler.post { onHint("Approved! Authenticating…") }
        authenticate(tok)
    }

    private fun handleAuthResponse(json: JSONObject) {
        val authenticated = json.optJSONObject("data")?.optBoolean("authenticated", false) ?: false
        val reason = json.optJSONObject("data")?.optString("reason", "") ?: ""
        if (authenticated) {
            cancelRetry()
            mainHandler.post { onHint("") }
            state = ConnState.CONNECTED
        } else {
            // Token might be expired or not yet approved — retry in 1s
            mainHandler.post { onHint(if (reason.isNotEmpty()) reason else "Waiting for approval…") }
            scheduleRetry(1000L) {
                if (token != null) authenticate(token!!)
                else { mainHandler.post { onHint("No token — reconnect") }; state = ConnState.ERROR }
            }
        }
    }

    private fun handleApiError(json: JSONObject) {
        val msg = json.optJSONObject("data")?.optString("message", "API error") ?: "API error"
        val errId = json.optJSONObject("data")?.optInt("errorID", -1) ?: -1
        // errorID 50: token invalid/expired — clear and re-request
        if (errId == 50) {
            prefs.edit().remove("auth_token").apply()
            token = null
            mainHandler.post { onHint("Token expired, requesting new approval…") }
            state = ConnState.AUTHENTICATING
            requestToken()
        } else {
            mainHandler.post { onHint(msg) }
            state = ConnState.ERROR
        }
    }

    private fun scheduleRetry(delayMs: Long, action: () -> Unit) {
        cancelRetry()
        val r = Runnable {
            if (state != ConnState.CONNECTED && state != ConnState.DISCONNECTED) action()
        }
        retryRunnable = r
        mainHandler.postDelayed(r, delayMs)
    }

    private fun cancelRetry() {
        retryRunnable?.let { mainHandler.removeCallbacks(it) }
        retryRunnable = null
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
