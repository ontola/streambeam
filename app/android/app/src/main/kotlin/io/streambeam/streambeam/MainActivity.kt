package io.streambeam.streambeam

import android.content.Intent
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private var deepLinkChannel: MethodChannel? = null
    private var pendingLink: String? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        flutterEngine.platformViewsController.registry.registerViewFactory(
            "streambeam/video",
            StreamViewFactory(flutterEngine.dartExecutor.binaryMessenger),
        )
        deepLinkChannel = MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "streambeam/deeplink",
        )
        deepLinkChannel?.setMethodCallHandler { call, result ->
            when (call.method) {
                // Dart pulls the link that launched the app (cold start); the
                // link is consumed so a hot restart doesn't replay it.
                "getInitialLink" -> {
                    result.success(pendingLink)
                    pendingLink = null
                }
                else -> result.notImplemented()
            }
        }
        pendingLink = linkFrom(intent)
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
