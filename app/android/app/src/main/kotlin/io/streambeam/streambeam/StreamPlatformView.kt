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

    init {
        surfaceView.holder.addCallback(this)
    }

    override fun getView() = surfaceView

    override fun dispose() {
        surfaceView.holder.removeCallback(this)
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        val ptr = NativeStream.nativeWindowFromSurface(holder.surface)
        channel.invokeMethod("surfaceReady", ptr)
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {}
}

class StreamViewFactory(private val messenger: BinaryMessenger) :
    PlatformViewFactory(StandardMessageCodec.INSTANCE) {
    override fun create(context: Context, viewId: Int, args: Any?): PlatformView {
        return StreamPlatformView(context, messenger)
    }
}
