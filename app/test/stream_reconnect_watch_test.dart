import 'package:clock/clock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/src/rust/api.dart';
import 'package:streambeam/src/rust/frb_generated.dart';
import 'package:streambeam/stream_player.dart';
import 'package:streambeam/touch_cursor.dart';

class _PlaybackApi extends StreambeamFfiApi {
  BigInt frames = BigInt.from(9000);
  bool touch = false;
  final touchFrames = <List<int>>[];
  @override
  void crateApiSendTouchFrame({
    required List<int> ids,
    required List<int> xs,
    required List<int> ys,
    required int refW,
    required int refH,
  }) {
    touchFrames.add(List.of(ids));
  }

  @override
  bool crateApiPlaybackHostAcceptsTouch() => touch;

  @override
  BigInt crateApiPlaybackFramesRendered() => frames;
  @override
  void crateApiSetAudioMuted({required bool muted}) {}
  @override
  List<PlaybackDisplay> crateApiPlaybackDisplays() => [];
  @override
  int? crateApiSelectedPlaybackDisplay() => null;
  @override
  PlaybackDiagnostics crateApiPlaybackDiagnostics() => PlaybackDiagnostics(
    width: 3840,
    height: 2160,
    fps: 30,
    codec: 'av01',
    bytesReceived: frames * BigInt.from(1000),
    framesRendered: frames,
    networkRttUs: BigInt.from(6000),
  );
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

Widget player({bool input = false, String hostPlatform = ''}) => MaterialApp(
  home: StreamPlayerScreen(
    status: 'Streaming…',
    statusIsError: false,
    isStreaming: true,
    aspectRatio: 16 / 9,
    streamWidth: 3840,
    streamHeight: 2160,
    video: const ColoredBox(color: Colors.black),
    inputSupported: input,
    hostPlatform: hostPlatform,
    touchControls: false,
    onStop: () {},
    streamMode: StreamMode.quality,
    onStreamModeChanged: (_) {},
    customSettings: const CustomStreamSettings(),
    onCustomSettingsChanged: (_) {},
    onCustomSettingsCommitted: () {},
    onDisplayChanged: () {},
  ),
);

void main() {
  late _PlaybackApi api;
  setUpAll(() {
    api = _PlaybackApi();
    StreambeamFfi.initMock(api: api);
  });
  tearDownAll(StreambeamFfi.dispose);

  testWidgets(
    'late Android negotiation switches to direct multi-touch without a cursor',
    (tester) async {
      api.touch = false;
      api.frames = BigInt.zero;
      await tester.pumpWidget(player(input: true));
      expect(
        tester
            .widget<StreamInputLayer>(find.byType(StreamInputLayer))
            .hostAcceptsTouch,
        isFalse,
      );
      api.touch = true;
      api.frames = BigInt.from(30);
      await tester.pump(const Duration(milliseconds: 500));
      expect(
        tester
            .widget<StreamInputLayer>(find.byType(StreamInputLayer))
            .hostAcceptsTouch,
        isTrue,
      );
      final area = tester.getRect(find.byType(StreamInputLayer));
      final first = await tester.startGesture(area.center, pointer: 1);
      final second = await tester.startGesture(
        area.center + const Offset(40, 0),
        pointer: 2,
      );
      await first.moveBy(const Offset(0, -30));
      expect(api.touchFrames.last.length, 2);
      await second.up();
      expect(api.touchFrames.last.length, 1);
      await first.up();
      expect(api.touchFrames.last, isEmpty);
      await tester.pump();
      expect(find.byType(TouchCursorView), findsNothing);
      await tester.pumpWidget(const SizedBox.shrink());
      api.touch = false;
    },
  );

  testWidgets('waiting for approval never reuses the previous picture', (
    tester,
  ) async {
    await withClock(Clock(() => tester.binding.clock.now()), () async {
      api.frames = BigInt.from(9000);
      await tester.pumpWidget(player());
      await tester.pump(const Duration(seconds: 6));
      expect(find.text('Stream stopped responding'), findsNothing);
      expect(find.text('Waiting for video…'), findsOneWidget);
      await tester.pump(const Duration(seconds: 7));
      expect(find.text('Stream stopped responding'), findsNothing);
      expect(find.text('Still waiting for video'), findsOneWidget);
      await tester.pumpWidget(const SizedBox.shrink());
    });
  });

  for (final observeZero in [false, true]) {
    testWidgets(
      'reconnect counter reset keeps live video visible ($observeZero)',
      (tester) async => withClock(
        Clock(() => tester.binding.clock.now()),
        () async {
          // Android mounts the player before the new Rust session resets the
          // previous stream's global frame counter.
          api.frames = BigInt.from(9000);
          await tester.pumpWidget(player());
          if (observeZero) {
            api.frames = BigInt.zero;
            await tester.pump(const Duration(milliseconds: 400));
          }
          for (var i = 1; i <= 20; i++) {
            api.frames = BigInt.from(i * 12);
            await tester.pump(const Duration(milliseconds: 400));
            expect(find.text('Stream stopped responding'), findsNothing);
          }
          // A real freeze must still be detected, and clear when video resumes.
          await tester.pump(const Duration(seconds: 6));
          expect(find.text('Stream stopped responding'), findsOneWidget);
          api.frames += BigInt.one;
          await tester.pump(const Duration(milliseconds: 400));
          expect(find.text('Stream stopped responding'), findsNothing);
          await tester.pumpWidget(const SizedBox.shrink());
        },
      ),
    );
  }
}
