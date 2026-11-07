package space.dualizm.duoray

import android.annotation.TargetApi
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.wifi.WifiInfo
import android.os.Build

/**
 * Tells Rust which network the phone uses: Wi-Fi (with its name when the
 * app may read it), mobile data, another one, or none. The app is outside
 * its own VPN, so its default network is the real one even while connected.
 */
object NetworkWatcher {
    private var callback: ConnectivityManager.NetworkCallback? = null

    fun start(context: Context) {
        if (callback == null) register(context)
    }

    /** After the location permission changed: Wi-Fi names may be readable now. */
    fun restart(context: Context) {
        val manager = context.getSystemService(ConnectivityManager::class.java)
        callback?.let { runCatching { manager.unregisterNetworkCallback(it) } }
        callback = null
        register(context)
    }

    private fun register(context: Context) {
        val manager = context.getSystemService(ConnectivityManager::class.java)
        val cb = if (Build.VERSION.SDK_INT >= 31) {
            Callback(ConnectivityManager.NetworkCallback.FLAG_INCLUDE_LOCATION_INFO)
        } else {
            Callback()
        }
        runCatching { manager.registerDefaultNetworkCallback(cb) }.onSuccess { callback = cb }
    }

    private class Callback : ConnectivityManager.NetworkCallback {
        constructor() : super()

        @TargetApi(31)
        constructor(flags: Int) : super(flags)

        override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
            val kind = when {
                caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> 1
                caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> 2
                else -> 3
            }
            val ssid = if (kind == 1 && Build.VERSION.SDK_INT >= 29) (caps.transportInfo as? WifiInfo)?.ssid else null
            Native.networkChanged(kind, ssid)
        }

        override fun onLost(network: Network) = Native.networkChanged(0, null)
    }
}
