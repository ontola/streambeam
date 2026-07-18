package io.streambeam.streambeam

import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.projection.MediaProjectionManager
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Base64
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.WindowManager
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.util.ArrayDeque
import java.util.concurrent.ConcurrentHashMap

class MainActivity : FlutterActivity() {
    private var deepLinkChannel: MethodChannel? = null
    private var pendingLink: String? = null
    private var multicastLock: WifiManager.MulticastLock? = null
    private var streamingWifiLock: WifiManager.WifiLock? = null
    private var gamepadBridge: GamepadBridge? = null
    private var pendingCaptureResult: MethodChannel.Result? = null
    private var captureRequestInFlight = false
    private var captureRequestWasRemote = false
    private var remoteCaptureRequestHandled = false
    private var activityResumed = false
    private var captureNotificationShown = false
    private val captureHandler = Handler(Looper.getMainLooper())
    private var capturePoll: Runnable? = null
    private val nativeHosts = ConcurrentHashMap<String, Map<String, Any>>()
    private val pendingResolves = ArrayDeque<NsdServiceInfo>()
    private var resolvingService = false
    private var nsdDiscoveryListener: NsdManager.DiscoveryListener? = null

    private companion object {
        const val REQUEST_SCREEN_CAPTURE = 9731
        const val CAPTURE_REQUEST_CHANNEL = "streambeam_capture_requests"
        const val CAPTURE_REQUEST_NOTIFICATION = 44
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        // Let the fullscreen player own the landscape cutout strip. Flutter's
        // normal screens still respect MediaQuery/SafeArea, while the player
        // paints the exposed strip black and can place its menu affordance in
        // it instead of leaving the activity theme's white background visible.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            val attributes = window.attributes
            attributes.layoutInDisplayCutoutMode =
                WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            window.attributes = attributes
        }
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
            "streambeam/native_discovery",
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "start" -> {
                    startNativeDiscovery()
                    result.success(null)
                }
                "getHosts" -> result.success(nativeHosts.values.toList())
                else -> result.notImplemented()
            }
        }
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "streambeam/screencast",
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "requestCapture" -> requestScreenCapture(result, remote = false)
                "isCaptureActive" -> result.success(ScreenCaptureService.isCaptureActive())
                "stopCapture" -> {
                    stopService(Intent(this, ScreenCaptureService::class.java))
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }
        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "streambeam/remote_input",
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "isEnabled" -> result.success(remoteTouchEnabled())
                "openSettings" -> {
                    startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }
        pendingLink = linkFrom(intent)
        requestPostNotifications()
        startCaptureRequestPolling()
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

    private fun remoteTouchEnabled(): Boolean {
        val enabled = Settings.Secure.getString(
            contentResolver,
            Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
        ) ?: return false
        val component = "$packageName/${RemoteTouchService::class.java.name}"
        return enabled.split(':').any { it.equals(component, ignoreCase = true) }
    }

    private fun requestScreenCapture(result: MethodChannel.Result?, remote: Boolean) {
        if (captureRequestInFlight) {
            result?.error("busy", "a capture request is already in progress", null)
            return
        }
        captureRequestInFlight = true
        captureRequestWasRemote = remote
        pendingCaptureResult = result
        val mpm = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        try {
            startActivityForResult(mpm.createScreenCaptureIntent(), REQUEST_SCREEN_CAPTURE)
        } catch (e: Exception) {
            captureRequestInFlight = false
            pendingCaptureResult = null
            result?.error("capture_prompt", e.message, null)
        }
    }

    /**
     * Turn a verified viewer's SessionRequest into Android's mandatory system
     * consent prompt. The Rust flag is only set after pairing/authentication,
     * so discovery traffic and unknown devices can never raise this UI.
     */
    private fun startCaptureRequestPolling() {
        if (capturePoll != null) return
        val poll = object : Runnable {
            override fun run() {
                val wanted = try {
                    NativeStream.nativeCaptureRequested()
                } catch (_: Exception) {
                    false
                }
                if (!wanted) {
                    remoteCaptureRequestHandled = false
                    cancelCaptureRequestNotification()
                    if (ScreenCaptureService.isRemoteCaptureActive()) {
                        stopService(Intent(this@MainActivity, ScreenCaptureService::class.java))
                    }
                } else if (
                    !ScreenCaptureService.isCaptureActive() &&
                    !captureRequestInFlight &&
                    !remoteCaptureRequestHandled
                ) {
                    if (activityResumed) {
                        remoteCaptureRequestHandled = true
                        cancelCaptureRequestNotification()
                        requestScreenCapture(null, remote = true)
                    } else {
                        showCaptureRequestNotification()
                    }
                }
                captureHandler.postDelayed(this, 300)
            }
        }
        capturePoll = poll
        captureHandler.post(poll)
    }

    private fun showCaptureRequestNotification() {
        if (captureNotificationShown) return
        captureNotificationShown = true
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            manager.createNotificationChannel(
                NotificationChannel(
                    CAPTURE_REQUEST_CHANNEL,
                    "Screen sharing requests",
                    NotificationManager.IMPORTANCE_HIGH,
                ),
            )
        }
        var flags = PendingIntent.FLAG_UPDATE_CURRENT
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            flags = flags or PendingIntent.FLAG_IMMUTABLE
        }
        val open = PendingIntent.getActivity(
            this,
            CAPTURE_REQUEST_NOTIFICATION,
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            flags,
        )
        val notification = Notification.Builder(this, CAPTURE_REQUEST_CHANNEL)
            .setContentTitle("A paired device wants to watch")
            .setContentText("Tap to open Android's screen-sharing confirmation.")
            .setSmallIcon(android.R.drawable.ic_menu_view)
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        manager.notify(CAPTURE_REQUEST_NOTIFICATION, notification)
    }

    private fun cancelCaptureRequestNotification() {
        if (!captureNotificationShown) return
        captureNotificationShown = false
        (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager)
            .cancel(CAPTURE_REQUEST_NOTIFICATION)
    }

    override fun onResume() {
        super.onResume()
        activityResumed = true
    }

    override fun onPause() {
        activityResumed = false
        super.onPause()
    }

    @Deprecated("startActivityForResult result routing")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQUEST_SCREEN_CAPTURE) return
        captureRequestInFlight = false
        val wasRemote = captureRequestWasRemote
        captureRequestWasRemote = false
        val pending = pendingCaptureResult
        pendingCaptureResult = null
        if (resultCode == Activity.RESULT_OK && data != null) {
            val svc = Intent(this, ScreenCaptureService::class.java).apply {
                putExtra(ScreenCaptureService.EXTRA_RESULT_CODE, resultCode)
                putExtra(ScreenCaptureService.EXTRA_RESULT_DATA, data)
                putExtra(ScreenCaptureService.EXTRA_REMOTE_REQUEST, wasRemote)
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
        capturePoll?.let { captureHandler.removeCallbacks(it) }
        capturePoll = null
        stopNativeDiscovery()
        gamepadBridge?.dispose()
        gamepadBridge = null
        releaseMulticastLock()
        releaseStreamingWifiLock()
        super.onDestroy()
    }

    private fun startNativeDiscovery() {
        if (nsdDiscoveryListener != null) return
        val nsd = getSystemService(Context.NSD_SERVICE) as NsdManager
        val listener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(serviceType: String) = Unit
            override fun onDiscoveryStopped(serviceType: String) = Unit
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                nsdDiscoveryListener = null
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) = Unit

            override fun onServiceFound(serviceInfo: NsdServiceInfo) {
                if (!serviceInfo.serviceType.startsWith("_streambeam._udp")) return
                pendingResolves.addLast(serviceInfo)
                resolveNext(nsd)
            }

            override fun onServiceLost(serviceInfo: NsdServiceInfo) {
                nativeHosts.remove(serviceInfo.serviceName)
            }
        }
        nsdDiscoveryListener = listener
        try {
            nsd.discoverServices("_streambeam._udp.", NsdManager.PROTOCOL_DNS_SD, listener)
        } catch (_: Exception) {
            nsdDiscoveryListener = null
        }
    }

    @Suppress("DEPRECATION")
    private fun resolveNext(nsd: NsdManager) {
        if (resolvingService || pendingResolves.isEmpty()) return
        resolvingService = true
        val candidate = pendingResolves.removeFirst()
        nsd.resolveService(candidate, object : NsdManager.ResolveListener {
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                resolvingService = false
                resolveNext(nsd)
            }

            override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
                val fingerprint = serviceInfo.attributes["fp"]
                    ?.toString(Charsets.UTF_8)
                    ?.let {
                        try {
                            Base64.decode(it, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
                        } catch (_: IllegalArgumentException) {
                            null
                        }
                    }
                val address = serviceInfo.host?.hostAddress
                if (fingerprint != null && address != null) {
                    nativeHosts[serviceInfo.serviceName] = mapOf(
                        "name" to serviceInfo.serviceName,
                        "port" to serviceInfo.port,
                        "addrs" to listOf(address),
                        "pubkey" to fingerprint,
                    )
                }
                resolvingService = false
                resolveNext(nsd)
            }
        })
    }

    private fun stopNativeDiscovery() {
        val listener = nsdDiscoveryListener ?: return
        nsdDiscoveryListener = null
        try {
            val nsd = getSystemService(Context.NSD_SERVICE) as NsdManager
            nsd.stopServiceDiscovery(listener)
        } catch (_: Exception) {
        }
        pendingResolves.clear()
        resolvingService = false
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
