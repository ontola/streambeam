import 'dart:async';

import 'package:flutter/material.dart';

/// A touch trackpad owns an absolute host position: Windows acceleration must
/// not move the remote pointer farther than the cursor drawn on the phone.
/// Normalized coordinates survive rotation and stream resolution changes.
class TouchCursor extends ValueNotifier<Offset?> {
  TouchCursor() : super(null);

  static const idleDelay = Duration(seconds: 3);
  Timer? _idleTimer;
  Offset? _lastPosition;
  bool _contactActive = false;

  Offset get position => _lastPosition ?? const Offset(0.5, 0.5);
  bool get hasPosition => _lastPosition != null;

  void setContactActive(bool active) {
    _contactActive = active;
    _scheduleHide();
  }

  void _scheduleHide() {
    _idleTimer?.cancel();
    _idleTimer = null;
    if (!_contactActive && value != null) {
      _idleTimer = Timer(idleDelay, hide);
    }
  }

  Offset place(Offset normalized) {
    final position = Offset(
      normalized.dx.clamp(0.0, 1.0),
      normalized.dy.clamp(0.0, 1.0),
    );
    _lastPosition = position;
    value = position;
    _scheduleHide();
    return position;
  }

  Offset move(Offset normalizedDelta) => place(position + normalizedDelta);

  void hide() {
    _idleTimer?.cancel();
    _idleTimer = null;
    value = null;
  }

  @override
  void dispose() {
    _idleTimer?.cancel();
    super.dispose();
  }
}

/// The top-left tip is the click location, with an outline visible on both
/// light and dark desktops. This overlay never intercepts touches.
class TouchCursorView extends StatelessWidget {
  const TouchCursorView({super.key});

  @override
  Widget build(BuildContext context) => const IgnorePointer(
    child: CustomPaint(size: Size(22, 28), painter: _CursorPainter()),
  );
}

class _CursorPainter extends CustomPainter {
  const _CursorPainter();

  @override
  void paint(Canvas canvas, Size size) {
    final arrow = Path()
      ..moveTo(1, 1)
      ..lineTo(2.5, 21)
      ..quadraticBezierTo(2.6, 22, 3.4, 21.3)
      ..lineTo(7.4, 17.6)
      ..lineTo(11.1, 25.3)
      ..quadraticBezierTo(11.4, 26, 12.1, 25.6)
      ..lineTo(14.1, 24.6)
      ..quadraticBezierTo(14.8, 24.3, 14.4, 23.6)
      ..lineTo(10.7, 16)
      ..lineTo(16.4, 15.5)
      ..quadraticBezierTo(17.5, 15.4, 16.7, 14.6)
      ..close();
    canvas.drawPath(
      arrow.shift(const Offset(0.5, 1.5)),
      Paint()
        ..color = const Color(0x66000000)
        ..maskFilter = const MaskFilter.blur(BlurStyle.normal, 1.5),
    );
    canvas.drawPath(
      arrow,
      Paint()
        ..shader = const LinearGradient(
          begin: Alignment.topLeft,
          end: Alignment.bottomRight,
          colors: [Colors.white, Color(0xFFE2E8F0)],
        ).createShader(Offset.zero & size),
    );
    canvas.drawPath(
      arrow,
      Paint()
        ..color = const Color(0xFF253044)
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.15
        ..strokeJoin = StrokeJoin.round,
    );
  }

  @override
  bool shouldRepaint(_CursorPainter oldDelegate) => false;
}
