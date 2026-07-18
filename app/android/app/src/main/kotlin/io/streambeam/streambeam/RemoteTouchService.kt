package io.streambeam.streambeam

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.graphics.Path
import android.os.Handler
import android.os.Looper
import android.os.Bundle
import android.os.Build
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo

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
    private var shiftDown = false

    private val poll = object : Runnable {
        override fun run() {
            if (!dispatching) NativeStream.nativeTakeTouchFrame()?.let(::dispatchFrame)
            repeat(8) {
                NativeStream.nativeTakeKeyEvent()?.let(::applyKey) ?: return@repeat
            }
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
        shiftDown = false
    }

    private fun applyKey(event: IntArray) {
        if (event.size < 2) return
        val vk = event[0]
        val down = event[1] != 0
        if (vk == 0x10 || vk == 0xA0 || vk == 0xA1) {
            shiftDown = down
            return
        }
        if (!down) return
        val node = rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT) ?: return
        when (vk) {
            0x08 -> replaceSelection(node, "", deleteBefore = true)
            0x0D -> {
                if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R ||
                    !node.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.id)
                ) {
                    replaceSelection(node, "\n")
                }
            }
            0x25 -> moveSelection(node, -1)
            0x27 -> moveSelection(node, 1)
            0x2E -> replaceSelection(node, "", deleteAfter = true)
            else -> vkText(vk, shiftDown)?.let { replaceSelection(node, it) }
        }
    }

    private fun replaceSelection(
        node: AccessibilityNodeInfo,
        insertion: String,
        deleteBefore: Boolean = false,
        deleteAfter: Boolean = false,
    ) {
        val current = node.text?.toString().orEmpty()
        var start = node.textSelectionStart.takeIf { it >= 0 } ?: current.length
        var end = node.textSelectionEnd.takeIf { it >= 0 } ?: start
        if (start > end) {
            val swap = start
            start = end
            end = swap
        }
        if (start == end && deleteBefore && start > 0) start--
        if (start == end && deleteAfter && end < current.length) end++
        val next = current.replaceRange(start, end, insertion)
        val cursor = start + insertion.length
        val setText = Bundle().apply {
            putCharSequence(
                AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE,
                next,
            )
        }
        if (node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, setText)) {
            val selection = Bundle().apply {
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, cursor)
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, cursor)
            }
            node.performAction(AccessibilityNodeInfo.ACTION_SET_SELECTION, selection)
        }
    }

    private fun moveSelection(node: AccessibilityNodeInfo, delta: Int) {
        val length = node.text?.length ?: 0
        val cursor = ((node.textSelectionEnd.takeIf { it >= 0 } ?: length) + delta)
            .coerceIn(0, length)
        val selection = Bundle().apply {
            putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, cursor)
            putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, cursor)
        }
        node.performAction(AccessibilityNodeInfo.ACTION_SET_SELECTION, selection)
    }

    private fun vkText(vk: Int, shift: Boolean): String? {
        if (vk in 0x41..0x5A) {
            val char = ('a'.code + vk - 0x41).toChar()
            return (if (shift) char.uppercaseChar() else char).toString()
        }
        if (vk in 0x30..0x39) {
            val plain = "0123456789"[vk - 0x30]
            val shifted = ")!@#$%^&*("[vk - 0x30]
            return (if (shift) shifted else plain).toString()
        }
        val pair = when (vk) {
            0x20 -> ' ' to ' '
            0xBA -> ';' to ':'
            0xBB -> '=' to '+'
            0xBC -> ',' to '<'
            0xBD -> '-' to '_'
            0xBE -> '.' to '>'
            0xBF -> '/' to '?'
            0xC0 -> '`' to '~'
            0xDB -> '[' to '{'
            0xDC -> '\\' to '|'
            0xDD -> ']' to '}'
            0xDE -> '\'' to '"'
            else -> return null
        }
        return (if (shift) pair.second else pair.first).toString()
    }
}
