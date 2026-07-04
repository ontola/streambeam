package io.streambeam.streambeam

import android.content.Context
import android.graphics.SurfaceTexture
import android.view.Surface
import android.view.TextureView
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.StandardMessageCodec
import io.flutter.plugin.platform.PlatformView
import io.flutter.plugin.platform.PlatformViewFactory

class StreamPlatformView(
    context: Context,
    messenger: BinaryMessenger,
) : PlatformView, TextureView.SurfaceTextureListener {
    private val textureView = TextureView(context)
    private val channel = MethodChannel(messenger, "streambeam/video_surface")
    private var lastWindowPtr: Long = 0L

    init {
        textureView.surfaceTextureListener = this
        textureView.isOpaque = true
    }

    override fun getView() = textureView

    override fun dispose() {
        textureView.surfaceTextureListener = null
        lastWindowPtr = 0L
    }

    override fun onSurfaceTextureAvailable(surface: SurfaceTexture, width: Int, height: Int) {
        notifySurfaceReady(Surface(surface))
    }

    override fun onSurfaceTextureSizeChanged(surface: SurfaceTexture, width: Int, height: Int) {
        if (width > 0 && height > 0) {
            notifySurfaceReady(Surface(surface))
        }
    }

    override fun onSurfaceTextureDestroyed(surface: SurfaceTexture): Boolean {
        lastWindowPtr = 0L
        return true
    }

    override fun onSurfaceTextureUpdated(surface: SurfaceTexture) {}

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
