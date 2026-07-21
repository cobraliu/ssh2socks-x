package com.ssh2socks.app

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import androidx.core.app.NotificationCompat
import mobile.Callback
import mobile.Mobile
import mobile.Protector
import mobile.Tunnel
import org.json.JSONObject

/**
 * Foreground VpnService that runs the Go engine and pipes the tun device to the
 * in-process SOCKS5 proxy via tun2socks. Implements the Go Callback (events) and
 * Protector (socket protection) interfaces directly.
 */
class SshVpnService : VpnService(), Callback, Protector {

    companion object {
        const val ACTION_START = "com.ssh2socks.app.START"
        const val ACTION_STOP = "com.ssh2socks.app.STOP"
        const val EXTRA_CONFIG = "config"
        private const val CHANNEL_ID = "ssh2socks_vpn"
        private const val NOTI_ID = 1
    }

    private var tunnel: Tunnel? = null
    private var vpnInterface: ParcelFileDescriptor? = null
    private var mtu = 1500

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                teardown("已停止")
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_START -> {
                val cfg = intent.getStringExtra(EXTRA_CONFIG) ?: ""
                startForeground(NOTI_ID, notification("正在连接…"))
                bringUp(cfg)
            }
        }
        return START_STICKY
    }

    private fun bringUp(cfgJson: String) {
        try {
            val j = JSONObject(cfgJson)
            mtu = j.optInt("mtu", 1500)
            vpnInterface = buildInterface(j)
            tunnel = Mobile.newTunnel(cfgJson, this, this)
            tunnel?.start()
            // Engine runs asynchronously; StartTun is triggered from onState("connected").
        } catch (e: Exception) {
            Events.log("启动失败: ${e.message}")
            Events.state("error", e.message ?: "")
            teardown("启动失败")
            stopSelf()
        }
    }

    private fun buildInterface(j: JSONObject): ParcelFileDescriptor {
        val b = Builder()
            .setSession("ssh2socks")
            .setMtu(mtu)
            .addAddress("10.0.0.2", 32)
            .addAddress("fd00::2", 128)
            .addDnsServer(j.optString("dns", "1.1.1.1"))
            .addRoute("0.0.0.0", 0)
            .addRoute("::", 0)

        val perApp = j.optString("routingMode") == "perApp"
        if (perApp) {
            val apps = j.optJSONArray("allowedApps")
            var added = 0
            if (apps != null) {
                for (i in 0 until apps.length()) {
                    try {
                        b.addAllowedApplication(apps.getString(i)); added++
                    } catch (_: Exception) { /* uninstalled package */ }
                }
            }
            // If nothing valid was selected, fall back to routing nothing but us.
            if (added == 0) b.addAllowedApplication(packageName)
        } else {
            // Global: never capture our own traffic (belt-and-suspenders with protect()).
            try {
                b.addDisallowedApplication(packageName)
            } catch (_: Exception) {
            }
        }
        return b.establish() ?: throw IllegalStateException("无法建立 VPN 接口")
    }

    private fun teardown(note: String) {
        try {
            tunnel?.stop()
        } catch (_: Exception) {
        }
        tunnel = null
        try {
            vpnInterface?.close()
        } catch (_: Exception) {
        }
        vpnInterface = null
        Events.state("stopped", note)
        stopForeground(STOP_FOREGROUND_REMOVE)
    }

    override fun onDestroy() {
        teardown("服务销毁")
        super.onDestroy()
    }

    // ---- mobile.Protector ----
    // gomobile maps Go `int` → Java `long`; delegate to VpnService.protect(int).
    override fun protect(fd: Long): Boolean = protect(fd.toInt())

    // ---- mobile.Callback ----
    override fun onState(state: String, message: String) {
        Events.state(state, message)
        updateNotification(stateLabel(state))
        if (state == "connected") {
            val fd = vpnInterface?.fd ?: return
            try {
                tunnel?.startTun(fd.toLong(), mtu.toLong())
                Events.log("tun2socks 已接入")
            } catch (e: Exception) {
                Events.log("接入 tun 失败: ${e.message}")
            }
        }
    }

    override fun onLog(line: String) = Events.log(line)

    // gomobile maps Go `int` → Java `long`.
    override fun onProbe(ok: Boolean, latencyMS: Long, message: String) =
        Events.probe(ok, latencyMS.toInt(), message)

    // ---- notification ----
    private fun stateLabel(s: String) = when (s) {
        "connected" -> "已连接"
        "connecting" -> "连接中…"
        "error" -> "错误"
        else -> "已停止"
    }

    private fun notification(text: String): android.app.Notification {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val ch = NotificationChannel(CHANNEL_ID, "VPN 状态", NotificationManager.IMPORTANCE_LOW)
            getSystemService(NotificationManager::class.java).createNotificationChannel(ch)
        }
        val tap = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("ssh2socks")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_lock_lock)
            .setOngoing(true)
            .setContentIntent(tap)
            .build()
    }

    private fun updateNotification(text: String) {
        getSystemService(NotificationManager::class.java).notify(NOTI_ID, notification(text))
    }
}
