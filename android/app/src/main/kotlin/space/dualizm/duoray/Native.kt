package space.dualizm.duoray

import android.content.Context
import android.os.Build
import android.provider.Settings
import java.io.File

/** The Rust side, libduoray_mobile.so (src/platform/android.rs). */
object Native {
    init {
        System.loadLibrary("duoray_mobile")
    }

    /** Puts xray's geo databases in place and tells Rust where things are. Safe to repeat. */
    fun boot(context: Context) {
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

    @JvmStatic external fun init(filesDir: String, nativeLibraryDir: String, deviceId: String, osVersion: String, model: String)

    /** The TUN is open; false when Rust has nothing to run on it. */
    @JvmStatic external fun tunUp(service: DuorayVpnService, fd: Int): Boolean

    @JvmStatic external fun tunDown()

    @JvmStatic external fun vpnDenied()

    /** kind: 0 offline, 1 Wi-Fi, 2 mobile, 3 other. */
    @JvmStatic external fun networkChanged(kind: Int, ssid: String?)
}
