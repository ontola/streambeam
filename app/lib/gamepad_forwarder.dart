import 'dart:async';

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import 'src/rust/api.dart';

/// XInput `wButtons` bitfield (matches Windows / VIIPER Xbox 360 layout).
class XInputButtons {
  static const dpadUp = 0x0001;
  static const dpadDown = 0x0002;
  static const dpadLeft = 0x0004;
  static const dpadRight = 0x0008;
  static const start = 0x0010;
  static const back = 0x0020;
  static const leftStick = 0x0040;
  static const rightStick = 0x0080;
  static const leftShoulder = 0x0100;
  static const rightShoulder = 0x0200;
  static const a = 0x1000;
  static const b = 0x2000;
  static const x = 0x4000;
  static const y = 0x8000;
}

/// Forwards a phone-paired gamepad to the PC host as XInput state.
class GamepadForwarder extends StatefulWidget {
  const GamepadForwarder({
    required this.enabled,
    this.paused = false,
    this.onActivity,
    super.key,
  });

  final bool enabled;

  /// Keep the virtual controller attached while menus temporarily own input.
  final bool paused;

  /// Deliberate native input only, excluding heartbeats and idle stick drift.
  final VoidCallback? onActivity;

  @override
  State<GamepadForwarder> createState() => _GamepadForwarderState();
}

class _GamepadForwarderState extends State<GamepadForwarder> {
  static const _channel = EventChannel('streambeam/gamepad');
  static const _heartbeat = Duration(milliseconds: 50);

  StreamSubscription<dynamic>? _sub;
  Timer? _heartbeatTimer;
  final _deviceToSlot = <String, int>{};
  final _states = <String, _LocalGamepadState>{};
  int _nextSlot = 0;

  @override
  void initState() {
    super.initState();
    if (widget.enabled) _start();
  }

  @override
  void didUpdateWidget(GamepadForwarder oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.enabled && !oldWidget.enabled) {
      _start();
    } else if (!widget.enabled && oldWidget.enabled) {
      _stop();
    }
    if (widget.enabled && widget.paused != oldWidget.paused) {
      _sendHeartbeats();
    }
  }

  @override
  void dispose() {
    _stop();
    super.dispose();
  }

  void _start() {
    _sub ??= _channel.receiveBroadcastStream().listen(_onNativeEvent);
    _heartbeatTimer ??= Timer.periodic(_heartbeat, (_) => _sendHeartbeats());
  }

  void _stop() {
    for (final slot in _deviceToSlot.values) {
      sendGamepadPlug(pad: slot, connected: false);
    }
    _deviceToSlot.clear();
    _states.clear();
    _nextSlot = 0;
    unawaited(_sub?.cancel());
    _sub = null;
    _heartbeatTimer?.cancel();
    _heartbeatTimer = null;
  }

  void _onNativeEvent(dynamic raw) {
    if (!widget.enabled || raw is! Map) return;
    final kind = raw['kind'] as String?;
    final gamepadId = raw['gamepadId']?.toString();
    if (gamepadId == null) return;

    switch (kind) {
      case 'connected':
        _onConnected(gamepadId);
      case 'disconnected':
        _onDisconnected(gamepadId);
      case 'button':
        final name = raw['name'] as String?;
        final value = (raw['value'] as num?)?.toDouble() ?? 0;
        if (name == null) return;
        _ensureConnected(gamepadId);
        _states[gamepadId]?.setButton(name, value >= 0.5);
        if (!widget.paused && value >= 0.5) widget.onActivity?.call();
        _sendState(gamepadId);
      case 'axis':
        final name = raw['name'] as String?;
        final value = (raw['value'] as num?)?.toDouble() ?? 0;
        if (name == null) return;
        _ensureConnected(gamepadId);
        _states[gamepadId]?.setAxis(name, value);
        if (!widget.paused && value.abs() > 0.18) widget.onActivity?.call();
        _sendState(gamepadId);
    }
  }

  void _ensureConnected(String gamepadId) {
    if (!_deviceToSlot.containsKey(gamepadId)) {
      _onConnected(gamepadId);
    }
  }

  void _onConnected(String gamepadId) {
    if (_deviceToSlot.containsKey(gamepadId) || _nextSlot > 3) return;
    final slot = _nextSlot++;
    _deviceToSlot[gamepadId] = slot;
    _states[gamepadId] = _LocalGamepadState();
    sendGamepadPlug(pad: slot, connected: true);
    _sendState(gamepadId);
  }

  void _onDisconnected(String gamepadId) {
    final slot = _deviceToSlot.remove(gamepadId);
    _states.remove(gamepadId);
    if (slot != null) sendGamepadPlug(pad: slot, connected: false);
  }

  void _sendHeartbeats() {
    if (!widget.enabled || _states.isEmpty) return;
    for (final id in _states.keys) {
      _sendState(id);
    }
  }

  void _sendState(String gamepadId) {
    final slot = _deviceToSlot[gamepadId];
    final current = _states[gamepadId];
    if (slot == null || current == null) return;
    final state = widget.paused ? _LocalGamepadState() : current;
    sendGamepadState(
      pad: slot,
      buttons: state.buttons,
      lt: state.ltByte,
      rt: state.rtByte,
      lx: state.lxShort,
      ly: state.lyShort,
      rx: state.rxShort,
      ry: state.ryShort,
    );
  }

  @override
  Widget build(BuildContext context) => const SizedBox.shrink();
}

class _LocalGamepadState {
  int buttons = 0;
  double lx = 0;
  double ly = 0;
  double rx = 0;
  double ry = 0;
  double lt = 0;
  double rt = 0;

  void setButton(String name, bool pressed) {
    switch (name) {
      case 'leftTrigger':
        lt = pressed ? 1.0 : 0.0;
        return;
      case 'rightTrigger':
        rt = pressed ? 1.0 : 0.0;
        return;
    }
    final bit = _buttonBit(name);
    if (bit == 0) return;
    if (pressed) {
      buttons |= bit;
    } else {
      buttons &= ~bit;
    }
  }

  void setAxis(String name, double value) {
    switch (name) {
      case 'leftStickX':
        lx = value;
      case 'leftStickY':
        ly = value;
      case 'rightStickX':
        rx = value;
      case 'rightStickY':
        ry = value;
      case 'leftTrigger':
        lt = value.clamp(0, 1);
      case 'rightTrigger':
        rt = value.clamp(0, 1);
      case 'dpadX':
        buttons =
            (buttons & ~(XInputButtons.dpadLeft | XInputButtons.dpadRight));
        if (value < -0.5) {
          buttons |= XInputButtons.dpadLeft;
        } else if (value > 0.5) {
          buttons |= XInputButtons.dpadRight;
        }
      case 'dpadY':
        buttons = (buttons & ~(XInputButtons.dpadUp | XInputButtons.dpadDown));
        if (value < -0.5) {
          buttons |= XInputButtons.dpadDown;
        } else if (value > 0.5) {
          buttons |= XInputButtons.dpadUp;
        }
    }
  }

  int get ltByte => (lt * 255).round().clamp(0, 255);
  int get rtByte => (rt * 255).round().clamp(0, 255);
  int get lxShort => (lx * 32767).round().clamp(-32768, 32767);
  int get lyShort => (ly * 32767).round().clamp(-32768, 32767);
  int get rxShort => (rx * 32767).round().clamp(-32768, 32767);
  int get ryShort => (ry * 32767).round().clamp(-32768, 32767);

  int _buttonBit(String name) => switch (name) {
    'a' => XInputButtons.a,
    'b' => XInputButtons.b,
    'x' => XInputButtons.x,
    'y' => XInputButtons.y,
    'leftBumper' => XInputButtons.leftShoulder,
    'rightBumper' => XInputButtons.rightShoulder,
    'back' => XInputButtons.back,
    'start' => XInputButtons.start,
    'leftStick' => XInputButtons.leftStick,
    'rightStick' => XInputButtons.rightStick,
    'dpadUp' => XInputButtons.dpadUp,
    'dpadDown' => XInputButtons.dpadDown,
    'dpadLeft' => XInputButtons.dpadLeft,
    'dpadRight' => XInputButtons.dpadRight,
    _ => 0,
  };
}
