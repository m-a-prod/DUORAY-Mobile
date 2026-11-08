package space.dualizm.duoray

import android.Manifest
import android.app.NativeActivity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.PackageManager
import android.location.LocationManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.Drawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
import android.view.HapticFeedbackConstants
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import androidx.core.content.FileProvider
import androidx.core.view.WindowInsetsControllerCompat
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/** Hosts the Rust UI (android_main). The public methods are called from Rust. */
class MainActivity : NativeActivity() {
    /** An update waiting for "install unknown apps" to be allowed. */
    private var pendingApk: String? = null

    /** The system Back goes to the window while a sheet or a page is open (Android 13+). */
    private var backCallback: Any? = null


    override fun onCreate(savedInstanceState: Bundle?) {
        Native.boot(this)
        NetworkWatcher.start(applicationContext)
        super.onCreate(savedInstanceState)
        preferHighRefreshRate()
        watchCaption()
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

    /** duoray:// links, the launcher shortcuts, and Rust asking for the VPN with no window. */
    private fun handleLink(intent: Intent?) {
        when (intent?.action) {
            ACTION_SCAN -> scanQr()
            ACTION_TOGGLE -> Native.toggle()
            ACTION_CONNECT -> startVpn()
        }
        val link = intent?.data?.toString() ?: return
        if (link.startsWith("duoray://")) Native.openLink(link)
    }

    /**
     * Rust says whether the window takes Back. At the top level the callback
     * is gone and the system plays its predictive "back to home" animation.
     */
    fun setBackEnabled(on: Boolean) = runOnUiThread {
        if (Build.VERSION.SDK_INT < 33) return@runOnUiThread
        val registered = backCallback as OnBackInvokedCallback?
        if (on && registered == null) {
            val callback = OnBackInvokedCallback { Native.back() }
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, callback)
            backCallback = callback
        } else if (!on && registered != null) {
            onBackInvokedDispatcher.unregisterOnBackInvokedCallback(registered)
            backCallback = null
        }
    }

    /**
     * The fastest mode the screen has at its resolution (120 Hz and the
     * like): with no request a NativeActivity's surface stays at 60 Hz. The
     * system's own limit (Smooth Display, battery saver) still applies.
     */
    private fun preferHighRefreshRate() {
        val display = (if (Build.VERSION.SDK_INT >= 30) display else @Suppress("DEPRECATION") windowManager.defaultDisplay) ?: return
        val current = display.mode
        val best = display.supportedModes
            .filter { it.physicalWidth == current.physicalWidth && it.physicalHeight == current.physicalHeight }
            .maxByOrNull { it.refreshRate } ?: return
        window.attributes = window.attributes.apply {
            preferredDisplayModeId = best.modeId
            preferredRefreshRate = best.refreshRate
        }
    }

    /** The caption bar last reported to Rust, in pixels. */
    private var captionTop = -1

    /**
     * A floating window's caption bar (freeform, the makers' floating
     * windows) lies over the window's top. Android reports it with the
     * window's insets; Slint's safe area leaves it out, so Rust adds it.
     */
    private fun watchCaption() {
        if (Build.VERSION.SDK_INT < 30) return
        val decor = window.decorView
        decor.viewTreeObserver.addOnGlobalLayoutListener {
            val px = decor.rootWindowInsets?.getInsets(android.view.WindowInsets.Type.captionBar())?.top ?: 0
            if (px != captionTop) {
                captionTop = px
                Native.captionTop(px)
            }
        }
    }

    /** Dark bar icons over a light theme, light ones over a dark theme. */
    fun setLightBars(light: Boolean) = runOnUiThread {
        val controller = WindowInsetsControllerCompat(window, window.decorView)
        controller.isAppearanceLightStatusBars = light
        controller.isAppearanceLightNavigationBars = light
    }

    /** "vpn" (always-on lives there), "location", or "app" (this app's permissions). */
    fun openSettings(which: String) = runOnUiThread {
        val intent = when (which) {
            "vpn" -> Intent(Settings.ACTION_VPN_SETTINGS)
            "location" -> Intent(Settings.ACTION_LOCATION_SOURCE_SETTINGS)
            else -> Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:$packageName"))
        }
        runCatching { startActivity(intent) }
    }

    /** «Виджеты» in the settings: the launcher's own dialog puts one on the home screen. */
    fun pinWidget(kind: String) = runOnUiThread {
        val provider = when (kind) {
            "status" -> StatusWidget::class.java
            "detail" -> DetailWidget::class.java
            "favorites" -> FavoritesWidget::class.java
            else -> ButtonWidget::class.java
        }
        val manager = getSystemService(android.appwidget.AppWidgetManager::class.java)
        if (manager?.isRequestPinAppWidgetSupported == true) {
            manager.requestPinAppWidget(android.content.ComponentName(this, provider), null, null)
        }
    }

    /**
     * Android 17 keeps apps off the local network without this permission:
     * devices on the Wi-Fi or the hotspot could not reach the shared proxy.
     */
    private fun localNetworkGranted(): Boolean =
        Build.VERSION.SDK_INT < 37 || checkSelfPermission(LOCAL_NETWORK) == PackageManager.PERMISSION_GRANTED

    fun requestLocalNetwork() = runOnUiThread {
        if (!localNetworkGranted()) requestPermissions(arrayOf(LOCAL_NETWORK), REQUEST_LOCAL_NETWORK)
    }

    /** Whether Android may stop DUORAY in the background to save battery. */
    private fun reportBattery() {
        val power = getSystemService(PowerManager::class.java)
        Native.batteryUnrestricted(power?.isIgnoringBatteryOptimizations(packageName) == true)
    }

    /** Android's own dialog: let DUORAY run in the background (the VPN is not stopped to save battery). */
    fun requestBackground() = runOnUiThread {
        runCatching {
            startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")))
        }.onFailure {
            runCatching { startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)) }
        }
    }

    /** Tells Rust what keeps Wi-Fi names hidden, if anything (the routing page explains it). */
    private fun reportWifiAccess() {
        val fine = checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED
        val coarse = checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED
        val locationOn = Build.VERSION.SDK_INT < 28 || getSystemService(LocationManager::class.java)?.isLocationEnabled == true
        Native.wifiAccess(
            when {
                fine && locationOn -> 3
                fine -> 2
                coarse -> 1
                else -> 0
            },
        )
    }

    /** Asks for the VPN the first time, then starts it. */
    fun startVpn() = runOnUiThread {
        val consent = DuorayVpnService.needsConsent(this)
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
        startActivityForResult(Intent(this, ScanActivity::class.java), REQUEST_SCAN)
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
        reportWifiAccess()
        reportBattery()
        Native.localNetwork(localNetworkGranted())
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
        if (requestCode == REQUEST_SCAN && resultCode == RESULT_OK) {
            data?.getStringExtra(ScanActivity.EXTRA_TEXT)?.let { Native.scanned(it) }
        }
        if (requestCode == REQUEST_VPN) {
            if (resultCode == RESULT_OK) DuorayVpnService.start(this) else Native.vpnDenied()
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQUEST_LOCAL_NETWORK) Native.localNetwork(localNetworkGranted())
        if (requestCode == REQUEST_LOCATION) {
            reportWifiAccess()
            NetworkWatcher.restart(applicationContext)
        }
    }

    companion object {
        const val ACTION_SCAN = "space.dualizm.duoray.SCAN"
        const val ACTION_TOGGLE = "space.dualizm.duoray.TOGGLE"
        const val ACTION_CONNECT = "space.dualizm.duoray.CONNECT"
        private const val REQUEST_VPN = 1
        private const val REQUEST_LOCATION = 2
        private const val REQUEST_NOTIFICATIONS = 3
        private const val REQUEST_SCAN = 4
        private const val REQUEST_LOCAL_NETWORK = 5
        private const val LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"
    }
}
