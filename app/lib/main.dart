import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import 'package:path_provider/path_provider.dart';

import 'src/rust/api.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await StreambeamFfi.init();
  final dir = await getApplicationDocumentsDirectory();
  setAppDataDir(dir: dir.path);
  runApp(const StreamBeamApp());
}

class StreamBeamApp extends StatelessWidget {
  const StreamBeamApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'StreamBeam',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.indigo),
        useMaterial3: true,
      ),
      home: const WatchPage(),
    );
  }
}

/// Android: MediaCodec → Surface. Pair with Windows/macOS host via QR.
class WatchPage extends StatefulWidget {
  const WatchPage({super.key});

  @override
  State<WatchPage> createState() => _WatchPageState();
}

class _WatchPageState extends State<WatchPage> {
  static const _surfaceChannel = MethodChannel('streambeam/video_surface');
  static const _discoveryInterval = Duration(seconds: 12);

  final _payloadCtrl = TextEditingController();
  bool _busy = false;
  bool _showVideo = false;
  bool _reconnectPending = false;
  String? _status;
  int? _pendingWindow;
  bool _surfaceReady = false;
  String? _savedHost;
  List<DiscoveredHost> _lanHosts = [];
  bool _discoveryBusy = false;
  Timer? _discoveryTimer;

  void _refreshSavedHost() {
    setState(() => _savedHost = savedHostLabel());
  }

  @override
  void initState() {
    super.initState();
    _refreshSavedHost();
    unawaited(_discoverHosts());
    _discoveryTimer = Timer.periodic(_discoveryInterval, (_) {
      if (!_busy && mounted) {
        unawaited(_discoverHosts());
      }
    });
    _surfaceChannel.setMethodCallHandler((call) async {
      if (call.method == 'surfaceReady') {
        setState(() {
          _pendingWindow = call.arguments as int?;
          _surfaceReady = _pendingWindow != null;
        });
        if (_busy && _pendingWindow != null) {
          if (_reconnectPending) {
            unawaited(_startReconnect(_pendingWindow!));
          } else {
            unawaited(_startPlayback(_pendingWindow!));
          }
        }
      }
    });
  }

  @override
  void dispose() {
    _discoveryTimer?.cancel();
    _payloadCtrl.dispose();
    super.dispose();
  }

  void _teardownVideo() {
    _showVideo = false;
    _reconnectPending = false;
    _surfaceReady = false;
    _pendingWindow = null;
  }

  Future<void> _discoverHosts() async {
    if (_discoveryBusy) return;
    if (mounted) setState(() => _discoveryBusy = true);
    try {
      final hosts = await discoverHosts(timeoutMs: 2500);
      if (!mounted) return;
      setState(() {
        _lanHosts = hosts;
        _discoveryBusy = false;
      });
    } catch (_) {
      if (mounted) setState(() => _discoveryBusy = false);
    }
  }

  Future<void> _applyPayload(String normalized, {bool autoPlay = false}) async {
    _payloadCtrl.text = normalized;
    await saveHostFromPayload(payload: normalized);
    _refreshSavedHost();
    if (!mounted) return;
    if (autoPlay) {
      await _play();
    } else {
      setState(() => _status = 'Paired — tap Play');
    }
  }

  Future<String?> _payloadFromClipboard() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text;
    if (text == null || text.isEmpty) return null;
    final normalized = _normalizePayload(text);
    if (normalized.startsWith('sbm1:')) return normalized;
    final match = RegExp(r'sbm1:[A-Za-z0-9_-]+').firstMatch(normalized);
    return match?.group(0);
  }

  Future<void> _pasteFromClipboard() async {
    if (_busy) return;
    final payload = await _payloadFromClipboard();
    if (payload == null) {
      setState(() => _status = 'Clipboard empty or missing sbm1: payload');
      return;
    }
    if (payload.length < 48) {
      setState(() => _status =
          'sbm1: in clipboard looks truncated — copy the full line from the host');
      return;
    }
    await _applyPayload(payload, autoPlay: true);
  }

  Future<void> _beginSession({required bool reconnect}) async {
    if (!Platform.isAndroid) {
      setState(() => _status = 'Playback is Android-only.');
      return;
    }
    setState(() {
      _busy = true;
      _showVideo = true;
      _reconnectPending = reconnect;
      _surfaceReady = false;
      _pendingWindow = null;
      _status = reconnect ? 'Reconnecting…' : 'Connecting…';
    });
    if (_surfaceReady && _pendingWindow != null) {
      if (reconnect) {
        unawaited(_startReconnect(_pendingWindow!));
      } else {
        unawaited(_startPlayback(_pendingWindow!));
      }
    }
  }

  Future<void> _startPlayback(int nativeWindow) async {
    final payload = _normalizePayload(_payloadCtrl.text);
    if (payload != _payloadCtrl.text) {
      _payloadCtrl.text = payload;
    }
    try {
      setState(() => _status = 'Streaming…');
      final stats = await playStream(
        payload: payload,
        nativeWindow: nativeWindow,
        maxVideoFrames: 0,
      );
      _refreshSavedHost();
      if (!mounted) return;
      setState(() {
        _status =
            'Stopped — ${stats.videoFrames} frames, ${stats.audioPackets} audio packets';
        _teardownVideo();
      });
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _status = 'Error: $e';
        _teardownVideo();
      });
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _startReconnect(int nativeWindow) async {
    try {
      setState(() => _status = 'Streaming…');
      final stats = await reconnectPlayStream(
        nativeWindow: nativeWindow,
        maxVideoFrames: 0,
      );
      if (!mounted) return;
      setState(() {
        _status =
            'Stopped — ${stats.videoFrames} frames, ${stats.audioPackets} audio packets';
        _teardownVideo();
      });
    } catch (e) {
      if (!mounted) return;
      setState(() {
        _status = 'Error: $e';
        _teardownVideo();
      });
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _play() async {
    final payload = _normalizePayload(_payloadCtrl.text);
    if (payload != _payloadCtrl.text) {
      _payloadCtrl.text = payload;
    }
    if (!payload.startsWith('sbm1:')) {
      setState(() => _status = 'Scan the QR from streambeam-host on your PC');
      return;
    }
    if (payload.length < 48) {
      setState(() => _status =
          'sbm1: payload looks truncated — copy the full single line from the host');
      return;
    }
    await _beginSession(reconnect: false);
  }

  Future<void> _reconnect() async {
    if (_savedHost == null) return;
    await _beginSession(reconnect: true);
  }

  Future<void> _scanQr() async {
    final payload = await Navigator.of(context).push<String>(
      MaterialPageRoute(builder: (_) => const QrScanPage()),
    );
    if (payload != null) {
      final normalized = _normalizePayload(payload);
      if (!normalized.startsWith('sbm1:')) {
        if (mounted) {
          setState(() => _status = 'QR did not contain sbm1: — scan the host terminal QR');
        }
        return;
      }
      _payloadCtrl.text = normalized;
      await saveHostFromPayload(payload: normalized);
      _refreshSavedHost();
      if (mounted) {
        setState(() => _status = 'Paired — tap Play');
      }
    }
  }

  String _normalizePayload(String raw) {
    return raw.replaceAll(RegExp(r'\s+'), '');
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return Scaffold(
      appBar: AppBar(
        title: const Text('StreamBeam'),
        actions: [
          if (_savedHost != null)
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: Chip(
                avatar: const Icon(Icons.check_circle, size: 18),
                label: Text(_savedHost!, style: theme.textTheme.labelMedium),
              ),
            ),
        ],
      ),
      body: SafeArea(
        child: RefreshIndicator(
          onRefresh: _discoverHosts,
          child: SingleChildScrollView(
            physics: const AlwaysScrollableScrollPhysics(),
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                if (_showVideo && Platform.isAndroid)
                ClipRRect(
                  borderRadius: BorderRadius.circular(12),
                  child: AspectRatio(
                    aspectRatio: 16 / 9,
                    child: Stack(
                      fit: StackFit.expand,
                      children: [
                        AndroidView(
                          viewType: 'streambeam/video',
                          layoutDirection: TextDirection.ltr,
                        ),
                        if (_busy && _status != 'Streaming…')
                          ColoredBox(
                            color: Colors.black54,
                            child: Center(
                              child: Column(
                                mainAxisSize: MainAxisSize.min,
                                children: [
                                  const CircularProgressIndicator(
                                    color: Colors.white,
                                  ),
                                  const SizedBox(height: 12),
                                  Text(
                                    _status ?? 'Connecting…',
                                    style: theme.textTheme.bodyMedium?.copyWith(
                                      color: Colors.white,
                                    ),
                                  ),
                                ],
                              ),
                            ),
                          ),
                      ],
                    ),
                  ),
                )
              else
                _IdleHero(
                  busy: _busy,
                  hosts: _lanHosts,
                  discovering: _discoveryBusy,
                ),
              const SizedBox(height: 20),
              FilledButton.icon(
                onPressed: _busy ? null : _scanQr,
                icon: const Icon(Icons.qr_code_scanner),
                label: const Text('Scan host QR'),
              ),
              const SizedBox(height: 8),
              OutlinedButton.icon(
                onPressed: _busy ? null : _pasteFromClipboard,
                icon: const Icon(Icons.content_paste),
                label: const Text('Paste & play'),
              ),
              const SizedBox(height: 8),
              Row(
                children: [
                  Expanded(
                    child: FilledButton.tonalIcon(
                      onPressed: _busy ? null : _play,
                      icon: const Icon(Icons.play_arrow),
                      label: const Text('Play'),
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: OutlinedButton.icon(
                      onPressed: (_busy || _savedHost == null) ? null : _reconnect,
                      icon: const Icon(Icons.link),
                      label: const Text('Reconnect'),
                    ),
                  ),
                ],
              ),
              if (_status != null && !_showVideo) ...[
                const SizedBox(height: 12),
                Material(
                  color: theme.colorScheme.surfaceContainerHighest,
                  borderRadius: BorderRadius.circular(8),
                  child: Padding(
                    padding: const EdgeInsets.all(12),
                    child: Text(_status!),
                  ),
                ),
              ],
            ],
          ),
        ),
      ),
    ),
    );
  }
}

class _IdleHero extends StatelessWidget {
  const _IdleHero({
    required this.busy,
    required this.hosts,
    required this.discovering,
  });

  final bool busy;
  final List<DiscoveredHost> hosts;
  final bool discovering;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return Container(
      padding: const EdgeInsets.symmetric(vertical: 32, horizontal: 16),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(12),
      ),
      child: Column(
        children: [
          Icon(
            Icons.cast_connected,
            size: 64,
            color: theme.colorScheme.primary,
          ),
          const SizedBox(height: 16),
          Text(
            'Stream from your PC',
            style: theme.textTheme.titleMedium,
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 8),
          Text(
            'Copy sbm1: from the host terminal, paste, or scan QR.',
            style: theme.textTheme.bodyMedium?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 16),
          Row(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              if (discovering)
                Padding(
                  padding: const EdgeInsets.only(right: 8),
                  child: SizedBox(
                    width: 14,
                    height: 14,
                    child: CircularProgressIndicator(
                      strokeWidth: 2,
                      color: theme.colorScheme.primary,
                    ),
                  ),
                ),
              Icon(
                Icons.wifi_find,
                size: 18,
                color: theme.colorScheme.onSurfaceVariant,
              ),
              const SizedBox(width: 6),
              Text(
                hosts.isEmpty
                    ? 'Searching LAN for hosts…'
                    : '${hosts.length} host(s) on LAN',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
            ],
          ),
          if (hosts.isNotEmpty) ...[
            const SizedBox(height: 12),
            ...hosts.map(
              (h) => Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text(
                  '${h.name}  ${h.addrs.join(", ")}:${h.port}',
                  style: theme.textTheme.bodySmall,
                  textAlign: TextAlign.center,
                ),
              ),
            ),
            const SizedBox(height: 4),
            Text(
              'Scan QR on the host to pair.',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
              textAlign: TextAlign.center,
            ),
          ],
          if (busy) ...[
            const SizedBox(height: 20),
            const CircularProgressIndicator(),
          ],
        ],
      ),
    );
  }
}

class QrScanPage extends StatefulWidget {
  const QrScanPage({super.key});

  @override
  State<QrScanPage> createState() => _QrScanPageState();
}

class _QrScanPageState extends State<QrScanPage> {
  bool _done = false;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Scan host QR')),
      body: MobileScanner(
        onDetect: (capture) {
          if (_done) return;
          for (final b in capture.barcodes) {
            final raw = b.rawValue;
            if (raw != null && raw.startsWith('sbm1:')) {
              _done = true;
              Navigator.pop(context, raw);
              return;
            }
          }
        },
      ),
    );
  }
}
