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
import android.provider.Settings
import android.view.HapticFeedbackConstants
import androidx.core.content.FileProvider
import com.google.zxing.integration.android.IntentIntegrator
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/** Hosts the Rust UI (android_main). The public methods are called from Rust. */
class MainActivity : NativeActivity() {
    /** An update waiting for "install unknown apps" to be allowed. */
    private var pendingApk: String? = null

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

    /** The camera, for a subscription QR code; the result goes to Native.scanned. */
    fun scanQr() = runOnUiThread {
        IntentIntegrator(this)
            .setDesiredBarcodeFormats(IntentIntegrator.QR_CODE)
            .setPrompt("Наведите камеру на QR-код подписки")
            .setBeepEnabled(false)
            .setOrientationLocked(false)
            .initiateScan()
    }

    fun copyText(text: String) = runOnUiThread {
        getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText("DUORAY", text))
    }

    fun openUrl(url: String) = runOnUiThread {
        runCatching { startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
    }

    /**
     * A downloaded update (verified by Rust) to the system installer. The
     * first time Android asks to allow installs from DUORAY; once allowed,
     * the install goes on from onResume.
     */
    fun installApk(path: String) = runOnUiThread {
        if (Build.VERSION.SDK_INT >= 26 && !packageManager.canRequestPackageInstalls()) {
            pendingApk = path
            runCatching {
                startActivity(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName")))
            }
            return@runOnUiThread
        }
        pendingApk = null
        val uri = FileProvider.getUriForFile(this, "$packageName.files", File(path))
        runCatching {
            startActivity(
                Intent(Intent.ACTION_VIEW)
                    .setDataAndType(uri, "application/vnd.android.package-archive")
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK),
            )
        }
    }

    override fun onResume() {
        super.onResume()
        val apk = pendingApk ?: return
        if (Build.VERSION.SDK_INT < 26 || packageManager.canRequestPackageInstalls()) installApk(apk) else pendingApk = null
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
        IntentIntegrator.parseActivityResult(requestCode, resultCode, data)?.contents?.let { Native.scanned(it) }
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
