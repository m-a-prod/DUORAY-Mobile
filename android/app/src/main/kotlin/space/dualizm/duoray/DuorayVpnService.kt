package space.dualizm.duoray

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.net.VpnService
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import org.json.JSONObject
import java.io.File

/**
 * Opens the TUN and hands it to Rust, which runs xray on it (xray-core's
 * own tun inbound reads the descriptor). The app stays outside its VPN, so
 * xray's connections to the servers go out directly.
 */
class DuorayVpnService : VpnService() {
    private var tun: ParcelFileDescriptor? = null

    override fun onCreate() {
        super.onCreate()
        Native.boot(this)
        NetworkWatcher.start(applicationContext)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            shutdown()
            return START_NOT_STICKY
        }
        showNotification()
        val fd = tun ?: establish()
        if (fd == null) {
            // Not allowed to open a VPN (the permission was taken back).
            Native.vpnDenied()
            shutdown()
            return START_NOT_STICKY
        }
        tun = fd
        if (!Native.tunUp(this, fd.fd)) {
            shutdown()
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    private fun establish(): ParcelFileDescriptor? = runCatching {
        val builder = Builder()
            .setSession(getString(R.string.app_name))
            .setMtu(MTU)
            .addAddress(ADDRESS, 30)
            .addRoute("0.0.0.0", 0)
            .addDnsServer(DNS)
        applyAppRules(builder)
        if (Build.VERSION.SDK_INT >= 29) builder.setMetered(false)
        builder.establish()
    }.getOrNull()

    /**
     * Per-app rules from mobile.json (src/settings.rs): every app, every app
     * but the listed ones, or only the listed ones. The app itself always
     * stays outside, so xray reaches the servers directly.
     */
    private fun applyAppRules(builder: Builder) {
        val apps = runCatching { JSONObject(File(filesDir, "mobile.json").readText()).optJSONObject("apps") }.getOrNull()
        val mode = apps?.optString("mode") ?: "all"
        val packages = apps?.optJSONArray("packages")?.let { list -> (0 until list.length()).map { list.getString(it) } }.orEmpty()
        if (mode == "only") {
            val added = packages.count { pkg -> pkg != packageName && runCatching { builder.addAllowedApplication(pkg) }.isSuccess }
            if (added > 0) return
        }
        builder.addDisallowedApplication(packageName)
        if (mode == "bypass") packages.forEach { pkg -> runCatching { builder.addDisallowedApplication(pkg) } }
    }

    /** Called from Rust when the per-app rules changed: a new TUN, then the old one goes. */
    fun rebuildFromNative() {
        Handler(Looper.getMainLooper()).post {
            val old = tun ?: return@post
            val fresh = establish() ?: return@post
            tun = fresh
            Native.tunUp(this, fresh.fd)
            old.close()
        }
    }

    /** Called from Rust when xray stopped on its own. */
    fun stopFromNative() {
        Handler(Looper.getMainLooper()).post { shutdown() }
    }

    private fun shutdown() {
        tun?.close()
        tun = null
        Native.tunDown()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onRevoke() = shutdown()

    override fun onDestroy() {
        if (tun != null) {
            tun?.close()
            tun = null
            Native.tunDown()
        }
        super.onDestroy()
    }

    private fun showNotification() {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.channel_vpn), NotificationManager.IMPORTANCE_LOW),
        )
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(
            this,
            1,
            Intent(this, DuorayVpnService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_duoray)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(getString(R.string.notification_connected))
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(this, R.drawable.ic_stat_duoray),
                    getString(R.string.action_disconnect),
                    stop,
                ).build(),
            )
            .build()
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    companion object {
        private const val ACTION_STOP = "space.dualizm.duoray.STOP"
        private const val CHANNEL = "vpn"
        private const val NOTIFICATION_ID = 1

        // Must match TUN_MTU in src/service.rs.
        private const val MTU = 1500
        private const val ADDRESS = "172.19.0.1"

        // Queries go through the tunnel like any other traffic.
        private const val DNS = "1.1.1.1"

        fun start(context: Context) {
            context.startForegroundService(Intent(context, DuorayVpnService::class.java))
        }

        fun stop(context: Context) {
            runCatching { context.startService(Intent(context, DuorayVpnService::class.java).setAction(ACTION_STOP)) }
        }
    }
}
