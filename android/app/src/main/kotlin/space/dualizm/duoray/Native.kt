package space.dualizm.duoray

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.service.quicksettings.TileService
import java.io.File

/** The Rust side, libduoray_mobile.so (src/platform/android.rs). */
object Native {
    init {
        System.loadLibrary("duoray_mobile")
    }

    /** The application, for the calls Rust makes with no window open. */
    private var app: Context? = null

    /** Puts xray's geo databases in place and tells Rust where things are. Safe to repeat. */
    fun boot(context: Context) {
        app = context.applicationContext
        copyGeoData(context)
        init(
            context.filesDir.absolutePath,
            context.applicationInfo.nativeLibraryDir,
            Settings.Secure.getString(context.contentResolver, Settings.Secure.ANDROID_ID) ?: "",
            Build.VERSION.RELEASE ?: "",
            "${Build.MANUFACTURER} ${Build.MODEL}",
        )
    }

    /** geoip.dat and geosite.dat from the APK, copied once per install or update. */
    private fun copyGeoData(context: Context) {
        val dir = File(context.filesDir, "xray").apply { mkdirs() }
        val stamp = File(dir, ".installed")
        val installed = context.packageManager.getPackageInfo(context.packageName, 0).lastUpdateTime.toString()
        if (stamp.exists() && stamp.readText() == installed) return
        for (name in context.assets.list("xray").orEmpty()) {
            context.assets.open("xray/$name").use { input ->
                File(dir, name).outputStream().use { input.copyTo(it) }
            }
        }
        stamp.writeText(installed)
    }

    /**
     * Rust's connection state (src/status.rs): the notification and the
     * Quick Settings tile show it.
     */
    @JvmStatic
    fun onStatus(packed: String) {
        val status = ConnectionStatus.parse(packed)
        Handler(Looper.getMainLooper()).post {
            ConnectionStatus.current = status
            DuorayVpnService.instance?.refreshNotification()
            DuorayTileService.listening?.show()
            app?.let { Widgets.statusChanged(it) }
            app?.let { context ->
                runCatching { TileService.requestListeningState(context, ComponentName(context, DuorayTileService::class.java)) }
            }
        }
    }

    /**
     * Rust wants the VPN with no window open (the tile, a shortcut). When the
     * VPN is allowed already the service starts right away; otherwise the
     * app opens and asks.
     */
    @JvmStatic
    fun startVpnInBackground(@Suppress("UNUSED_PARAMETER") unused: String) {
        val context = app ?: return
        Handler(Looper.getMainLooper()).post {
            if (DuorayVpnService.needsConsent(context) == null) {
                DuorayVpnService.start(context)
            } else {
                context.startActivity(
                    Intent(context, MainActivity::class.java)
                        .setAction(MainActivity.ACTION_CONNECT)
                        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                )
            }
        }
    }

    @JvmStatic external fun init(filesDir: String, nativeLibraryDir: String, deviceId: String, osVersion: String, model: String)

    /** The TUN is open; false when Rust has nothing to run on it. */
    @JvmStatic external fun tunUp(service: DuorayVpnService, fd: Int): Boolean

    @JvmStatic external fun tunDown()

    @JvmStatic external fun vpnDenied()

    /** A duoray://add/... link the app was opened with. */
    @JvmStatic external fun openLink(link: String)

    /** What the QR scanner read. */
    @JvmStatic external fun scanned(text: String)

    /** kind: 0 offline, 1 Wi-Fi, 2 mobile, 3 other. */
    @JvmStatic external fun networkChanged(kind: Int, ssid: String?)

    /** 0 no location access, 1 approximate only, 2 precise but location off, 3 Wi-Fi names readable. */
    @JvmStatic external fun wifiAccess(code: Int)

    /** Whether Android leaves DUORAY alone in the background (battery optimization off). */
    @JvmStatic external fun batteryUnrestricted(unrestricted: Boolean)

    /** Whether DUORAY may talk to the local network (Android 17 asks the user). */
    @JvmStatic external fun localNetwork(granted: Boolean)

    /** The caption bar of a floating window over the top of the window, in pixels. */
    @JvmStatic external fun captionTop(px: Int)

    /** The system Back, while the window takes it. */
    @JvmStatic external fun back()

    /** Connect to the last server, or disconnect. */
    @JvmStatic external fun toggle()

    /** A pinned server tapped in a home screen widget: connect or switch to it. */
    @JvmStatic external fun connectTo(key: String)

    /** Rust rewrote files/widgets.json (the theme or the pinned servers). */
    @JvmStatic
    fun refreshWidgets(@Suppress("UNUSED_PARAMETER") unused: String) {
        app?.let { Widgets.refresh(it) }
    }
}

/** The connection as Rust last described it (src/status.rs, Notice::pack). */
data class ConnectionStatus(
    /** 0 off, 1 connecting, 2 on, 3 failed. */
    val state: Int,
    val title: String,
    val text: String,
    val speeds: String,
    /** The exit country's code, for the Live Update chip. */
    val chip: String,
    /** When the connection came up, Unix ms; 0 unknown. */
    val sinceMs: Long,
    /** Ask to be a Live Update (the setting «Закреплять в шторке»). */
    val promote: Boolean,
    /** «Свернуть уведомление»: the minimal channel. */
    val quiet: Boolean,
    /** The server's key (src/servers.rs), for the favorites widget: last, it holds U+001F itself. */
    val key: String = "",
) {
    val active: Boolean get() = state == 1 || state == 2

    companion object {
        var current = ConnectionStatus(0, "Не подключено", "", "", "", 0, true, false)

        fun parse(packed: String): ConnectionStatus {
            val f = packed.split('\u001f')
            fun at(i: Int) = f.getOrElse(i) { "" }
            return ConnectionStatus(at(0).toIntOrNull() ?: 0, at(1), at(2), at(3), at(4), at(5).toLongOrNull() ?: 0, at(6) != "0", at(7) == "1", f.drop(8).joinToString("\u001f"))
        }
    }
}
