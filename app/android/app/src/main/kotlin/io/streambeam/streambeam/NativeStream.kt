package io.streambeam.streambeam

import android.view.Surface

object NativeStream {
    init {
        System.loadLibrary("rust_lib_streambeam")
    }

    // Playback (watching): wrap a Java Surface as an ANativeWindow* for the decoder.
    external fun nativeWindowFromSurface(surface: Surface): Long

    // Hosting (beaming): push MediaCodec-encoded screen frames into the host encoder.
    external fun nativeBeginScreenSession(width: Int, height: Int)
    external fun nativeSubmitScreenFrame(
        data: ByteArray,
        keyframe: Boolean,
        isConfig: Boolean,
        width: Int,
        height: Int,
    )
    external fun nativeEndScreenSession()

    /** True when a client asked for a keyframe; ask MediaCodec for a sync frame. */
    external fun nativeTakeIdrRequest(): Boolean

    /** True once a paired viewer has requested this screen. */
    external fun nativeCaptureRequested(): Boolean

    /** `[refW, refH, count, id, x, y, pressure, ...]`, or null while idle. */
    external fun nativeTakeTouchFrame(): IntArray?
    external fun nativeTakeKeyEvent(): IntArray?
    external fun nativeSetTouchInjectionEnabled(enabled: Boolean)

    // Pairing approval (raised as a notification while the app is backgrounded).
    /** Pending pairing request as "id|clientName", or null. */
    external fun nativePollApprovalRequest(): String?
    external fun nativeRespondPairing(id: Long, allow: Boolean)
}
