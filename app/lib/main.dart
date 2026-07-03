import 'package:flutter/material.dart';
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
      home: const ConnectPage(),
    );
  }
}

class ConnectPage extends StatefulWidget {
  const ConnectPage({super.key});

  @override
  State<ConnectPage> createState() => _ConnectPageState();
}

class _ConnectPageState extends State<ConnectPage> {
  final _payloadCtrl = TextEditingController();
  bool _busy = false;
  String? _status;

  @override
  void dispose() {
    _payloadCtrl.dispose();
    super.dispose();
  }

  Future<void> _connect({int frames = 30}) async {
    final payload = _payloadCtrl.text.trim();
    if (!payload.startsWith('sbm1:')) {
      setState(() => _status = 'Paste a valid sbm1: payload from fakehost');
      return;
    }
    setState(() {
      _busy = true;
      _status = 'Pairing…';
    });
    try {
      final stats = await connectAndReceive(payload: payload, maxVideoFrames: frames);
      setState(() {
        _status =
            'OK ${stats.width}x${stats.height} @ ${stats.fps} fps (${stats.codec})\n'
            '${stats.videoFrames} video / ${stats.audioPackets} audio packets';
      });
    } catch (e) {
      setState(() => _status = 'Error: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  Future<void> _scanQr() async {
    if (!mounted) return;
    final payload = await Navigator.of(context).push<String>(
      MaterialPageRoute(builder: (_) => const QrScanPage()),
    );
    if (payload != null && payload.startsWith('sbm1:')) {
      _payloadCtrl.text = payload;
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('StreamBeam'),
        actions: [
          Padding(
            padding: const EdgeInsets.only(right: 12),
            child: Center(
              child: Text(
                'v${streambeamVersion()}',
                style: Theme.of(context).textTheme.labelSmall,
              ),
            ),
          ),
        ],
      ),
      body: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            const Text(
              'Pair with fakehost (macOS dev loop). MediaCodec video lands in M3.',
            ),
            const SizedBox(height: 12),
            TextField(
              controller: _payloadCtrl,
              decoration: const InputDecoration(
                labelText: 'sbm1: pairing payload',
                border: OutlineInputBorder(),
              ),
              maxLines: 3,
            ),
            const SizedBox(height: 12),
            Row(
              children: [
                FilledButton.icon(
                  onPressed: _busy ? null : () => _connect(),
                  icon: const Icon(Icons.play_arrow),
                  label: const Text('Receive 30 frames'),
                ),
                const SizedBox(width: 8),
                OutlinedButton.icon(
                  onPressed: _busy ? null : _scanQr,
                  icon: const Icon(Icons.qr_code_scanner),
                  label: const Text('Scan QR'),
                ),
              ],
            ),
            const SizedBox(height: 16),
            if (_status != null)
              Text(_status!, style: Theme.of(context).textTheme.bodyLarge),
          ],
        ),
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
