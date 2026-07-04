package io.streambeam.streambeam

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjectionManager
import android.net.wifi.WifiManager
import android.view.KeyEvent
import android.view.MotionEvent
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private var deepLinkChannel: MethodChannel? = null
    private var pendingLink: String? = null
    private var multicastLock: WifiManager.MulticastLock? = null
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
