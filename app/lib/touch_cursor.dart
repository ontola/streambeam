import 'package:flutter/material.dart';

/// A touch trackpad owns an absolute host position: Windows acceleration must
/// not move the remote pointer farther than the cursor drawn on the phone.
/// Normalized coordinates survive rotation and stream resolution changes.
class TouchCursor extends ValueNotifier<Offset?> {
  TouchCursor() : super(null);

  Offset place(Offset normalized) {
    final position = Offset(
      normalized.dx.clamp(0.0, 1.0),
      normalized.dy.clamp(0.0, 1.0),
    );
    value = position;
    return position;
  }

  Offset move(Offset normalizedDelta) =>
      place((value ?? const Offset(0.5, 0.5)) + normalizedDelta);

  void hide() => value = null;
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
      ..lineTo(1, 22)
      ..lineTo(6, 17)
      ..lineTo(11, 27)
      ..lineTo(15, 25)
      ..lineTo(10, 15)
      ..lineTo(20, 15)
      ..close();
    canvas.drawPath(arrow, Paint()..color = Colors.white);
    canvas.drawPath(
      arrow,
      Paint()
        ..color = Colors.black
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5
        ..strokeJoin = StrokeJoin.round,
    );
  }

  @override
  bool shouldRepaint(_CursorPainter oldDelegate) => false;
}
