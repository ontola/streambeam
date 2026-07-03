package io.streambeam.streambeam

import android.content.Context
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.StandardMessageCodec
import io.flutter.plugin.platform.PlatformView
import io.flutter.plugin.platform.PlatformViewFactory

class StreamPlatformView(
    context: Context,
    messenger: BinaryMessenger,
) : PlatformView, SurfaceHolder.Callback {
    private val surfaceView = SurfaceView(context)
    private val channel = MethodChannel(messenger, "streambeam/video_surface")
    private var lastWindowPtr: Long = 0L

    init {
        surfaceView.holder.addCallback(this)
    }

    override fun getView() = surfaceView

    override fun dispose() {
        surfaceView.holder.removeCallback(this)
        lastWindowPtr = 0L
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        notifySurfaceReady(holder.surface)
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        if (width > 0 && height > 0) {
            notifySurfaceReady(holder.surface)
        }
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        lastWindowPtr = 0L
    }

    private fun notifySurfaceReady(surface: Surface) {
        val ptr = NativeStream.nativeWindowFromSurface(surface)
        if (ptr == lastWindowPtr) return
        lastWindowPtr = ptr
        channel.invokeMethod("surfaceReady", ptr)
    }
}

class StreamViewFactory(private val messenger: BinaryMessenger) :
    PlatformViewFactory(StandardMessageCodec.INSTANCE) {
    override fun create(context: Context, viewId: Int, args: Any?): PlatformView {
        return StreamPlatformView(context, messenger)
    }
}
