import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import 'src/rust/api.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await StreambeamFfi.init();
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
      home: const HomePage(),
    );
  }
}

class HomePage extends StatefulWidget {
  const HomePage({super.key});

  @override
  State<HomePage> createState() => _HomePageState();
}

class _HomePageState extends State<HomePage> {
  int _tab = 0;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('StreamBeam')),
      body: _tab == 0 ? const WatchPage() : const StatsPage(),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _tab,
        onDestinationSelected: (i) => setState(() => _tab = i),
        destinations: const [
          NavigationDestination(icon: Icon(Icons.play_circle), label: 'Watch'),
          NavigationDestination(icon: Icon(Icons.analytics), label: 'Stats'),
        ],
      ),
    );
  }
}

/// Android: MediaCodec → Surface. macOS/desktop: instructions for visual-test.sh.
class WatchPage extends StatefulWidget {
  const WatchPage({super.key});

  @override
  State<WatchPage> createState() => _WatchPageState();
}

class _WatchPageState extends State<WatchPage> {
  static const _surfaceChannel = MethodChannel('streambeam/video_surface');

  final _payloadCtrl = TextEditingController();
  bool _busy = false;
  String? _status;
  int? _pendingWindow;
  bool _surfaceReady = false;

  @override
  void initState() {
    super.initState();
    _surfaceChannel.setMethodCallHandler((call) async {
      if (call.method == 'surfaceReady') {
        setState(() {
          _pendingWindow = call.arguments as int?;
          _surfaceReady = _pendingWindow != null;
        });
        if (_busy && _pendingWindow != null) {
          await _startPlayback(_pendingWindow!);
        }
      }
    });
  }

  @override
  void dispose() {
    _payloadCtrl.dispose();
    super.dispose();
  }

  Future<void> _startPlayback(int nativeWindow) async {
    final payload = _payloadCtrl.text.trim();
    try {
      final stats = await playStream(
        payload: payload,
        nativeWindow: nativeWindow,
        maxVideoFrames: 120,
      );
      setState(() {
        _status =
            'Played ${stats.videoFrames} video / ${stats.audioPackets} audio';
      });
    } catch (e) {
      setState(() => _status = 'Error: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _play() async {
    final payload = _payloadCtrl.text.trim();
    if (!payload.startsWith('sbm1:')) {
      setState(() => _status = 'Paste a valid sbm1: payload from fakehost');
      return;
    }
    if (!Platform.isAndroid) {
      setState(() {
        _status =
            'Visual test on Mac:\n  ./scripts/visual-test.sh\n\n'
            '(starts fakehost + ffplay window)';
      });
      return;
    }
    setState(() {
      _busy = true;
      _status = 'Waiting for surface…';
    });
    if (_surfaceReady && _pendingWindow != null) {
      await _startPlayback(_pendingWindow!);
    }
  }

  Future<void> _scanQr() async {
    final payload = await Navigator.of(context).push<String>(
      MaterialPageRoute(builder: (_) => const QrScanPage()),
    );
    if (payload != null && payload.startsWith('sbm1:')) {
      _payloadCtrl.text = payload;
    }
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (Platform.isAndroid)
            AspectRatio(
              aspectRatio: 16 / 9,
              child: AndroidView(
                viewType: 'streambeam/video',
                layoutDirection: TextDirection.ltr,
              ),
            )
          else
            Container(
              height: 180,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                border: Border.all(color: Theme.of(context).dividerColor),
                borderRadius: BorderRadius.circular(8),
              ),
              child: const Text('Video preview on Android\nMac → ./scripts/visual-test.sh'),
            ),
          const SizedBox(height: 12),
          TextField(
            controller: _payloadCtrl,
            decoration: const InputDecoration(
              labelText: 'sbm1: pairing payload',
              border: OutlineInputBorder(),
            ),
            maxLines: 2,
          ),
          const SizedBox(height: 8),
          Row(
            children: [
              FilledButton.icon(
                onPressed: _busy ? null : _play,
                icon: const Icon(Icons.play_arrow),
                label: const Text('Play'),
              ),
              const SizedBox(width: 8),
              OutlinedButton.icon(
                onPressed: _scanQr,
                icon: const Icon(Icons.qr_code_scanner),
                label: const Text('Scan'),
              ),
            ],
          ),
          if (_status != null) ...[
            const SizedBox(height: 12),
            Text(_status!),
          ],
        ],
      ),
    );
  }
}

class StatsPage extends StatefulWidget {
  const StatsPage({super.key});

  @override
  State<StatsPage> createState() => _StatsPageState();
}

class _StatsPageState extends State<StatsPage> {
  final _payloadCtrl = TextEditingController();
  bool _busy = false;
  String? _status;

  @override
  void dispose() {
    _payloadCtrl.dispose();
    super.dispose();
  }

  Future<void> _connect() async {
    final payload = _payloadCtrl.text.trim();
    if (!payload.startsWith('sbm1:')) {
      setState(() => _status = 'Paste a valid sbm1: payload');
      return;
    }
    setState(() {
      _busy = true;
      _status = 'Connecting…';
    });
    try {
      final stats = await connectAndReceive(payload: payload, maxVideoFrames: 30);
      setState(() {
        _status =
            '${stats.width}x${stats.height} @ ${stats.fps} fps\n'
            '${stats.videoFrames} video / ${stats.audioPackets} audio packets';
      });
    } catch (e) {
      setState(() => _status = 'Error: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.all(16),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text('Headless receive test (v${streambeamVersion()})'),
          const SizedBox(height: 12),
          TextField(
            controller: _payloadCtrl,
            decoration: const InputDecoration(
              labelText: 'sbm1: payload',
              border: OutlineInputBorder(),
            ),
            maxLines: 3,
          ),
          const SizedBox(height: 8),
          FilledButton(
            onPressed: _busy ? null : _connect,
            child: const Text('Receive 30 frames'),
          ),
          if (_status != null) ...[
            const SizedBox(height: 12),
            Text(_status!),
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
