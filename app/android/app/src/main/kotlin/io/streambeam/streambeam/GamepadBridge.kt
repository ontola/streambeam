package io.streambeam.streambeam

import android.app.Activity
import android.hardware.input.InputManager
import android.util.Log
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import io.flutter.plugin.common.EventChannel
import kotlin.math.abs

private data class AxisSpec(
    val id: Int,
    val name: String,
    val invert: Boolean = false,
)

class GamepadBridge(
    private val activity: Activity,
    messenger: io.flutter.plugin.common.BinaryMessenger,
) : InputManager.InputDeviceListener {
    private val tag = "StreamBeamGamepad"
    private val sinkHolder = mutableListOf<EventChannel.EventSink>()
    private val devices = mutableMapOf<Int, InputDevice>()
    private val lastAxis = mutableMapOf<Pair<Int, String>, Float>()
    private var decorView: View? = null
    private var decorMotionListener: View.OnGenericMotionListener? = null

    private val axes = listOf(
        AxisSpec(MotionEvent.AXIS_X, "leftStickX"),
        AxisSpec(MotionEvent.AXIS_Y, "leftStickY", invert = true),
        AxisSpec(MotionEvent.AXIS_Z, "rightStickX"),
        AxisSpec(MotionEvent.AXIS_RZ, "rightStickY", invert = true),
        AxisSpec(MotionEvent.AXIS_RX, "rightStickX"),
        AxisSpec(MotionEvent.AXIS_RY, "rightStickY", invert = true),
        AxisSpec(MotionEvent.AXIS_HAT_X, "dpadX"),
        AxisSpec(MotionEvent.AXIS_HAT_Y, "dpadY", invert = true),
        AxisSpec(MotionEvent.AXIS_LTRIGGER, "leftTrigger"),
        AxisSpec(MotionEvent.AXIS_RTRIGGER, "rightTrigger"),
        AxisSpec(MotionEvent.AXIS_BRAKE, "leftTrigger"),
        AxisSpec(MotionEvent.AXIS_GAS, "rightTrigger"),
    )

    private val inputManager =
        activity.getSystemService(Activity.INPUT_SERVICE) as InputManager

    init {
        EventChannel(messenger, "streambeam/gamepad").setStreamHandler(
            object : EventChannel.StreamHandler {
                override fun onListen(arguments: Any?, events: EventChannel.EventSink?) {
                    if (events != null) {
                        sinkHolder.clear()
                        sinkHolder.add(events)
                        refreshDevices()
                        for (id in devices.keys) {
                            emit(mapOf("kind" to "connected", "gamepadId" to id.toString()))
                        }
                    }
                }

                override fun onCancel(arguments: Any?) {
                    sinkHolder.clear()
                }
            },
        )
        inputManager.registerInputDeviceListener(this, null)
        refreshDevices()
        attachDecorMotionListener()
    }

    fun dispose() {
        inputManager.unregisterInputDeviceListener(this)
        decorView?.setOnGenericMotionListener(null)
        decorView = null
        decorMotionListener = null
        sinkHolder.clear()
    }

    private fun attachDecorMotionListener() {
        val listener = View.OnGenericMotionListener { _, event ->
            if (devices.containsKey(event.deviceId)) {
                onMotionEvent(event)
            } else {
                false
            }
        }
        val decor = activity.window?.decorView
        decor?.setOnGenericMotionListener(listener)
        decorView = decor
        decorMotionListener = listener
    }

    fun onKeyEvent(event: KeyEvent): Boolean {
        if (!devices.containsKey(event.deviceId)) return false
        val name = keyName(event.keyCode) ?: return false
        emit(
            mapOf(
                "kind" to "button",
                "gamepadId" to event.deviceId.toString(),
                "name" to name,
                "value" to if (event.action == KeyEvent.ACTION_DOWN) 1.0 else 0.0,
            ),
        )
        return true
    }

    fun onMotionEvent(event: MotionEvent): Boolean {
        if (!devices.containsKey(event.deviceId)) return false
        var handled = false
        for (axis in axes) {
            val device = devices[event.deviceId] ?: continue
            if (device.motionRanges.none { it.axis == axis.id }) continue
            val raw = event.getAxisValue(axis.id) * if (axis.invert) -1f else 1f
            val key = event.deviceId to axis.name
            val last = lastAxis[key]
            if (last != null && abs(raw - last) < 0.001f) continue
            lastAxis[key] = raw
            emit(
                mapOf(
                    "kind" to "axis",
                    "gamepadId" to event.deviceId.toString(),
                    "name" to axis.name,
                    "value" to raw.toDouble(),
                ),
            )
            handled = true
        }
        return handled
    }

    private fun emit(payload: Map<String, Any>) {
        for (sink in sinkHolder) {
            activity.runOnUiThread { sink.success(payload) }
        }
    }

    private fun refreshDevices() {
        devices.clear()
        for (id in InputDevice.getDeviceIds()) {
            val device = InputDevice.getDevice(id) ?: continue
            if (isGamepad(device)) {
                devices[id] = device
            }
        }
    }

    private fun isGamepad(device: InputDevice): Boolean {
        val sources = device.sources
        return (sources and InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD ||
            (sources and InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK
    }

    override fun onInputDeviceAdded(deviceId: Int) {
        val device = InputDevice.getDevice(deviceId) ?: return
        if (!isGamepad(device)) return
        devices[deviceId] = device
        emit(mapOf("kind" to "connected", "gamepadId" to deviceId.toString()))
        Log.i(tag, "gamepad connected: ${device.name}")
    }

    override fun onInputDeviceRemoved(deviceId: Int) {
        if (devices.remove(deviceId) != null) {
            emit(mapOf("kind" to "disconnected", "gamepadId" to deviceId.toString()))
            Log.i(tag, "gamepad disconnected: $deviceId")
        }
    }

    override fun onInputDeviceChanged(deviceId: Int) {
        val device = InputDevice.getDevice(deviceId) ?: return
        if (isGamepad(device)) {
            devices[deviceId] = device
        } else {
            onInputDeviceRemoved(deviceId)
        }
    }

    private fun keyName(keyCode: Int): String? = when (keyCode) {
        KeyEvent.KEYCODE_BUTTON_A -> "a"
        KeyEvent.KEYCODE_BUTTON_B -> "b"
        KeyEvent.KEYCODE_BUTTON_X -> "x"
        KeyEvent.KEYCODE_BUTTON_Y -> "y"
        KeyEvent.KEYCODE_BUTTON_L1 -> "leftBumper"
        KeyEvent.KEYCODE_BUTTON_R1 -> "rightBumper"
        KeyEvent.KEYCODE_BUTTON_L2 -> "leftTrigger"
        KeyEvent.KEYCODE_BUTTON_R2 -> "rightTrigger"
        KeyEvent.KEYCODE_BUTTON_THUMBL -> "leftStick"
        KeyEvent.KEYCODE_BUTTON_THUMBR -> "rightStick"
        KeyEvent.KEYCODE_BUTTON_START -> "start"
        KeyEvent.KEYCODE_BUTTON_SELECT, KeyEvent.KEYCODE_BUTTON_MODE -> "back"
        KeyEvent.KEYCODE_DPAD_UP -> "dpadUp"
        KeyEvent.KEYCODE_DPAD_DOWN -> "dpadDown"
        KeyEvent.KEYCODE_DPAD_LEFT -> "dpadLeft"
        KeyEvent.KEYCODE_DPAD_RIGHT -> "dpadRight"
        else -> null
    }
}
