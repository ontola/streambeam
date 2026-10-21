import 'src/rust/api.dart';

/// Apple platforms have native Bonjour; Windows and Linux use Rust mDNS.
/// Android retries with NSD when the multicast browser returns no hosts.
Future<List<DiscoveredHost>> discoverHostsOnPlatform({
  required String operatingSystem,
  required Future<List<DiscoveredHost>> Function() rustDiscovery,
  required Future<List<DiscoveredHost>> Function() nativeDiscovery,
}) async {
  if (operatingSystem == 'macos' || operatingSystem == 'ios') {
    return nativeDiscovery();
  }
  final hosts = await rustDiscovery();
  if (operatingSystem == 'android' && hosts.isEmpty) {
    return nativeDiscovery();
  }
  return hosts;
}
