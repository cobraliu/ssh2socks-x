package com.ssh2socks.app

import android.app.Activity
import android.content.Intent
import android.net.VpnService
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodChannel
import mobile.Mobile

class MainActivity : FlutterActivity() {
    private val methodChannel = "ssh2socks/vpn"
    private val eventChannel = "ssh2socks/events"
    private val reqVpn = 7001

    private var pendingPrepare: MethodChannel.Result? = null

    override fun configureFlutterEngine(engine: FlutterEngine) {
        super.configureFlutterEngine(engine)
        val messenger = engine.dartExecutor.binaryMessenger

        EventChannel(messenger, eventChannel).setStreamHandler(Events)

        MethodChannel(messenger, methodChannel).setMethodCallHandler { call, result ->
            when (call.method) {
                "prepareVpn" -> prepareVpn(result)
                "start" -> {
                    val cfg = call.argument<String>("config") ?: ""
                    val i = Intent(this, SshVpnService::class.java)
                        .setAction(SshVpnService.ACTION_START)
                        .putExtra(SshVpnService.EXTRA_CONFIG, cfg)
                    startForegroundService(i)
                    result.success(null)
                }
                "stop" -> {
                    startService(Intent(this, SshVpnService::class.java)
                        .setAction(SshVpnService.ACTION_STOP))
                    result.success(null)
                }
                "state" -> result.success(Events.lastState)
                "listHosts" -> {
                    val text = call.argument<String>("configText") ?: ""
                    Thread {
                        try {
                            val json = Mobile.listHosts(text)
                            runOnUiThread { result.success(json) }
                        } catch (e: Exception) {
                            runOnUiThread { result.error("PARSE", e.message, null) }
                        }
                    }.start()
                }
                else -> result.notImplemented()
            }
        }
    }

    private fun prepareVpn(result: MethodChannel.Result) {
        val intent = VpnService.prepare(this)
        if (intent == null) {
            result.success(true)
            return
        }
        pendingPrepare = result
        startActivityForResult(intent, reqVpn)
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == reqVpn) {
            pendingPrepare?.success(resultCode == Activity.RESULT_OK)
            pendingPrepare = null
        }
    }
}
