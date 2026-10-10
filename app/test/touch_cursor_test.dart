import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/src/rust/frb_generated.dart';
import 'package:streambeam/stream_player.dart';
import 'package:streambeam/touch_cursor.dart';

class _MouseApi implements StreambeamFfiApi {
  final positions = <(int, int)>[];
  int clicks = 0;

  @override
  void crateApiSendMouseAbs({
    required int x,
    required int y,
    required int refW,
    required int refH,
  }) => positions.add((x, y));

  @override
  void crateApiSendMouseClick({required int button}) => clicks++;

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  testWidgets('idle hide waits for lift and refreshes on stationary taps', (
    tester,
  ) async {
    final cursor = TouchCursor();
    cursor.setContactActive(true);
    cursor.place(const Offset(0.3, 0.7));
    await tester.pump(const Duration(seconds: 5));
    expect(cursor.value, const Offset(0.3, 0.7));
    cursor.setContactActive(false);
    await tester.pump(const Duration(seconds: 2));
    cursor.place(cursor.position);
    await tester.pump(const Duration(seconds: 2));
    expect(cursor.value, isNotNull);
    await tester.pump(const Duration(seconds: 1));
    expect(cursor.value, isNull);
    expect(cursor.position, const Offset(0.3, 0.7));
    cursor.move(const Offset(0.1, 0));
    expect(cursor.value, const Offset(0.4, 0.7));
    cursor.dispose();
  });
  testWidgets(
    'touch trackpad draws the sent position, clamps and preserves it',
    (tester) async {
      final api = _MouseApi();
      StreambeamFfi.initMock(api: api);
      final transform = TransformationController();
      final controllerActivity = ValueNotifier<int>(0);
      Widget view({
        bool enabled = true,
        PointerMode mode = PointerMode.trackpad,
      }) => MaterialApp(
        home: StreamInputLayer(
          videoRect: const Rect.fromLTWH(0, 100, 800, 400),
          videoSize: const Size(800, 400),
          refWidth: 1600,
          refHeight: 800,
          transformController: transform,
          controllerActivity: controllerActivity,
          mode: mode,
          screenEdgeGrab: 0,
          mouseControlEnabled: enabled,
          hostAcceptsTouch: false,
          multiTouchSession: false,
          onMultiTouchSessionChanged: (_) {},
          child: const SizedBox.expand(),
        ),
      );
      await tester.pumpWidget(view());
      expect(find.byType(TouchCursorView), findsNothing);
      final finger = await tester.startGesture(const Offset(300, 300));
      await tester.pump();
      expect(api.positions.last, (800, 400));
      expect(
        tester.getTopLeft(find.byType(TouchCursorView)),
        const Offset(399, 299),
      );
      await finger.moveBy(const Offset(100, 50));
      await tester.pump(const Duration(milliseconds: 10));
      expect(api.positions.last, (1000, 500));
      expect(
        tester.getTopLeft(find.byType(TouchCursorView)),
        const Offset(499, 349),
      );
      await finger.moveBy(const Offset(1000, 1000));
      await tester.pump(const Duration(milliseconds: 10));
      expect(api.positions.last, (1600, 800));
      await finger.up();
      expect(api.clicks, 0);
      await tester.pumpWidget(view(enabled: false));
      expect(find.byType(TouchCursorView), findsNothing);
      await tester.pumpWidget(view());
      expect(
        tester.getTopLeft(find.byType(TouchCursorView)),
        const Offset(799, 499),
      );
      await tester.tapAt(const Offset(300, 300));
      expect(api.positions.last, (1600, 800));
      expect(api.clicks, 1);

      double cursorOpacity() => tester
          .widget<AnimatedOpacity>(
            find.ancestor(
              of: find.byType(TouchCursorView),
              matching: find.byType(AnimatedOpacity),
            ),
          )
          .opacity;
      await tester.pump(TouchCursor.idleDelay);
      expect(cursorOpacity(), 0);
      await tester.tapAt(const Offset(300, 300));
      await tester.pump();
      expect(cursorOpacity(), 1);
      expect(api.positions.last, (1600, 800)); // hiding retains position
      controllerActivity.value++;
      await tester.pump();
      expect(cursorOpacity(), 0);

      // A direct touch maps through zoom, and the cursor is projected back to
      // the same visible point. A stale viewport mapping would send (600, 400).
      transform.value = Matrix4.diagonal3Values(2, 2, 1);
      await tester.pumpWidget(view(mode: PointerMode.direct));
      await tester.tapAt(const Offset(300, 300));
      await tester.pump();
      expect(api.positions.last, (300, 100));
      expect(
        tester.getTopLeft(find.byType(TouchCursorView)),
        const Offset(299, 299),
      );
      await tester.pumpWidget(const SizedBox());
      transform.dispose();
      controllerActivity.dispose();
      StreambeamFfi.dispose();
    },
  );
}
