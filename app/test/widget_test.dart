import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/main.dart';
import 'package:streambeam/src/rust/api.dart';
import 'package:streambeam/src/rust/frb_generated.dart';

// Widget tests have no native library or host services. Explicitly model the
// idle device instead of depending on a developer's built Rust dylib.
class IdleApi extends StreambeamFfiApi {
  @override
  bool crateApiHostPlatformSupported() => false;
  @override
  String? crateApiSavedHostLabel() => null;
  @override
  bool crateApiSavedHostCanWake() => false;
  @override
  SavedHostInfo? crateApiLoadSavedHostInfo() => null;
  @override
  String? crateApiTakeRemoteClipboard() => null;
  @override
  String? crateApiHostTakeRemoteClipboard() => null;
  @override
  Uint8List crateApiHostPublicKey() => Uint8List(32);
  @override
  Future<List<DiscoveredHost>> crateApiDiscoverHosts({
    required int timeoutMs,
  }) async => [];
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  setUp(() => StreambeamFfi.initMock(api: IdleApi()));
  tearDown(StreambeamFfi.dispose);

  testWidgets('app loads without native host services', (tester) async {
    await tester.pumpWidget(const StreamBeamApp());
    await tester.pump();
    expect(find.text('StreamBeam'), findsOneWidget);
    expect(tester.takeException(), isNull);
    await tester.pumpWidget(const SizedBox.shrink());
  });
}
