package space.dualizm.duoray

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.net.VpnService
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import org.json.JSONObject
import java.io.File

/**
 * Opens the TUN and hands it to Rust, which runs xray on it (xray-core's
 * own tun inbound reads the descriptor). The app stays outside its VPN, so
 * xray's connections to the servers go out directly.
 */
class DuorayVpnService : VpnService() {
    private var tun: ParcelFileDescriptor? = null

    // «Прокси» (no TUN) handed to Rust and running.
    private var proxyUp = false

    override fun onCreate() {
        super.onCreate()
        instance = this
        Native.boot(this)
        NetworkWatcher.start(applicationContext)
        // With the screen off (Always On Display too) nobody sees the speeds:
        // Rust stops reading them and updating the notification.
        registerReceiver(screen, IntentFilter().apply {
            addAction(Intent.ACTION_SCREEN_ON)
            addAction(Intent.ACTION_SCREEN_OFF)
        })
        Native.screenOn(getSystemService(PowerManager::class.java).isInteractive)
    }

    private val screen = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            Native.screenOn(intent.action == Intent.ACTION_SCREEN_ON)
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            shutdown()
            return START_NOT_STICKY
        }
        showNotification()
        if (tun == null && tunnelMode(this) == "proxy") {
            // xray only listens on its proxy ports: no TUN, no VPN.
            if (proxyUp) return START_STICKY
            proxyUp = true
            if (!Native.tunUp(this, NO_TUN)) {
                shutdown()
                return START_NOT_STICKY
            }
            return START_STICKY
        }
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
            .addDnsServer(DNS)
        for ((address, prefix) in routes(excludeLan())) builder.addRoute(address, prefix)
        applyAppRules(builder)
        if (Build.VERSION.SDK_INT >= 29) builder.setMetered(false)
        builder.establish()
    }.getOrNull()

    /**
     * The local network stays off the VPN with «Локальная сеть» напрямую or
     * «Доступ через хотспот» on: the router, printers and the devices using
     * the phone as a proxy are reached directly, and answers to them do not
     * vanish into the tunnel (Android routes them by the VPN's uid rules).
     */
    private fun excludeLan(): Boolean {
        val share = runCatching {
            JSONObject(File(filesDir, "mobile.json").readText()).optJSONObject("share")?.optBoolean("enabled") == true
        }.getOrDefault(false) && tunnelMode(this) != "vpn"
        val lan = runCatching {
            JSONObject(File(filesDir, "store.json").readText())
                .optJSONObject("settings")?.optJSONObject("routing")?.optJSONObject("simple")?.optBoolean("lan_direct") == true
        }.getOrDefault(false)
        return share || lan
    }

    /**
     * Per-app rules from mobile.json (src/settings.rs): every app, every app
     * but the listed ones, or only the listed ones. The app itself always
     * stays outside, so xray reaches the servers directly.
     */
    private fun applyAppRules(builder: Builder) {
        val apps = runCatching { JSONObject(File(filesDir, "mobile.json").readText()).optJSONObject("apps") }.getOrNull()
        val mode = apps?.optString("mode") ?: "all"
        // Each mode keeps its own list (src/settings.rs); "packages" is the old shared one.
        fun list(name: String) = apps?.optJSONArray(name)?.let { l -> (0 until l.length()).map { l.getString(it) } }.orEmpty()
        val packages = list(mode).ifEmpty { list("packages") }
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
            // A start already under way may still be handing the old one to
            // xray: closed a little later, not under its feet.
            Handler(Looper.getMainLooper()).postDelayed({ old.close() }, 3000)
        }
    }

    /** Called from Rust when xray stopped on its own. */
    fun stopFromNative() {
        Handler(Looper.getMainLooper()).post { shutdown() }
    }

    private fun shutdown() {
        // Rust first: it must not hand a closed descriptor to a new xray.
        val old = tun
        tun = null
        proxyUp = false
        Native.tunDown()
        old?.close()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onRevoke() = shutdown()

    override fun onDestroy() {
        runCatching { unregisterReceiver(screen) }
        if (instance === this) instance = null
        val old = tun
        if (old != null || proxyUp) {
            tun = null
            proxyUp = false
            Native.tunDown()
            old?.close()
        }
        super.onDestroy()
    }

    private fun showNotification() {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.channel_vpn), NotificationManager.IMPORTANCE_LOW),
        )
        // «Свернуть уведомление»: Android needs one while the VPN runs; this
        // channel keeps it folded at the bottom of the shade, no status bar icon.
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_QUIET, getString(R.string.channel_quiet), NotificationManager.IMPORTANCE_MIN),
        )
        val notification = buildNotification()
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    /** Rust described the connection anew (Native.onStatus). */
    fun refreshNotification() {
        if (tun == null) return
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, buildNotification())
    }

    /**
     * The connection in the shade: the exit country, the server and its ping,
     * the speeds and how long it has been up. On Android 16+ it asks to be a
     * Live Update: a chip with the exit country in the status bar, a card on
     * the lock screen.
     */
    private fun buildNotification(): android.app.Notification {
        val status = ConnectionStatus.current
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(
            this,
            1,
            Intent(this, DuorayVpnService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val title = status.title.ifEmpty { getString(R.string.notification_connected) }
        val builder = NotificationCompat.Builder(this, if (status.quiet) CHANNEL_QUIET else CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_duoray)
            .setContentTitle(title)
            .setContentText(status.text.ifEmpty { null })
            .setSubText(status.speeds.ifEmpty { null })
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .addAction(R.drawable.ic_stat_duoray, getString(R.string.action_disconnect), stop)
        if (status.state == 2 && status.sinceMs > 0) {
            builder.setWhen(status.sinceMs).setShowWhen(true).setUsesChronometer(true)
        } else {
            builder.setShowWhen(false)
        }
        if (status.state == 2 && status.promote && !status.quiet) {
            builder.setRequestPromotedOngoing(true)
            builder.setShortCriticalText(status.chip.ifEmpty { "VPN" })
        }
        return builder.build()
    }

    companion object {
        /** Private and link-local ranges, kept off the VPN by excludeLan. */
        private val LAN = listOf("10.0.0.0" to 8, "172.16.0.0" to 12, "192.168.0.0" to 16, "169.254.0.0" to 16)

        /**
         * IPv4 routes into the TUN: everything, or everything but LAN as the
         * smallest set of prefixes (Builder.excludeRoute needs Android 13).
         */
        fun routes(excludeLan: Boolean): List<Pair<String, Int>> {
            if (!excludeLan) return listOf("0.0.0.0" to 0)
            fun ip(s: String) = s.split('.').fold(0L) { acc, part -> (acc shl 8) or part.toLong() }
            val holes = LAN.map { (a, p) -> ip(a) to p }
            fun inside(start: Long, prefix: Int, hole: Pair<Long, Int>) =
                hole.second <= prefix && (start ushr (32 - hole.second)) == (hole.first ushr (32 - hole.second))
            fun overlaps(start: Long, prefix: Int, hole: Pair<Long, Int>) =
                inside(start, prefix, hole) || (prefix <= hole.second && (hole.first ushr (32 - prefix).coerceAtMost(32)) == (start ushr (32 - prefix).coerceAtMost(32)))
            val out = mutableListOf<Pair<String, Int>>()
            fun walk(start: Long, prefix: Int) {
                when {
                    holes.any { inside(start, prefix, it) } -> return
                    holes.none { overlaps(start, prefix, it) } ->
                        out += listOf(24, 16, 8, 0).joinToString(".") { ((start ushr it) and 0xff).toString() } to prefix
                    else -> {
                        walk(start, prefix + 1)
                        walk(start or (1L shl (31 - prefix)), prefix + 1)
                    }
                }
            }
            walk(0, 0)
            return out
        }

        /** The running service, for Rust's status updates. */
        var instance: DuorayVpnService? = null
            private set

        private const val ACTION_STOP = "space.dualizm.duoray.STOP"
        private const val CHANNEL = "vpn"
        private const val CHANNEL_QUIET = "vpn_quiet"
        private const val NOTIFICATION_ID = 1

        // Must match TUN_MTU in src/service.rs.
        private const val MTU = 1500
        private const val ADDRESS = "172.19.0.1"

        // Queries go through the tunnel like any other traffic.
        private const val DNS = "1.1.1.1"

        // src/service.rs NO_TUN: the service runs, xray has no TUN.
        private const val NO_TUN = -2

        /**
         * «Режим работы» from mobile.json (src/settings.rs TunnelMode): "vpn",
         * "vpn_proxy" or "proxy". Files from before it ran the hotspot proxy
         * next to the VPN.
         */
        fun tunnelMode(context: Context): String = runCatching {
            val json = JSONObject(File(context.filesDir, "mobile.json").readText())
            val shared = json.optJSONObject("share")?.optBoolean("enabled") == true
            json.optString("tunnel", if (shared) "vpn_proxy" else "vpn")
        }.getOrDefault("vpn")

        /** The proxy alone needs no VPN permission. */
        fun needsConsent(context: Context): Intent? = if (tunnelMode(context) == "proxy") null else VpnService.prepare(context)

        fun start(context: Context) {
            context.startForegroundService(Intent(context, DuorayVpnService::class.java))
        }

        fun stop(context: Context) {
            runCatching { context.startService(Intent(context, DuorayVpnService::class.java).setAction(ACTION_STOP)) }
        }
    }
}
