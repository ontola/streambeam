package io.streambeam.streambeam

import android.view.Surface

object NativeStream {
    init {
        System.loadLibrary("rust_lib_streambeam")
    }

    external fun nativeWindowFromSurface(surface: Surface): Long
}
