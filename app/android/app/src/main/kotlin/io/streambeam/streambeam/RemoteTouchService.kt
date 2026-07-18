package io.streambeam.streambeam

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.graphics.Path
import android.os.Handler
import android.os.Looper
import android.view.accessibility.AccessibilityEvent

/** Injects the viewing device's full touch state into this Android host. */
class RemoteTouchService : AccessibilityService() {
    private data class Point(val x: Float, val y: Float)
    private data class Active(
        val stroke: GestureDescription.StrokeDescription,
        val point: Point,
    )

    private val handler = Handler(Looper.getMainLooper())
    private var active = mutableMapOf<Int, Active>()
    private var dispatching = false

    private val poll = object : Runnable {
        override fun run() {
            if (!dispatching) NativeStream.nativeTakeTouchFrame()?.let(::dispatchFrame)
            handler.postDelayed(this, 8)
        }
    }

    override fun onServiceConnected() {
        super.onServiceConnected()
        NativeStream.nativeSetTouchInjectionEnabled(true)
        handler.removeCallbacks(poll)
        handler.post(poll)
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) = Unit
    override fun onInterrupt() = releaseAll()

    override fun onDestroy() {
        handler.removeCallbacks(poll)
        releaseAll()
        NativeStream.nativeSetTouchInjectionEnabled(false)
        super.onDestroy()
    }

    private fun dispatchFrame(frame: IntArray) {
        if (frame.size < 3) return
        val refW = frame[0].coerceAtLeast(1).toFloat()
        val refH = frame[1].coerceAtLeast(1).toFloat()
        val count = frame[2].coerceIn(0, 10)
        if (frame.size < 3 + count * 4) return
        val width = resources.displayMetrics.widthPixels.toFloat()
        val height = resources.displayMetrics.heightPixels.toFloat()
        val current = linkedMapOf<Int, Point>()
        repeat(count) { index ->
            val base = 3 + index * 4
            current[frame[base]] = Point(
                (frame[base + 1] / refW * width).coerceIn(0f, width - 1),
                (frame[base + 2] / refH * height).coerceIn(0f, height - 1),
            )
        }

        val builder = GestureDescription.Builder()
        val next = mutableMapOf<Int, Active>()
        for ((id, point) in current) {
            val path = Path()
            val previous = active[id]
            if (previous == null) {
                path.moveTo(point.x, point.y)
                val stroke = GestureDescription.StrokeDescription(path, 0, 16, true)
                builder.addStroke(stroke)
                next[id] = Active(stroke, point)
            } else {
                path.moveTo(previous.point.x, previous.point.y)
                path.lineTo(point.x, point.y)
                val stroke = previous.stroke.continueStroke(path, 0, 16, true)
                builder.addStroke(stroke)
                next[id] = Active(stroke, point)
            }
        }
        for ((id, previous) in active) {
            if (id in current) continue
            val path = Path().apply { moveTo(previous.point.x, previous.point.y) }
            builder.addStroke(previous.stroke.continueStroke(path, 0, 1, false))
        }
        if (current.isEmpty() && active.isEmpty()) return

        dispatching = true
        val accepted = dispatchGesture(
            builder.build(),
            object : GestureResultCallback() {
                override fun onCompleted(gestureDescription: GestureDescription?) {
                    active = next
                    dispatching = false
                }

                override fun onCancelled(gestureDescription: GestureDescription?) {
                    active.clear()
                    dispatching = false
                }
            },
            handler,
        )
        if (!accepted) {
            active.clear()
            dispatching = false
        }
    }

    private fun releaseAll() {
        active.clear()
        dispatching = false
    }
}
