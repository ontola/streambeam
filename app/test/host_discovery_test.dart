import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/host_discovery.dart';
import 'package:streambeam/src/rust/api.dart';

void main() {
  final host = DiscoveredHost(
    name: 'Host',
    port: 47989,
    addrs: ['192.168.0.1'],
    hostPubkey: Uint8List(32),
    platform: 'windows',
  );

  for (final platform in ['windows', 'linux']) {
    test('$platform discovers hosts without a native channel', () async {
      final found = await discoverHostsOnPlatform(
        operatingSystem: platform,
        rustDiscovery: () async => [host],
        nativeDiscovery: () => throw StateError('No native channel'),
      );
      expect(found, [host]);
    });
  }

  for (final platform in ['macos', 'ios']) {
    test('$platform uses native Bonjour', () async {
      final found = await discoverHostsOnPlatform(
        operatingSystem: platform,
        rustDiscovery: () => throw StateError('Raw mDNS is unavailable'),
        nativeDiscovery: () async => [host],
      );
      expect(found, [host]);
    });
  }

  test('Android falls back to NSD only for an empty Rust result', () async {
    var nativeCalls = 0;
    Future<List<DiscoveredHost>> native() async {
      nativeCalls++;
      return [host];
    }

    expect(
      await discoverHostsOnPlatform(
        operatingSystem: 'android',
        rustDiscovery: () async => [host],
        nativeDiscovery: native,
      ),
      [host],
    );
    expect(nativeCalls, 0);
    expect(
      await discoverHostsOnPlatform(
        operatingSystem: 'android',
        rustDiscovery: () async => [],
        nativeDiscovery: native,
      ),
      [host],
    );
    expect(nativeCalls, 1);
  });
}
