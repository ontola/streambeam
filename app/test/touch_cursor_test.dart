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
  testWidgets(
    'touch trackpad draws the sent position, clamps and preserves it',
    (tester) async {
      final api = _MouseApi();
      StreambeamFfi.initMock(api: api);
      final transform = TransformationController();
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
      StreambeamFfi.dispose();
    },
  );
}
