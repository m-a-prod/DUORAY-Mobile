package space.dualizm.duoray

import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService

/**
 * The Quick Settings tile: on while connected, with the exit country under
 * it; a tap connects to the last server or disconnects (Rust decides, the
 * same as the button in the app).
 */
class DuorayTileService : TileService() {
    override fun onStartListening() {
        super.onStartListening()
        Native.boot(this)
        show()
    }

    override fun onClick() {
        super.onClick()
        Native.boot(this)
        if (isLocked) {
            unlockAndRun { Native.toggle() }
        } else {
            Native.toggle()
        }
    }

    private fun show() {
        val tile = qsTile ?: return
        val status = ConnectionStatus.current
        tile.state = if (status.active) Tile.STATE_ACTIVE else Tile.STATE_INACTIVE
        tile.label = getString(R.string.app_name)
        if (Build.VERSION.SDK_INT >= 29) {
            tile.subtitle = when (status.state) {
                1 -> getString(R.string.tile_connecting)
                2 -> status.title
                else -> getString(R.string.tile_off)
            }
        }
        if (Build.VERSION.SDK_INT >= 30) {
            tile.stateDescription = tile.subtitle
        }
        tile.updateTile()
    }
}
