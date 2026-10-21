import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/main.dart';
import 'package:streambeam/src/rust/api.dart';
import 'package:streambeam/src/rust/frb_generated.dart';
import 'package:streambeam/stream_player.dart';

import 'widget_test.dart' show IdleApi;

class _PairingApi extends IdleApi {
  SavedHostInfo? saved;
  // No event is emitted until playback subscribes. A broadcast controller also
  // closes when an earlier assertion fails before that subscription exists;
  // a single-subscription controller would hang teardown and hide the failure.
  final stream = StreamController<DesktopStreamEvent>.broadcast();
  int connections = 0;

  @override
  Future<List<DiscoveredHost>> crateApiDiscoverHosts({
    required int timeoutMs,
  }) async => [
    DiscoveredHost(
      name: 'MacBook Pro',
      port: 47989,
      addrs: ['192.168.0.76'],
      hostPubkey: Uint8List.fromList(List.filled(32, 7)),
      platform: 'macos',
    ),
  ];

  @override
  SavedHostInfo? crateApiLoadSavedHostInfo() => saved;
  @override
  void crateApiStopPlayback() {}
  @override
  void crateApiSetAudioMuted({required bool muted}) {}
  @override
  BigInt crateApiPlaybackFramesRendered() => BigInt.zero;
  @override
  bool crateApiPlaybackHostAcceptsTouch() => false;
  @override
  List<PlaybackDisplay> crateApiPlaybackDisplays() => [];
  @override
  int? crateApiSelectedPlaybackDisplay() => null;
  @override
  PlaybackDiagnostics crateApiPlaybackDiagnostics() => PlaybackDiagnostics(
    width: 1920,
    height: 1080,
    fps: 30,
    codec: 'h264',
    bytesReceived: BigInt.zero,
    framesRendered: BigInt.zero,
    networkRttUs: BigInt.zero,
  );
  @override
  Stream<DesktopStreamEvent> crateApiPlayStreamDesktopKnown({
    required String hostName,
    required List<int> hostPubkey,
    required String addr,
    required bool allowAv1,
    required bool allowHdr,
    required int maxWidth,
    required int maxHeight,
    required int maxFps,
  }) {
    connections++;
    return stream.stream;
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  late _PairingApi api;
  final pubkey = Uint8List.fromList(List.filled(32, 7));
  const discovery = MethodChannel('streambeam/native_discovery');
  const video = MethodChannel('streambeam/video_desktop');
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;

  setUp(() {
    api = _PairingApi();
    StreambeamFfi.initMock(api: api);
    messenger.setMockMethodCallHandler(
      discovery,
      (call) async => call.method == 'getHosts'
          ? [
              {
                'name': 'MacBook Pro',
                'port': 47989,
                'addrs': ['192.168.0.76'],
                'pubkey': pubkey,
                'platform': 'macos',
              },
            ]
          : null,
    );
    messenger.setMockMethodCallHandler(
      video,
      (call) async => switch (call.method) {
        'configure' => {'textureId': 1, 'width': 1920, 'height': 1080},
        'supportsAv1' || 'setUpscale' => false,
        _ => null,
      },
    );
  });
  tearDown(() async {
    await api.stream.close();
    messenger.setMockMethodCallHandler(discovery, null);
    messenger.setMockMethodCallHandler(video, null);
    StreambeamFfi.dispose();
  });

  testWidgets(
    'stopping a paired stream refreshes its tile before stream completion',
    (tester) async {
      // Exercise the viewer's persistence lifecycle directly. AppShell keeps
      // the unimplemented Linux viewer behind its platform capability gate;
      // that shell policy must not prevent this mocked regression running in CI.
      await tester.pumpWidget(const MaterialApp(home: WatchPage()));
      // Native discovery crosses an asynchronous channel before starting its
      // one-second browse. Advance until its result is rendered, not exactly
      // one second after pumpWidget (which can predate that browse timer).
      for (
        var i = 0;
        i < 50 && find.text('Set up a connection').evaluate().isEmpty;
        i++
      ) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      expect(find.text('Set up a connection'), findsOneWidget);
      await tester.tap(find.text('MacBook Pro'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Request connection'));
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 300));
      expect(api.connections, 1);

      // Native playback persists the authenticated host before producing video.
      // Keep the stream open: manually stopping must not rely on its completion.
      api.saved = SavedHostInfo(
        name: 'MacBook Pro',
        port: 47989,
        addrs: ['192.168.0.76'],
        hostPubkey: pubkey,
      );
      api.stream.add(
        DesktopStreamEvent(
          isConfig: true,
          codec: 'h264',
          codecConfig: Uint8List(0),
          width: 1920,
          height: 1080,
          fps: 30,
          data: Uint8List(0),
          keyframe: true,
          hdr: false,
        ),
      );
      await tester.pump();
      await tester.pump();
      expect(find.byType(StreamPlayerScreen), findsOneWidget);
      tester
          .widget<StreamPlayerScreen>(find.byType(StreamPlayerScreen))
          .onStop();
      await tester.pump();
      expect(find.text('Set up a connection'), findsNothing);
      expect(find.text('Saved'), findsOneWidget);
      expect(find.text('Tap to start streaming'), findsOneWidget);
      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );
}
