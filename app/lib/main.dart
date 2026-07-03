import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import 'package:path_provider/path_provider.dart';
import 'package:url_launcher/url_launcher.dart';

import 'src/rust/api.dart';
import 'src/rust/frb_generated.dart';
import 'stream_player.dart';

final _githubRepoUrl = Uri.parse('https://github.com/joepio/streambeam');
final _githubReleasesUrl = Uri.parse('https://github.com/joepio/streambeam/releases');
final _ontolaUrl = Uri.parse('https://ontola.io');
const _logoAsset = 'assets/streambeam-logo.svg';

Future<void> openGithubRepo() async {
  await launchUrl(_githubRepoUrl, mode: LaunchMode.externalApplication);
}

Future<void> openGithubReleases() async {
  await launchUrl(_githubReleasesUrl, mode: LaunchMode.externalApplication);
}

Future<void> openOntola() async {
  await launchUrl(_ontolaUrl, mode: LaunchMode.externalApplication);
}

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

class _WatchPageState extends State<WatchPage> with WidgetsBindingObserver {
  static const _surfaceChannel = MethodChannel('streambeam/video_surface');
  static const _deepLinkChannel = MethodChannel('streambeam/deeplink');
  static const _discoveryInterval = Duration(seconds: 12);
  static const _clipboardPollInterval = Duration(seconds: 2);

  final _payloadCtrl = TextEditingController();
  bool _busy = false;
  bool _showVideo = false;
  bool _reconnectPending = false;
  String? _status;
  bool _statusIsError = false;
  int? _pendingWindow;
  bool _surfaceReady = false;
  String? _savedHost;
  List<DiscoveredHost> _lanHosts = [];
  DiscoveredHost? _pendingLanHost;
  String? _pendingLanAddr;
  bool _discoveryBusy = false;
  Timer? _discoveryTimer;
  late final MobileScannerController _scannerController;
  String? _lastQrPayload;
  bool _qrPreviewOpen = false;
  bool _clipboardReady = false;
  Timer? _clipboardTimer;
  Timer? _surfaceTimeout;
  int _sessionId = 0;
  bool _playInFlight = false;
  int _streamWidth = 640;
  int _streamHeight = 360;

  void _setStatus(String message, {bool error = false}) {
    if (!mounted) return;
    setState(() {
      _status = message;
      _statusIsError = error;
    });
  }

  void _reportError(String message) {
    _setStatus(_humanizeError(message), error: true);
  }

  void _dismissStatus() {
    if (!mounted) return;
    setState(() {
      _status = null;
      _statusIsError = false;
    });
  }

  void _rememberStreamSize(StreamStats stats) {
    if (stats.width > 0 && stats.height > 0) {
      _streamWidth = stats.width;
      _streamHeight = stats.height;
    }
  }

  double get _streamAspectRatio => _streamWidth / _streamHeight;

  bool _sessionAlive(int sessionId) => mounted && sessionId == _sessionId;

  String _humanizeError(String raw) {
    var m = raw.trim();
    const legacyPrefix = 'bad pairing payload:';
    if (m.toLowerCase().startsWith(legacyPrefix)) {
      m = m.substring(legacyPrefix.length).trim();
    }
    if (m.startsWith('Error: ')) {
      m = m.substring('Error: '.length);
    }

    final lower = m.toLowerCase();
    if (lower.contains('truncated') ||
        lower.contains('too short') ||
        lower.contains('incomplete') ||
        lower.contains('cut off')) {
      return 'Pairing code incomplete. Scan the QR on your PC, or copy the entire sbm1: line from the terminal.';
    }
    if (lower.contains('missing sbm1') || lower.contains('invalid base64')) {
      return 'That is not a valid pairing code. Use sbm1: from streambeam-host on your PC, or scan the QR.';
    }
    if (lower.contains('expired')) {
      return 'Pairing code expired. Restart streambeam-host on your PC and scan the new QR.';
    }
    if (lower.contains('errorunknown') || lower.contains('video decoder')) {
      return m;
    }
    if (lower.contains('could not reach host') || lower.contains('connect failed')) {
      return 'Could not reach your PC. Check that streambeam-host is running and both devices are on the same Wi‑Fi.';
    }
    return m;
  }

  Future<void> _copyStatus() async {
    final text = _status;
    if (text == null) return;
    await Clipboard.setData(ClipboardData(text: text));
  }

  void _cancelSurfaceTimeout() {
    _surfaceTimeout?.cancel();
    _surfaceTimeout = null;
  }

  void _armSurfaceTimeout() {
    _cancelSurfaceTimeout();
    _surfaceTimeout = Timer(const Duration(seconds: 12), () {
      if (!mounted || !_busy || _pendingWindow != null) return;
      _reportError('Video surface did not start. Try again.');
      setState(() {
        _teardownVideo();
        _busy = false;
      });
      unawaited(_syncScanner());
    });
  }

  void _refreshSavedHost() {
    setState(() => _savedHost = savedHostLabel());
  }

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _refreshSavedHost();
    _scannerController = MobileScannerController(
      autoStart: false,
      detectionSpeed: DetectionSpeed.noDuplicates,
      facing: CameraFacing.back,
    );
    unawaited(_discoverHosts());
    unawaited(_refreshClipboard());
    _clipboardTimer = Timer.periodic(_clipboardPollInterval, (_) {
      if (!_busy && mounted) unawaited(_refreshClipboard());
    });
    _discoveryTimer = Timer.periodic(_discoveryInterval, (_) {
      if (!_busy && mounted) {
        unawaited(_discoverHosts());
      }
    });
    _deepLinkChannel.setMethodCallHandler((call) async {
      if (call.method == 'link') {
        await _handleIncomingLink(call.arguments as String?);
      }
    });
    // Link that cold-started the app (QR scanned before the app was running).
    unawaited(
      _deepLinkChannel
          .invokeMethod<String>('getInitialLink')
          .then(_handleIncomingLink)
          .catchError((_) {}),
    );
    _surfaceChannel.setMethodCallHandler((call) async {
      if (call.method == 'surfaceReady') {
        _cancelSurfaceTimeout();
        final window = call.arguments as int?;
        setState(() {
          _pendingWindow = window;
          _surfaceReady = window != null;
        });
        if (_busy && window != null && !_playInFlight) {
          _playInFlight = true;
          if (_reconnectPending) {
            unawaited(_startReconnect(window));
          } else {
            unawaited(_startPlayback(window));
          }
        }
      }
    });
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _discoveryTimer?.cancel();
    _clipboardTimer?.cancel();
    _cancelSurfaceTimeout();
    _scannerController.dispose();
    _payloadCtrl.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed && !_busy) {
      unawaited(_refreshClipboard());
    }
  }

  Future<void> _refreshClipboard() async {
    final payload = await _payloadFromClipboard();
    final ready = payload != null;
    if (!mounted || ready == _clipboardReady) return;
    setState(() => _clipboardReady = ready);
  }

  Future<void> _syncScanner() async {
    if (!Platform.isAndroid) return;
    try {
      if (_qrPreviewOpen && !_busy) {
        await _scannerController.start();
      } else {
        await _scannerController.stop();
      }
    } catch (_) {
      // Camera may be unavailable briefly while streaming stops.
    }
  }

  Future<void> _toggleQrPreview() async {
    if (_busy) return;
    setState(() => _qrPreviewOpen = !_qrPreviewOpen);
    await _syncScanner();
  }

  void _closeQrPreview() {
    if (!_qrPreviewOpen) return;
    setState(() => _qrPreviewOpen = false);
    unawaited(_syncScanner());
  }

  void _handleQrDetect(BarcodeCapture capture) {
    if (_busy) return;
    for (final b in capture.barcodes) {
      final raw = b.rawValue;
      if (raw == null) continue;
      final payload = _extractPayload(raw);
      if (payload == null || payload.length < 48) continue;
      if (payload == _lastQrPayload) continue;
      _lastQrPayload = payload;
      _closeQrPreview();
      unawaited(_applyPayload(payload));
      return;
    }
  }

  /// Pulls the `sbm1:` pairing code out of raw QR text, a
  /// streambeam.eu/session/#… link, or a streambeam:// link.
  String? _extractPayload(String raw) {
    final normalized = _normalizePayload(raw);
    if (normalized.startsWith('sbm1:')) return normalized;
    return RegExp(r'sbm1:[A-Za-z0-9_-]+').firstMatch(normalized)?.group(0);
  }

  Future<void> _handleIncomingLink(String? link) async {
    if (link == null || _busy || !mounted) return;
    final payload = _extractPayload(link);
    if (payload == null || payload.length < 48) {
      _reportError(
        'That link has no pairing code. Scan the QR on your PC for a fresh one.',
      );
      return;
    }
    if (payload == _lastQrPayload) return;
    _lastQrPayload = payload;
    _closeQrPreview();
    await _applyPayload(payload);
  }

  void _teardownVideo() {
    _showVideo = false;
    _reconnectPending = false;
    _surfaceReady = false;
    _pendingWindow = null;
    _playInFlight = false;
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

  Future<void> _applyPayload(String normalized) async {
    try {
      _payloadCtrl.text = normalized;
      await saveHostFromPayload(payload: normalized);
      _refreshSavedHost();
      if (!mounted) return;
      await _play();
    } catch (e) {
      _reportError(e.toString());
    }
  }

  Future<String?> _payloadFromClipboard() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text;
    if (text == null || text.isEmpty) return null;
    return _extractPayload(text);
  }

  Future<void> _pasteFromClipboard() async {
    if (_busy) return;
    try {
      final payload = await _payloadFromClipboard();
      if (payload == null) {
        _reportError('Clipboard empty. Copy sbm1: from streambeam-host on your PC, or scan the QR.');
        return;
      }
      if (payload.length < 48) {
        _reportError(
          'Pairing code incomplete. Copy the entire sbm1: line from your PC, or scan the QR.',
        );
        return;
      }
      await _applyPayload(payload);
      await _refreshClipboard();
    } catch (e) {
      _reportError(e.toString());
    }
  }

  Future<void> _beginSession({required bool reconnect}) async {
    if (!Platform.isAndroid) {
      _reportError('Playback is Android-only.');
      return;
    }
    _cancelSurfaceTimeout();
    ++_sessionId;
    setState(() {
      _busy = true;
      _showVideo = true;
      _reconnectPending = reconnect;
      _surfaceReady = false;
      _pendingWindow = null;
      _playInFlight = false;
      _qrPreviewOpen = false;
      _status = reconnect ? 'Reconnecting…' : 'Connecting…';
      _statusIsError = false;
    });
    unawaited(_syncScanner());
    _armSurfaceTimeout();
    if (_surfaceReady && _pendingWindow != null && !_playInFlight) {
      _cancelSurfaceTimeout();
      _playInFlight = true;
      if (reconnect) {
        unawaited(_startReconnect(_pendingWindow!));
      } else {
        unawaited(_startPlayback(_pendingWindow!));
      }
    }
  }

  Future<void> _startPlayback(int nativeWindow) async {
    final sessionId = _sessionId;
    final lanHost = _pendingLanHost;
    final lanAddr = _pendingLanAddr;
    _pendingLanHost = null;
    _pendingLanAddr = null;
    try {
      if (!_sessionAlive(sessionId)) return;
      setState(() {
        _status = 'Streaming…';
        _statusIsError = false;
      });
      final StreamStats stats;
      if (lanHost != null && lanAddr != null) {
        stats = await playDiscoveredHost(
          hostName: lanHost.name,
          hostPubkey: lanHost.hostPubkey,
          addr: lanAddr,
          nativeWindow: nativeWindow,
          maxVideoFrames: 0,
        );
      } else {
        final payload = _normalizePayload(_payloadCtrl.text);
        if (payload != _payloadCtrl.text) {
          _payloadCtrl.text = payload;
        }
        stats = await playStream(
          payload: payload,
          nativeWindow: nativeWindow,
          maxVideoFrames: 0,
        );
      }
      if (!_sessionAlive(sessionId)) return;
      _rememberStreamSize(stats);
      _refreshSavedHost();
      setState(() {
        _status =
            'Stopped — ${stats.videoFrames} frames, ${stats.audioPackets} audio packets';
        _statusIsError = false;
        _teardownVideo();
      });
    } catch (e) {
      if (!_sessionAlive(sessionId)) return;
      setState(() {
        _status = _humanizeError(e.toString());
        _statusIsError = true;
        _teardownVideo();
      });
    } finally {
      _cancelSurfaceTimeout();
      if (_sessionAlive(sessionId)) {
        setState(() {
          _busy = false;
          _playInFlight = false;
        });
        unawaited(_syncScanner());
      }
    }
  }

  Future<void> _startReconnect(int nativeWindow) async {
    final sessionId = _sessionId;
    try {
      if (!_sessionAlive(sessionId)) return;
      setState(() {
        _status = 'Streaming…';
        _statusIsError = false;
      });
      final stats = await reconnectPlayStream(
        nativeWindow: nativeWindow,
        maxVideoFrames: 0,
      );
      if (!_sessionAlive(sessionId)) return;
      _rememberStreamSize(stats);
      setState(() {
        _status =
            'Stopped — ${stats.videoFrames} frames, ${stats.audioPackets} audio packets';
        _statusIsError = false;
        _teardownVideo();
      });
    } catch (e) {
      if (!_sessionAlive(sessionId)) return;
      setState(() {
        _status = _humanizeError(e.toString());
        _statusIsError = true;
        _teardownVideo();
      });
    } finally {
      _cancelSurfaceTimeout();
      if (_sessionAlive(sessionId)) {
        setState(() {
          _busy = false;
          _playInFlight = false;
        });
        unawaited(_syncScanner());
      }
    }
  }

  Future<void> _play() async {
    final payload = _normalizePayload(_payloadCtrl.text);
    if (payload != _payloadCtrl.text) {
      _payloadCtrl.text = payload;
    }
    if (!payload.startsWith('sbm1:')) {
      _reportError('No pairing code found. Copy sbm1: from streambeam-host or scan the QR.');
      return;
    }
    if (payload.length < 48) {
      _reportError(
        'Pairing code incomplete. Copy the entire sbm1: line from your PC, or scan the QR.',
      );
      return;
    }
    await _beginSession(reconnect: false);
  }

  Future<void> _forgetSavedHost() async {
    if (_busy) return;
    try {
      await forgetSavedHost();
      _refreshSavedHost();
      if (!mounted) return;
      setState(() {
        if (_status != null && !_statusIsError) {
          _status = null;
        }
      });
    } catch (e) {
      _reportError(e.toString());
    }
  }

  Future<void> _connectLanHost(DiscoveredHost host) async {
    if (_busy) return;
    if (host.hostPubkey.length != 32) {
      _reportError(
        '${host.name} does not support LAN connect yet. Update streambeam-host on your PC, then try again.',
      );
      return;
    }
    final addr = _bestLanAddr(host);
    if (addr == null) {
      _reportError('No network address for ${host.name}.');
      return;
    }
    _pendingLanHost = host;
    _pendingLanAddr = addr;
    await _beginSession(reconnect: false);
  }

  String? _bestLanAddr(DiscoveredHost host) {
    if (host.addrs.isEmpty) return null;
    return '${host.addrs.first}:${host.port}';
  }

  Future<void> _watch() async {
    if (_savedHost == null) return;
    await _beginSession(reconnect: true);
  }

  void _stopStreaming() {
    _cancelSurfaceTimeout();
    ++_sessionId;
    setState(() {
      _teardownVideo();
      _busy = false;
      _status = 'Stopped';
      _statusIsError = false;
    });
    unawaited(_syncScanner());
  }

  String _normalizePayload(String raw) {
    return raw.replaceAll(RegExp(r'\s+'), '');
  }

  @override
  Widget build(BuildContext context) {
    if (_showVideo && Platform.isAndroid) {
      return StreamPlayerScreen(
        status: _status,
        statusIsError: _statusIsError,
        isStreaming: _status == 'Streaming…',
        aspectRatio: _streamAspectRatio,
        streamWidth: _streamWidth,
        streamHeight: _streamHeight,
        onStop: _stopStreaming,
      );
    }

    return Scaffold(
      appBar: AppBar(
        title: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            ClipRRect(
              borderRadius: BorderRadius.circular(8),
              child: SvgPicture.asset(_logoAsset, width: 28, height: 28),
            ),
            const SizedBox(width: 10),
            const Text('StreamBeam'),
          ],
        ),
        actions: [
          TextButton.icon(
            onPressed: openGithubRepo,
            icon: const Icon(Icons.open_in_new, size: 18),
            label: const Text('GitHub'),
          ),
        ],
      ),
      body: SafeArea(
        child: RefreshIndicator(
          onRefresh: () async {
            await Future.wait([_discoverHosts(), _refreshClipboard()]);
          },
          child: SingleChildScrollView(
            physics: const AlwaysScrollableScrollPhysics(),
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                _IdleHero(
                  busy: _busy,
                  hosts: _lanHosts,
                  discovering: _discoveryBusy,
                  savedHost: _savedHost,
                  onHostTap: _busy
                      ? null
                      : (h) => unawaited(_connectLanHost(h)),
                ),
                const SizedBox(height: 20),
                if (Platform.isAndroid && _qrPreviewOpen) ...[
                  _QrScanPreview(
                    controller: _scannerController,
                    paused: _busy,
                    onDetect: _handleQrDetect,
                  ),
                  const SizedBox(height: 8),
                ],
                Row(
                  children: [
                    Expanded(
                      child: _clipboardReady
                          ? FilledButton.icon(
                              onPressed: _busy ? null : _pasteFromClipboard,
                              icon: const Icon(Icons.content_paste),
                              label: const Text('Paste & play'),
                            )
                          : OutlinedButton.icon(
                              onPressed: null,
                              icon: const Icon(Icons.content_paste_outlined),
                              label: const Text('Paste'),
                            ),
                    ),
                    if (Platform.isAndroid) ...[
                      const SizedBox(width: 8),
                      IconButton.filledTonal(
                        onPressed: _busy ? null : _toggleQrPreview,
                        tooltip: _qrPreviewOpen
                            ? 'Hide QR scanner'
                            : 'Scan host QR',
                        isSelected: _qrPreviewOpen,
                        icon: const Icon(Icons.qr_code_scanner),
                      ),
                    ],
                  ],
                ),
                if (_savedHost != null) ...[
                  const SizedBox(height: 8),
                  Row(
                    children: [
                      Expanded(
                        child: FilledButton.icon(
                          onPressed: _busy ? null : _watch,
                          icon: const Icon(Icons.play_arrow),
                          label: Text('Watch $_savedHost'),
                        ),
                      ),
                      const SizedBox(width: 4),
                      IconButton(
                        onPressed: _busy ? null : () => unawaited(_forgetSavedHost()),
                        tooltip: 'Forget $_savedHost',
                        icon: const Icon(Icons.delete_outline),
                      ),
                    ],
                  ),
                ],
                if (_status != null) ...[
                  const SizedBox(height: 12),
                  _StatusNotice(
                    message: _status!,
                    isError: _statusIsError,
                    onCopy: () => unawaited(_copyStatus()),
                    onDismiss: _statusIsError ? _dismissStatus : null,
                  ),
                ],
                const SizedBox(height: 24),
                Center(
                  child: TextButton(
                    onPressed: openOntola,
                    style: TextButton.styleFrom(
                      foregroundColor:
                          Theme.of(context).colorScheme.onSurfaceVariant,
                      textStyle: Theme.of(context).textTheme.bodySmall,
                    ),
                    child: const Text('Made with <3 by Ontola.io'),
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _StatusNotice extends StatelessWidget {
  const _StatusNotice({
    required this.message,
    required this.isError,
    required this.onCopy,
    this.onDismiss,
  });

  final String message;
  final bool isError;
  final VoidCallback onCopy;
  final VoidCallback? onDismiss;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final fg = isError
        ? theme.colorScheme.onErrorContainer
        : theme.colorScheme.onSurfaceVariant;

    return Material(
      color: isError
          ? theme.colorScheme.errorContainer
          : theme.colorScheme.surfaceContainerHighest,
      borderRadius: BorderRadius.circular(8),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(12, 12, 4, 12),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (isError) ...[
              Icon(Icons.error_outline, color: fg, size: 20),
              const SizedBox(width: 8),
            ],
            Expanded(
              child: SelectableText(
                message,
                style: theme.textTheme.bodyMedium?.copyWith(color: fg),
              ),
            ),
            if (isError) ...[
              IconButton(
                onPressed: onCopy,
                tooltip: 'Copy error',
                icon: Icon(Icons.copy, size: 20, color: fg),
                visualDensity: VisualDensity.compact,
              ),
              if (onDismiss != null)
                IconButton(
                  onPressed: onDismiss,
                  tooltip: 'Dismiss',
                  icon: Icon(Icons.close, size: 20, color: fg),
                  visualDensity: VisualDensity.compact,
                ),
            ],
          ],
        ),
      ),
    );
  }
}

class _QrScanPreview extends StatelessWidget {
  const _QrScanPreview({
    required this.controller,
    required this.onDetect,
    required this.paused,
  });

  final MobileScannerController controller;
  final void Function(BarcodeCapture capture) onDetect;
  final bool paused;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return ClipRRect(
      borderRadius: BorderRadius.circular(12),
      child: SizedBox(
        height: 128,
        child: Stack(
          fit: StackFit.expand,
          children: [
            MobileScanner(
              controller: controller,
              onDetect: paused ? null : onDetect,
              fit: BoxFit.cover,
            ),
            if (paused)
              const ColoredBox(
                color: Color(0x88000000),
                child: Center(
                  child: Icon(Icons.videocam, color: Colors.white70, size: 32),
                ),
              ),
            Positioned(
              left: 8,
              bottom: 8,
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: Colors.black54,
                  borderRadius: BorderRadius.circular(4),
                ),
                child: Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: 8,
                    vertical: 4,
                  ),
                  child: Text(
                    paused ? 'Scanner paused while streaming' : 'Point at host QR',
                    style: theme.textTheme.labelSmall?.copyWith(
                      color: Colors.white,
                    ),
                  ),
                ),
              ),
            ),
          ],
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
    this.savedHost,
    this.onHostTap,
  });

  final bool busy;
  final List<DiscoveredHost> hosts;
  final bool discovering;
  final String? savedHost;
  final void Function(DiscoveredHost host)? onHostTap;

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
            savedHost != null
                ? 'Tap a host below or Watch to reconnect.'
                : 'Run streambeam-host on your PC, then tap a host on your LAN or scan the QR.',
            style: theme.textTheme.bodyMedium?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 16),
          OutlinedButton.icon(
            onPressed: openGithubReleases,
            icon: const Icon(Icons.download),
            label: const Text('Download StreamBeam for Windows'),
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
                child: Material(
                  color: theme.colorScheme.surface,
                  borderRadius: BorderRadius.circular(8),
                  child: ListTile(
                    dense: true,
                    enabled: onHostTap != null && h.hostPubkey.length == 32,
                    leading: Icon(
                      Icons.computer,
                      color: theme.colorScheme.primary,
                    ),
                    title: Text(h.name),
                    subtitle: Text('${h.addrs.join(", ")}:${h.port}'),
                    trailing: onHostTap == null
                        ? null
                        : Icon(
                            h.hostPubkey.length == 32
                                ? Icons.play_circle_outline
                                : Icons.info_outline,
                            color: theme.colorScheme.onSurfaceVariant,
                          ),
                    onTap: onHostTap == null ? null : () => onHostTap!(h),
                  ),
                ),
              ),
            ),
            const SizedBox(height: 4),
            Text(
              hosts.any((h) => h.hostPubkey.length != 32)
                  ? 'Update streambeam-host on your PC for tap-to-connect.'
                  : 'Tap a host to connect. Approve on your PC if this is the first time.',
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
