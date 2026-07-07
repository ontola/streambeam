package io.streambeam.streambeam

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.projection.MediaProjectionManager
import android.net.wifi.WifiManager
import android.os.Build
import android.view.KeyEvent
import android.view.MotionEvent
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private var deepLinkChannel: MethodChannel? = null
    private var pendingLink: String? = null
    private var multicastLock: WifiManager.MulticastLock? = null
    private var streamingWifiLock: WifiManager.WifiLock? = null
    private var gamepadBridge: GamepadBridge? = null
    private var pendingCaptureResult: MethodChannel.Result? = null

    private companion object {
        const val REQUEST_SCREEN_CAPTURE = 9731
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        flutterEngine.platformViewsController.registry.registerViewFactory(
            "streambeam/video",
            StreamViewFactory(flutterEngine.dartExecutor.binaryMessenger),
        )
        gamepadBridge = GamepadBridge(this, flutterEngine.dartExecutor.binaryMessenger)
        deepLinkChannel = MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "streambeam/deeplink",
        )
        deepLinkChannel?.setMethodCallHandler { call, result ->
            when (call.method) {
                "getInitialLink" -> {
                    result.success(pendingLink)
                    pendingLink = null
                }
                else -> result.notImplemented()
            }
        }
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "streambeam/discovery",
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "acquireMulticastLock" -> {
                    try {
                        acquireMulticastLock()
                        result.success(null)
                    } catch (e: Exception) {
                        result.error("mdns", e.message, null)
                    }
                }
                "releaseMulticastLock" -> {
                    releaseMulticastLock()
                    result.success(null)
                }
                "acquireStreamingWifiLock" -> {
                    try {
                        acquireStreamingWifiLock()
                        result.success(null)
                    } catch (e: Exception) {
                        result.error("wifilock", e.message, null)
                    }
                }
                "releaseStreamingWifiLock" -> {
                    releaseStreamingWifiLock()
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "streambeam/device",
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "getName" -> result.success(deviceName())
                else -> result.notImplemented()
            }
        }
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "streambeam/screencast",
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "requestCapture" -> requestScreenCapture(result)
                "stopCapture" -> {
                    stopService(Intent(this, ScreenCaptureService::class.java))
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }
        pendingLink = linkFrom(intent)
        requestPostNotifications()
    }

    /** The user-set device name (Settings), falling back to the model. */
    private fun deviceName(): String {
        try {
            val setName = android.provider.Settings.Global.getString(
                contentResolver,
                android.provider.Settings.Global.DEVICE_NAME,
            )
            if (!setName.isNullOrBlank()) return setName
        } catch (_: Exception) {
        }
        val model = Build.MODEL ?: ""
        val mfr = Build.MANUFACTURER ?: ""
        return if (model.startsWith(mfr, ignoreCase = true) || mfr.isBlank()) {
            model.ifBlank { "Android device" }
        } else {
            "$mfr $model".trim()
        }
    }

    private fun requestPostNotifications() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        if (checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS)
            != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 9732)
        }
    }

    private fun requestScreenCapture(result: MethodChannel.Result) {
        if (pendingCaptureResult != null) {
            result.error("busy", "a capture request is already in progress", null)
            return
        }
        pendingCaptureResult = result
        val mpm = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        startActivityForResult(mpm.createScreenCaptureIntent(), REQUEST_SCREEN_CAPTURE)
    }

    @Deprecated("startActivityForResult result routing")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQUEST_SCREEN_CAPTURE) return
        val pending = pendingCaptureResult
        pendingCaptureResult = null
        if (resultCode == Activity.RESULT_OK && data != null) {
            val svc = Intent(this, ScreenCaptureService::class.java).apply {
                putExtra(ScreenCaptureService.EXTRA_RESULT_CODE, resultCode)
                putExtra(ScreenCaptureService.EXTRA_RESULT_DATA, data)
            }
            if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.O) {
                startForegroundService(svc)
            } else {
                startService(svc)
            }
            pending?.success(true)
        } else {
            pending?.success(false)
        }
    }

    override fun dispatchGenericMotionEvent(motionEvent: MotionEvent): Boolean {
        if (gamepadBridge?.onMotionEvent(motionEvent) == true) return true
        return super.dispatchGenericMotionEvent(motionEvent)
    }

    override fun dispatchKeyEvent(keyEvent: KeyEvent): Boolean {
        if (gamepadBridge?.onKeyEvent(keyEvent) == true) return true
        return super.dispatchKeyEvent(keyEvent)
    }

    override fun onDestroy() {
        gamepadBridge?.dispose()
        gamepadBridge = null
        releaseMulticastLock()
        releaseStreamingWifiLock()
        super.onDestroy()
    }

    private fun acquireMulticastLock() {
        if (multicastLock?.isHeld == true) return
        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        multicastLock = wifi.createMulticastLock("streambeam-mdns").apply {
            setReferenceCounted(true)
            acquire()
        }
    }

    private fun releaseMulticastLock() {
        multicastLock?.let { if (it.isHeld) it.release() }
        multicastLock = null
    }

    // WiFi power-save is brutal for real-time UDP: measured ping RTT to this
    // device swings 5-131ms while the radio naps between beacons, and bursts
    // of ~1.2KB video datagrams get dropped wholesale while small audio
    // packets survive. WIFI_MODE_FULL_LOW_LATENCY (API 29+) disables
    // power-save while held *and* the app is foreground — the same lock
    // Moonlight holds during streaming. Held only while a stream is active;
    // falls back to WIFI_MODE_FULL_HIGH_PERF on older devices.
    private fun acquireStreamingWifiLock() {
        if (streamingWifiLock?.isHeld == true) return
        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            WifiManager.WIFI_MODE_FULL_LOW_LATENCY
        } else {
            @Suppress("DEPRECATION")
            WifiManager.WIFI_MODE_FULL_HIGH_PERF
        }
        streamingWifiLock = wifi.createWifiLock(mode, "streambeam-stream").apply {
            setReferenceCounted(false)
            acquire()
        }
    }

    private fun releaseStreamingWifiLock() {
        streamingWifiLock?.let { if (it.isHeld) it.release() }
        streamingWifiLock = null
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        val link = linkFrom(intent) ?: return
        val channel = deepLinkChannel
        if (channel != null) {
            channel.invokeMethod("link", link)
        } else {
            pendingLink = link
        }
    }

    private fun linkFrom(intent: Intent?): String? =
        if (intent?.action == Intent.ACTION_VIEW) intent.dataString else null
}
