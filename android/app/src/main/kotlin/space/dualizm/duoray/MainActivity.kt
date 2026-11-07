package space.dualizm.duoray

import android.Manifest
import android.app.NativeActivity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.Drawable
import android.net.Uri
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.view.HapticFeedbackConstants
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/** Hosts the Rust UI (android_main). The public methods are called from Rust. */
class MainActivity : NativeActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        Native.boot(this)
        NetworkWatcher.start(applicationContext)
        super.onCreate(savedInstanceState)
        handleLink(intent)
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQUEST_NOTIFICATIONS)
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleLink(intent)
    }

    private fun handleLink(intent: Intent?) {
        val link = intent?.data?.toString() ?: return
        if (link.startsWith("duoray://")) Native.openLink(link)
    }

    /** Asks for the VPN the first time, then starts it. */
    fun startVpn() = runOnUiThread {
        val consent = VpnService.prepare(this)
        if (consent == null) DuorayVpnService.start(this) else startActivityForResult(consent, REQUEST_VPN)
    }

    fun stopVpn() = runOnUiThread { DuorayVpnService.stop(this) }

    fun requestWifiNames() = runOnUiThread {
        requestPermissions(
            arrayOf(Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION),
            REQUEST_LOCATION,
        )
    }

    fun haptic() = runOnUiThread {
        window.decorView.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
    }

    fun copyText(text: String) = runOnUiThread {
        getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText("DUORAY", text))
    }

    fun openUrl(url: String) = runOnUiThread {
        runCatching { startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
    }

    /** Launchable apps into files/apps.json, icons as PNGs in files/icons. Runs on the caller's thread. */
    fun listApps() {
        val pm = packageManager
        val dir = File(filesDir, "icons").apply { mkdirs() }
        val apps = JSONArray()
        val seen = HashSet<String>()
        val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        for (info in pm.queryIntentActivities(launcher, 0)) {
            val pkg = info.activityInfo.packageName
            if (pkg == packageName || !seen.add(pkg)) continue
            val icon = File(dir, "$pkg.png")
            if (!icon.exists()) runCatching { savePng(info.loadIcon(pm), icon) }
            apps.put(
                JSONObject()
                    .put("package", pkg)
                    .put("label", info.loadLabel(pm).toString())
                    .put("icon", if (icon.exists()) icon.absolutePath else ""),
            )
        }
        File(filesDir, "apps.json").writeText(apps.toString())
    }

    private fun savePng(drawable: Drawable, file: File) {
        val size = 96
        val bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888)
        drawable.setBounds(0, 0, size, size)
        drawable.draw(Canvas(bitmap))
        file.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQUEST_VPN) {
            if (resultCode == RESULT_OK) DuorayVpnService.start(this) else Native.vpnDenied()
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQUEST_LOCATION) NetworkWatcher.restart(applicationContext)
    }

    private companion object {
        const val REQUEST_VPN = 1
        const val REQUEST_LOCATION = 2
        const val REQUEST_NOTIFICATIONS = 3
    }
}
