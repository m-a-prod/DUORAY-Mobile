package space.dualizm.duoray

import android.Manifest
import android.animation.ValueAnimator
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.graphics.Shader
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.util.Size
import android.util.TypedValue
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.View
import android.view.ViewGroup
import android.view.animation.LinearInterpolator
import android.widget.FrameLayout
import android.widget.ImageButton
import android.widget.TextView
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.Camera
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import zxingcpp.BarcodeReader
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.min

/**
 * The QR scanner: the camera full screen behind a window frame, a torch, and
 * "Из галереи" for a code in a screenshot. Decoding is zxing-cpp (Apache-2.0),
 * which reads small, blurred and inverted codes the Java ZXing missed. The
 * text goes back to MainActivity, which hands it to Rust.
 */
class ScanActivity : ComponentActivity() {
    private val reader = BarcodeReader(
        BarcodeReader.Options(
            formats = setOf(BarcodeReader.Format.QR_CODE, BarcodeReader.Format.MICRO_QR_CODE),
            tryHarder = true,
            tryRotate = true,
            tryInvert = true,
            tryDownscale = true,
        ),
    )
    private val done = AtomicBoolean(false)
    private lateinit var analyzer: ExecutorService
    private lateinit var preview: PreviewView
    private lateinit var torch: ImageButton
    private lateinit var overlay: ScanOverlay
    private var camera: Camera? = null
    private var torchOn = false

    private val askCamera = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) startCamera() else overlay.hint = getString(R.string.scan_no_camera)
    }
    private val pickImage = registerForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        uri?.let(::readImage)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        analyzer = Executors.newSingleThreadExecutor()
        setContentView(buildViews())
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            startCamera()
        } else {
            askCamera.launch(Manifest.permission.CAMERA)
        }
    }

    override fun onDestroy() {
        analyzer.shutdown()
        super.onDestroy()
    }

    private fun buildViews(): View {
        val root = FrameLayout(this).apply { setBackgroundColor(Color.BLACK) }
        preview = PreviewView(this).apply {
            scaleType = PreviewView.ScaleType.FILL_CENTER
            implementationMode = PreviewView.ImplementationMode.COMPATIBLE
        }
        overlay = ScanOverlay(this).apply { hint = getString(R.string.scan_hint) }
        root.addView(preview, match())
        root.addView(overlay, match())

        val close = roundButton(R.drawable.ic_close, getString(R.string.scan_close)) { finish() }
        torch = roundButton(R.drawable.ic_flash_off, getString(R.string.scan_torch)) { toggleTorch() }.apply {
            visibility = View.GONE
        }
        val gallery = TextView(this).apply {
            text = getString(R.string.scan_gallery)
            setTextColor(Color.WHITE)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            gravity = Gravity.CENTER
            setCompoundDrawablesRelativeWithIntrinsicBounds(R.drawable.ic_image, 0, 0, 0)
            compoundDrawablePadding = dp(10)
            setPadding(dp(22), 0, dp(24), 0)
            background = pill(0x33FFFFFF)
            isClickable = true
            isFocusable = true
            setOnClickListener {
                pickImage.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
            }
        }
        root.addView(close, FrameLayout.LayoutParams(dp(48), dp(48), Gravity.TOP or Gravity.START))
        root.addView(torch, FrameLayout.LayoutParams(dp(48), dp(48), Gravity.TOP or Gravity.END))
        root.addView(gallery, FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, dp(52), Gravity.BOTTOM or Gravity.CENTER_HORIZONTAL))

        // Edge to edge: the camera under the bars, the buttons clear of them.
        ViewCompat.setOnApplyWindowInsetsListener(root) { _, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            fun View.place(top: Int, bottom: Int, start: Int, end: Int) {
                (layoutParams as FrameLayout.LayoutParams).setMargins(start, top, end, bottom)
                requestLayout()
            }
            close.place(bars.top + dp(8), 0, bars.left + dp(12), 0)
            torch.place(bars.top + dp(8), 0, 0, bars.right + dp(12))
            gallery.place(0, bars.bottom + dp(28), 0, 0)
            insets
        }
        return root
    }

    private fun startCamera() {
        val future = ProcessCameraProvider.getInstance(this)
        future.addListener({
            val provider = runCatching { future.get() }.getOrNull() ?: return@addListener
            val previewCase = Preview.Builder().build().also { it.surfaceProvider = preview.surfaceProvider }
            val analysis = ImageAnalysis.Builder()
                .setResolutionSelector(
                    ResolutionSelector.Builder()
                        .setResolutionStrategy(
                            ResolutionStrategy(Size(1280, 720), ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER),
                        )
                        .build(),
                )
                .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                .build()
            analysis.setAnalyzer(analyzer) { image ->
                val text = runCatching { image.use { reader.read(it) } }.getOrNull()
                    ?.firstNotNullOfOrNull { result -> result.text?.takeIf { it.isNotBlank() } }
                if (text != null) found(text)
            }
            runCatching {
                provider.unbindAll()
                camera = provider.bindToLifecycle(this, CameraSelector.DEFAULT_BACK_CAMERA, previewCase, analysis)
            }.onFailure {
                overlay.hint = getString(R.string.scan_no_camera)
            }
            torch.visibility = if (camera?.cameraInfo?.hasFlashUnit() == true) View.VISIBLE else View.GONE
        }, ContextCompat.getMainExecutor(this))
    }

    private fun toggleTorch() {
        val control = camera?.cameraControl ?: return
        torchOn = !torchOn
        control.enableTorch(torchOn)
        torch.setImageResource(if (torchOn) R.drawable.ic_flash_on else R.drawable.ic_flash_off)
    }

    /** A code in a picture: decoded off the main thread, at most 2048 px. */
    private fun readImage(uri: Uri) {
        analyzer.execute {
            val text = runCatching { decode(uri) }.getOrNull()?.let { bitmap ->
                reader.read(bitmap).firstNotNullOfOrNull { result -> result.text?.takeIf { it.isNotBlank() } }
            }
            if (text != null) {
                found(text)
            } else {
                runOnUiThread { Toast.makeText(this, R.string.scan_not_found, Toast.LENGTH_SHORT).show() }
            }
        }
    }

    private fun decode(uri: Uri): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        contentResolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / sample > 2048) sample *= 2
        val options = BitmapFactory.Options().apply {
            inSampleSize = sample
            inPreferredConfig = Bitmap.Config.ARGB_8888
        }
        return contentResolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, options) }
    }

    private fun found(text: String) {
        if (!done.compareAndSet(false, true)) return
        runOnUiThread {
            val feedback = if (Build.VERSION.SDK_INT >= 30) HapticFeedbackConstants.CONFIRM else HapticFeedbackConstants.LONG_PRESS
            window.decorView.performHapticFeedback(feedback)
            setResult(RESULT_OK, Intent().putExtra(EXTRA_TEXT, text))
            finish()
        }
    }

    private fun roundButton(icon: Int, label: String, onClick: () -> Unit) = ImageButton(this).apply {
        setImageResource(icon)
        contentDescription = label
        background = pill(0x33FFFFFF)
        setOnClickListener { onClick() }
    }

    private fun pill(color: Int) = GradientDrawable().apply {
        shape = GradientDrawable.RECTANGLE
        cornerRadius = dp(26).toFloat()
        setColor(color)
    }

    private fun match() = FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)

    private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()

    companion object {
        const val EXTRA_TEXT = "text"
    }
}

/**
 * The dimmed camera around a clear square window with DUORAY-pink corners,
 * a line sweeping through it, and the hint under it.
 */
class ScanOverlay(context: Context) : View(context) {
    var hint: String = ""
        set(value) {
            field = value
            invalidate()
        }

    private val density = resources.displayMetrics.density
    private val scrim = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = 0x99000000.toInt() }
    private val corners = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = PINK
        style = Paint.Style.STROKE
        strokeWidth = 4 * density
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
    }
    private val line = Paint(Paint.ANTI_ALIAS_FLAG)
    private val text = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.WHITE
        textSize = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, 15f, resources.displayMetrics)
        textAlign = Paint.Align.CENTER
    }
    private val frame = RectF()
    private val hole = Path()
    private val bracket = Path()
    private var sweep = 0f
    private val animator = ValueAnimator.ofFloat(0f, 1f).apply {
        duration = 2200
        repeatMode = ValueAnimator.REVERSE
        repeatCount = ValueAnimator.INFINITE
        interpolator = LinearInterpolator()
        addUpdateListener {
            sweep = it.animatedValue as Float
            invalidate()
        }
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        animator.start()
    }

    override fun onDetachedFromWindow() {
        animator.cancel()
        super.onDetachedFromWindow()
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        val side = min(min(w, h) * 0.72f, 300 * density)
        val left = (w - side) / 2
        val top = (h - side) / 2 - 24 * density
        frame.set(left, top, left + side, top + side)
        val radius = 24 * density
        hole.reset()
        hole.fillType = Path.FillType.EVEN_ODD
        hole.addRect(0f, 0f, w.toFloat(), h.toFloat(), Path.Direction.CW)
        hole.addRoundRect(frame, radius, radius, Path.Direction.CW)
        val arm = side * 0.16f
        bracket.reset()
        with(frame) {
            bracket.moveTo(left, top + arm); bracket.lineTo(left, top + radius)
            bracket.quadTo(left, top, left + radius, top); bracket.lineTo(left + arm, top)
            bracket.moveTo(right - arm, top); bracket.lineTo(right - radius, top)
            bracket.quadTo(right, top, right, top + radius); bracket.lineTo(right, top + arm)
            bracket.moveTo(right, bottom - arm); bracket.lineTo(right, bottom - radius)
            bracket.quadTo(right, bottom, right - radius, bottom); bracket.lineTo(right - arm, bottom)
            bracket.moveTo(left + arm, bottom); bracket.lineTo(left + radius, bottom)
            bracket.quadTo(left, bottom, left, bottom - radius); bracket.lineTo(left, bottom - arm)
        }
        line.shader = LinearGradient(frame.left, 0f, frame.right, 0f, intArrayOf(0x00F4C2D7, PINK, 0x00F4C2D7), null, Shader.TileMode.CLAMP)
    }

    override fun onDraw(canvas: Canvas) {
        canvas.drawPath(hole, scrim)
        canvas.drawPath(bracket, corners)
        val inset = 18 * density
        val y = frame.top + inset + (frame.height() - 2 * inset) * sweep
        canvas.drawRoundRect(frame.left + inset, y - density, frame.right - inset, y + density, density, density, line)
        canvas.drawText(hint, frame.centerX(), frame.bottom + 44 * density, text)
    }

    private companion object {
        const val PINK = 0xFFF4C2D7.toInt()
    }
}
