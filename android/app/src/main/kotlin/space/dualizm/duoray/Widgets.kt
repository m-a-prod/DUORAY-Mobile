package space.dualizm.duoray

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.SizeF
import android.view.View
import android.widget.RemoteViews
import org.json.JSONObject
import java.io.File

/**
 * The home screen widgets: «Кнопка», «Подключение», «Подробно» and
 * «Избранное». They take the connection from Rust's status (Native.onStatus)
 * and the app's theme and pinned servers from files/widgets.json
 * (src/widgets.rs), and redraw on every change of either.
 */
object Widgets {
    enum class Kind(val provider: Class<out AppWidgetProvider>) {
        BUTTON(ButtonWidget::class.java),
        STATUS(StatusWidget::class.java),
        DETAIL(DetailWidget::class.java),
        FAVORITES(FavoritesWidget::class.java),
    }

    private val handler = Handler(Looper.getMainLooper())
    private var scheduled = false
    private var lastRun = 0L

    /** A tap's expected state (and server), drawn until Rust reports. */
    private var expected: Expected? = null

    private class Expected(val state: Int, val key: String?)

    /** Rust's status changed: the tap's guess gives way to the real thing. */
    fun statusChanged(context: Context) {
        expected = null
        refresh(context)
    }

    /** Redraws every widget, at most once a second (speeds tick that often). */
    fun refresh(context: Context) {
        val app = context.applicationContext
        if (scheduled) return
        scheduled = true
        val wait = (lastRun + 1000 - SystemClock.uptimeMillis()).coerceIn(0, 1000)
        handler.postDelayed({
            scheduled = false
            lastRun = SystemClock.uptimeMillis()
            updateAll(app)
        }, wait)
    }

    /**
     * Answers a tap at once: «Подключение…» or «Отключение…» before the
     * VPN does anything. Rust's next status replaces it; a guess Rust never
     * answers (nothing changed) is dropped after a few seconds.
     */
    fun expect(context: Context, state: Int, key: String? = null) {
        val app = context.applicationContext
        val guess = Expected(state, key)
        expected = guess
        updateAll(app)
        handler.postDelayed({
            if (expected === guess) {
                expected = null
                updateAll(app)
            }
        }, 4000)
    }

    fun updateAll(context: Context) {
        val manager = AppWidgetManager.getInstance(context) ?: return
        for (kind in Kind.entries) {
            val ids = manager.getAppWidgetIds(ComponentName(context, kind.provider))
            for (id in ids) update(context, manager, id, kind)
        }
    }

    fun update(context: Context, manager: AppWidgetManager, id: Int, kind: Kind) {
        val data = Data.load(context)
        val views = if (Build.VERSION.SDK_INT >= 31) {
            // The launcher picks the layout for the size it gives: resizing
            // needs no call back into the app.
            RemoteViews(sizes(kind).associateWith { build(context, kind, data, it.width, it.height) })
        } else {
            val options: Bundle = manager.getAppWidgetOptions(id)
            val w = options.getInt(AppWidgetManager.OPTION_APPWIDGET_MIN_WIDTH, 0).toFloat()
            val h = options.getInt(AppWidgetManager.OPTION_APPWIDGET_MAX_HEIGHT, 0).toFloat()
            build(context, kind, data, w, h)
        }
        runCatching { manager.updateAppWidget(id, views) }
        if (kind == Kind.FAVORITES) {
            @Suppress("DEPRECATION")
            runCatching { manager.notifyAppWidgetViewDataChanged(id, R.id.fav_list) }
        }
    }

    /** Sizes (dp) with a layout of their own. */
    private fun sizes(kind: Kind): List<SizeF> = when (kind) {
        Kind.BUTTON -> listOf(SizeF(40f, 40f))
        Kind.STATUS -> listOf(SizeF(110f, 40f), SizeF(180f, 40f), SizeF(240f, 40f))
        // Heights where a line stops fitting (see detail()).
        Kind.DETAIL -> listOf(110f, 132f, 162f, 190f).flatMap { h -> listOf(SizeF(180f, h), SizeF(300f, h)) }
        // The rows scroll: only whether the hint under the heading fits.
        Kind.FAVORITES -> listOf(SizeF(110f, 60f), SizeF(110f, 100f))
    }

    // ── what is shown ──────────────────────────────────────────────

    private class Favorite(val key: String, val name: String, val flag: String)

    /** One palette of the app's theme (src/themes.rs, widget_colors). */
    private class Palette(private val json: JSONObject?, dark: Boolean) {
        private fun color(name: String, fallback: Long): Int =
            json?.optString(name)?.takeIf { it.startsWith("#") }?.let { runCatching { Color.parseColor(it) }.getOrNull() } ?: fallback.toInt()

        val light = json?.optBoolean("light", !dark) ?: !dark
        val card = color("card", if (light) 0xFFECE6F0 else 0xFF2A282D)
        val fg = color("fg", if (light) 0xFF1D1B20 else 0xFFE6E1E5)
        val accent = color("accent", if (light) 0xFF6750A4 else 0xFFD0BCFF)
        val onAccent = color("on_accent", if (light) 0xFFFFFFFF else 0xFF381E72)
        val tonal = color("tonal", if (light) 0xFFE8DEF8 else 0xFF4A4458)
        val onTonal = color("on_tonal", if (light) 0xFF1E192B else 0xFFE8DEF8)

        /** Secondary text: the text color toward the card, still ≥ 4.5:1. */
        val muted = blend(fg, card, 0.72f)
        val danger = (if (light) 0xFFB3261E else 0xFFF2B8B5).toInt()
        val onDanger = (if (light) 0xFFFFFFFF else 0xFF601410).toInt()
    }

    private class Data(val status: ConnectionStatus, val colors: Palette, val favorites: List<Favorite>, val last: Favorite?) {
        val state get() = status.state

        companion object {
            fun load(context: Context): Data {
                val json = runCatching { JSONObject(File(context.filesDir, "widgets.json").readText()) }.getOrNull()
                val night = context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
                val theme = json?.optJSONObject("theme")
                val colors = Palette(theme?.optJSONObject(if (night) "dark" else "light"), night)
                fun fav(o: JSONObject?) = o?.let { Favorite(it.optString("key"), it.optString("name"), it.optString("flag")) }
                val list = json?.optJSONArray("favorites")
                val favorites = if (list == null) emptyList() else (0 until list.length()).mapNotNull { fav(list.optJSONObject(it)) }
                var status = ConnectionStatus.current
                val guess = expected
                if (guess != null) {
                    val title = context.getString(if (guess.state == 0) R.string.widget_disconnecting else R.string.widget_connecting)
                    status = status.copy(state = 1, title = title, speeds = "", key = guess.key ?: status.key)
                }
                return Data(status, colors, favorites, fav(json?.optJSONObject("last")))
            }
        }
    }

    /** The exit country without its flag: "🇫🇮 Финляндия" → "Финляндия". */
    private fun country(status: ConnectionStatus): String =
        if (status.chip.isNotEmpty() && status.title.startsWith(status.chip)) status.title.removePrefix(status.chip).trim() else status.title

    /** A flag emoji (or nothing) for the disc: the exit when connected, else the server's. */
    private fun flag(data: Data): String {
        val chip = data.status.chip
        if (data.state == 2 && chip.codePointCount(0, chip.length) == 2) return chip
        return data.favorites.firstOrNull { it.key == data.status.key }?.flag ?: data.last?.flag.orEmpty()
    }

    private fun stateLabel(context: Context, data: Data): String = when (data.state) {
        2 -> context.getString(R.string.widget_on)
        // «Подключение…» or, after a tap, «Отключение…».
        1 -> data.status.title
        3 -> context.getString(R.string.widget_failed)
        else -> context.getString(R.string.widget_off)
    }

    /** The text under the title: the server (and ping), or what went wrong. */
    private fun subtitle(data: Data): String = when (data.state) {
        0 -> data.status.text.ifEmpty { data.last?.name.orEmpty() }
        else -> data.status.text
    }

    private fun title(context: Context, data: Data): String = when (data.state) {
        2 -> country(data.status)
        0 -> context.getString(R.string.widget_off)
        else -> data.status.title
    }

    // ── layouts ────────────────────────────────────────────────────

    private fun build(context: Context, kind: Kind, data: Data, w: Float, h: Float): RemoteViews = when (kind) {
        Kind.BUTTON -> button(context, data)
        Kind.STATUS -> status(context, data, w)
        Kind.DETAIL -> detail(context, data, w, h)
        Kind.FAVORITES -> favorites(context, data, h)
    }

    private fun button(context: Context, data: Data): RemoteViews {
        val v = RemoteViews(context.packageName, R.layout.widget_button)
        val c = data.colors
        tint(v, R.id.bg, c.card)
        power(context, v, data, R.id.power, R.id.power_bg, R.id.power_icon)
        // One cell: a word that fits.
        val label = when (data.state) {
            2 -> R.string.widget_short_on
            1 -> R.string.widget_short_busy
            3 -> R.string.widget_failed
            else -> R.string.widget_short_off
        }
        v.setTextViewText(R.id.label, context.getString(label))
        v.setTextColor(R.id.label, c.fg)
        return v
    }

    private fun status(context: Context, data: Data, w: Float): RemoteViews {
        val v = RemoteViews(context.packageName, R.layout.widget_status)
        val c = data.colors
        tint(v, R.id.bg, c.card)
        disc(v, data, R.id.flag_bg, R.id.flag, R.id.logo)
        // Narrow: the text gets the room, not the flag.
        v.setViewVisibility(R.id.flag_box, if (w >= 180f) View.VISIBLE else View.GONE)
        v.setTextViewText(R.id.title, title(context, data))
        v.setTextColor(R.id.title, c.fg)
        v.setTextViewText(R.id.subtitle, subtitle(data))
        v.setTextColor(R.id.subtitle, if (data.state == 3) c.danger else c.muted)
        val speeds = data.status.speeds.takeIf { data.state == 2 && w >= 240f && it.isNotEmpty() }
        v.setViewVisibility(R.id.speeds, if (speeds != null) View.VISIBLE else View.GONE)
        v.setTextViewText(R.id.speeds, speeds.orEmpty())
        v.setTextColor(R.id.speeds, c.muted)
        v.setOnClickPendingIntent(R.id.open, openApp(context))
        power(context, v, data, R.id.power, R.id.power_bg, R.id.power_icon)
        return v
    }

    private fun detail(context: Context, data: Data, w: Float, h: Float): RemoteViews {
        val v = RemoteViews(context.packageName, R.layout.widget_detail)
        val c = data.colors
        // Everything needs about 190 dp; a lower widget cut its buttons off.
        // Lines go from the least needed: time and speeds, then the brand
        // and state chip (the button says it), then the server line.
        val full = h <= 0f || h >= 190f
        val header = h <= 0f || h >= 162f
        val tiny = h in 1f..131f
        v.setViewVisibility(R.id.header, if (header) View.VISIBLE else View.GONE)
        v.setViewVisibility(R.id.flag_box, if (tiny) View.GONE else View.VISIBLE)
        if (tiny) {
            val pad = (10 * context.resources.displayMetrics.density).toInt()
            v.setViewPadding(R.id.content, pad, pad, pad, pad)
        }
        tint(v, R.id.bg, c.card)
        v.setInt(R.id.brand_logo, "setColorFilter", c.fg)
        v.setTextColor(R.id.brand, c.fg)
        // The state chip: accent while connected, the danger color on errors.
        val (chipBg, chipFg) = when (data.state) {
            2 -> c.accent to c.onAccent
            3 -> c.danger to c.onDanger
            else -> c.tonal to c.onTonal
        }
        tint(v, R.id.chip_bg, chipBg)
        v.setTextViewText(R.id.chip, stateLabel(context, data))
        v.setTextColor(R.id.chip, chipFg)
        disc(v, data, R.id.flag_bg, R.id.flag, R.id.logo)
        // The chip says the state: the title is the country, or the server.
        val server = data.status.text.substringBefore(" · ").ifEmpty { data.last?.name.orEmpty() }
        val (title, sub) = when (data.state) {
            2 -> country(data.status) to data.status.text
            3 -> (data.last?.name ?: context.getString(R.string.widget_failed)) to data.status.text
            1 -> data.status.key.let { k -> data.favorites.firstOrNull { it.key == k }?.name ?: server } to ""
            else -> server.ifEmpty { context.getString(R.string.app_name) } to ""
        }
        v.setTextViewText(R.id.title, title)
        v.setTextColor(R.id.title, c.fg)
        v.setTextViewText(R.id.subtitle, sub)
        v.setViewVisibility(R.id.subtitle, if (sub.isEmpty() || tiny) View.GONE else View.VISIBLE)
        v.setTextColor(R.id.subtitle, if (data.state == 3) c.danger else c.muted)
        val on = data.state == 2
        v.setViewVisibility(R.id.stats, if (on && full) View.VISIBLE else View.GONE)
        if (on && data.status.sinceMs > 0) {
            val base = SystemClock.elapsedRealtime() - (System.currentTimeMillis() - data.status.sinceMs).coerceAtLeast(0)
            v.setChronometer(R.id.timer, base, null, true)
            v.setViewVisibility(R.id.timer, View.VISIBLE)
        } else {
            v.setChronometer(R.id.timer, SystemClock.elapsedRealtime(), null, false)
            v.setViewVisibility(R.id.timer, View.GONE)
        }
        v.setTextColor(R.id.timer, c.fg)
        v.setTextViewText(R.id.speeds, data.status.speeds)
        v.setTextColor(R.id.speeds, c.muted)
        v.setOnClickPendingIntent(R.id.open, openApp(context))
        // The big button says what a tap does.
        val active = data.state == 1 || data.state == 2
        val (bg, fg) = if (active) c.tonal to c.onTonal else c.accent to c.onAccent
        tint(v, R.id.power_bg, bg)
        v.setInt(R.id.power_icon, "setColorFilter", fg)
        val label = context.getString(if (active) R.string.widget_disconnect else R.string.widget_connect)
        v.setTextViewText(R.id.power_text, label)
        v.setTextColor(R.id.power_text, fg)
        v.setContentDescription(R.id.power, label)
        v.setOnClickPendingIntent(R.id.power, action(context, WidgetActions.TOGGLE))
        tint(v, R.id.servers_bg, c.tonal)
        v.setInt(R.id.servers_icon, "setColorFilter", c.onTonal)
        v.setTextColor(R.id.servers_text, c.onTonal)
        // Narrow: the list icon alone.
        v.setViewVisibility(R.id.servers_text, if (w >= 300f) View.VISIBLE else View.GONE)
        v.setContentDescription(R.id.servers, context.getString(R.string.widget_servers))
        v.setOnClickPendingIntent(R.id.servers, openServers(context))
        return v
    }

    private fun favorites(context: Context, data: Data, h: Float): RemoteViews {
        val v = RemoteViews(context.packageName, R.layout.widget_favorites)
        val c = data.colors
        tint(v, R.id.bg, c.card)
        v.setTextColor(R.id.heading, c.fg)
        // Under the heading: where the connection is now.
        val now = if (data.state == 2) "${stateLabel(context, data)} · ${country(data.status)}" else stateLabel(context, data)
        v.setTextViewText(R.id.subtitle, now)
        v.setTextColor(R.id.subtitle, if (data.state == 3) c.danger else c.muted)
        v.setOnClickPendingIntent(R.id.open, openApp(context))
        power(context, v, data, R.id.power, R.id.power_bg, R.id.power_icon)

        v.setViewVisibility(R.id.empty, if (data.favorites.isEmpty() && (h <= 0f || h >= 100f)) View.VISIBLE else View.GONE)
        v.setTextColor(R.id.empty, c.muted)
        if (data.favorites.isEmpty()) v.setOnClickPendingIntent(R.id.empty, openApp(context))
        // The rows come from FavoritesService and scroll inside the widget.
        val rows = Intent(context, FavoritesService::class.java).setData(android.net.Uri.parse("duoray://widget/favorites"))
        @Suppress("DEPRECATION")
        v.setRemoteAdapter(R.id.fav_list, rows)
        v.setPendingIntentTemplate(R.id.fav_list, connectTemplate(context))
        return v
    }

    /** The rows of «Избранное», as FavoritesService hands them out. */
    fun favoriteRows(context: Context): List<RemoteViews> {
        val data = Data.load(context)
        val c = data.colors
        val active = data.state == 1 || data.state == 2
        return data.favorites.map { fav ->
            val v = RemoteViews(context.packageName, R.layout.widget_favorite_row)
            val current = active && fav.key == data.status.key
            tint(v, R.id.row_bg, if (current) c.accent else c.tonal)
            v.setTextViewText(R.id.row_flag, fav.flag)
            v.setTextViewText(R.id.row_name, fav.name)
            v.setTextColor(R.id.row_name, if (current) c.onAccent else c.onTonal)
            v.setViewVisibility(R.id.row_check, if (current && data.state == 2) View.VISIBLE else View.GONE)
            v.setInt(R.id.row_check, "setColorFilter", c.onAccent)
            v.setContentDescription(R.id.row, context.getString(R.string.widget_connect_to, fav.name))
            v.setOnClickFillInIntent(R.id.row, Intent().putExtra(WidgetActions.EXTRA_KEY, fav.key))
            v
        }
    }

    /** A pinned server's tap: the row fills in its key. Mutable for that. */
    private fun connectTemplate(context: Context): PendingIntent = PendingIntent.getBroadcast(
        context,
        2,
        Intent(context, WidgetActions::class.java).setAction(WidgetActions.CONNECT),
        (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0) or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** The round power button, colored by the state. */
    private fun power(context: Context, v: RemoteViews, data: Data, button: Int, bg: Int, icon: Int) {
        val c = data.colors
        val (b, f) = when (data.state) {
            2 -> c.accent to c.onAccent
            1 -> c.tonal to c.accent
            3 -> c.danger to c.onDanger
            else -> c.tonal to c.onTonal
        }
        v.setInt(bg, "setColorFilter", b)
        v.setInt(icon, "setColorFilter", f)
        val active = data.state == 1 || data.state == 2
        v.setContentDescription(button, context.getString(if (active) R.string.widget_disconnect else R.string.widget_connect))
        v.setOnClickPendingIntent(button, action(context, WidgetActions.TOGGLE))
    }

    /** The disc: the flag when there is one, else the DUORAY mark. */
    private fun disc(v: RemoteViews, data: Data, bg: Int, flag: Int, logo: Int) {
        val c = data.colors
        val emoji = flag(data)
        v.setInt(bg, "setColorFilter", if (data.state == 2) c.accent else c.tonal)
        v.setTextViewText(flag, emoji)
        v.setViewVisibility(flag, if (emoji.isNotEmpty()) View.VISIBLE else View.GONE)
        v.setViewVisibility(logo, if (emoji.isEmpty()) View.VISIBLE else View.GONE)
        v.setInt(logo, "setColorFilter", if (data.state == 2) c.onAccent else c.onTonal)
    }

    private fun tint(v: RemoteViews, id: Int, color: Int) {
        v.setInt(id, "setColorFilter", color)
    }

    private fun openApp(context: Context): PendingIntent = PendingIntent.getActivity(
        context,
        0,
        Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** The app on the list with the server in use (src/app.rs, show_in_use). */
    private fun openServers(context: Context): PendingIntent = PendingIntent.getActivity(
        context,
        1,
        Intent(context, MainActivity::class.java)
            .setData(android.net.Uri.parse("duoray://servers"))
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    private fun action(context: Context, name: String, key: String? = null): PendingIntent {
        val intent = Intent(context, WidgetActions::class.java).setAction(name)
        if (key != null) intent.putExtra(WidgetActions.EXTRA_KEY, key)
        return PendingIntent.getBroadcast(
            context,
            (name + key.orEmpty()).hashCode(),
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }

    private fun blend(a: Int, b: Int, t: Float): Int {
        fun mix(x: Int, y: Int) = (x * t + y * (1 - t)).toInt().coerceIn(0, 255)
        return Color.rgb(mix(Color.red(a), Color.red(b)), mix(Color.green(a), Color.green(b)), mix(Color.blue(a), Color.blue(b)))
    }
}

/** What a widget's buttons do: connect or disconnect, connect to a pinned server. */
class WidgetActions : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        Native.boot(context)
        val now = ConnectionStatus.current
        when (intent.action) {
            TOGGLE -> {
                Widgets.expect(context, if (now.active) 0 else 1)
                Native.toggle()
            }
            CONNECT -> {
                val key = intent.getStringExtra(EXTRA_KEY) ?: return
                if (now.active && now.key == key) return
                Widgets.expect(context, 1, key)
                Native.connectTo(key)
            }
        }
    }

    companion object {
        const val TOGGLE = "space.dualizm.duoray.widget.TOGGLE"
        const val CONNECT = "space.dualizm.duoray.widget.CONNECT"
        const val EXTRA_KEY = "key"
    }
}

/** A widget kind: the launcher asks for it to be drawn (added, resized, rebooted). */
open class WidgetProvider(private val kind: Widgets.Kind) : AppWidgetProvider() {
    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) {
        for (id in ids) Widgets.update(context, manager, id, kind)
    }

    override fun onAppWidgetOptionsChanged(context: Context, manager: AppWidgetManager, id: Int, options: Bundle) {
        Widgets.update(context, manager, id, kind)
    }
}

class ButtonWidget : WidgetProvider(Widgets.Kind.BUTTON)

class StatusWidget : WidgetProvider(Widgets.Kind.STATUS)

class DetailWidget : WidgetProvider(Widgets.Kind.DETAIL)

class FavoritesWidget : WidgetProvider(Widgets.Kind.FAVORITES)

/** The rows of «Избранное»: a list that scrolls inside the widget. */
class FavoritesService : android.widget.RemoteViewsService() {
    override fun onGetViewFactory(intent: Intent): RemoteViewsFactory = Factory(applicationContext)

    private class Factory(private val context: Context) : RemoteViewsFactory {
        private var rows: List<RemoteViews> = emptyList()

        override fun onCreate() {}

        override fun onDataSetChanged() {
            rows = Widgets.favoriteRows(context)
        }

        override fun onDestroy() {}

        override fun getCount() = rows.size

        override fun getViewAt(position: Int): RemoteViews =
            rows.getOrNull(position) ?: RemoteViews(context.packageName, R.layout.widget_favorite_row)

        override fun getLoadingView(): RemoteViews? = null

        override fun getViewTypeCount() = 1

        override fun getItemId(position: Int) = position.toLong()

        override fun hasStableIds() = false
    }
}
