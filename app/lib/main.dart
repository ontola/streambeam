import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import 'package:tray_manager/tray_manager.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:window_manager/window_manager.dart';

import 'beam_page.dart';
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

/// The shared top bar (logo + "StreamBeam" + GitHub link), identical on every
/// tab so the header doesn't change as you switch between Watch and Beam.
AppBar streamBeamAppBar() {
  return AppBar(
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
  );
}

/// True while a tab is showing fullscreen content (the video player), so the
/// [AppShell] hides its tab bar and the stream fills the screen.
final ValueNotifier<bool> appImmersive = ValueNotifier<bool>(false);

/// Brings the window to front when the tray icon is clicked. The embedded
/// host (see `app/rust/src/host.rs`) runs on the Dart isolate's background
/// thread regardless of window visibility, so this is purely a "show me the
/// UI" action, not a start/stop control.
class _TrayClickHandler with TrayListener {
  void register() => trayManager.addListener(this);

  @override
  void onTrayIconMouseUp() {
    unawaited(windowManager.show());
    unawaited(windowManager.focus());
  }
}

final _trayClickHandler = _TrayClickHandler();

Future<void> _initTray() async {
  await trayManager.setIcon('assets/tray_icon.ico');
  await trayManager.setToolTip('StreamBeam');
  _trayClickHandler.register();
}

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  if (Platform.isWindows || Platform.isLinux || Platform.isMacOS) {
    await windowManager.ensureInitialized();
    const windowOptions = WindowOptions(
      size: Size(480, 720),
      minimumSize: Size(400, 600),
      center: true,
      title: 'StreamBeam',
    );
    windowManager.waitUntilReadyToShow(windowOptions, () async {
      await windowManager.show();
      await windowManager.focus();
    });
  }
  // Windows-only for now: the tray icon asset is a Windows .ico, and the
  // legacy win32 tray (crates/host/src/tray.rs, --legacy-tray) is what this
  // is meant to replace there. Worth revisiting for Linux/macOS separately.
  if (Platform.isWindows) {
    await _initTray();
  }
  await StreambeamFfi.init();
  final dir = await streambeamDataDir();
  setAppDataDir(dir: dir);
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
      home: const AppShell(),
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
  static const _discoveryChannel = MethodChannel('streambeam/discovery');
  static const _discoveryInterval = Duration(seconds: 8);
  static const _clipboardPollInterval = Duration(seconds: 2);

  final _payloadCtrl = TextEditingController();
  bool _busy = false;
  bool _showVideo = false;
  bool _reconnectPending = false;
  String? _status;
  bool _statusIsError = false;
  int? _pendingWindow;
  bool _surfaceReady = false;
  SavedHostInfo? _savedHostInfo;
  List<DiscoveredHost> _lanHosts = [];
  DiscoveredHost? _pendingLanHost;
  String? _pendingLanAddr;
  bool _discoveryBusy = false;
  String? _discoveryError;
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
    setState(() {
      _savedHostInfo = loadSavedHostInfo();
    });
  }

  bool _pubkeysEqual(List<int> a, List<int> b) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (a[i] != b[i]) return false;
    }
    return true;
  }

  List<_HostListEntry> _hostEntries() {
    final entries = <_HostListEntry>[];
    final matchedLan = <int>{};

    final saved = _savedHostInfo;
    if (saved != null) {
      DiscoveredHost? lanMatch;
      int? lanIndex;
      for (var i = 0; i < _lanHosts.length; i++) {
        final h = _lanHosts[i];
        if (h.name == saved.name ||
            (saved.hostPubkey.length == 32 &&
                h.hostPubkey.length == 32 &&
                _pubkeysEqual(saved.hostPubkey, h.hostPubkey))) {
          lanMatch = h;
          lanIndex = i;
          break;
        }
      }
      if (lanIndex != null) matchedLan.add(lanIndex);
      entries.add(
        _HostListEntry(
          name: saved.name,
          addrs: lanMatch?.addrs ?? saved.addrs,
          port: lanMatch?.port ?? saved.port,
          hostPubkey: saved.hostPubkey.isNotEmpty
              ? saved.hostPubkey
              : (lanMatch?.hostPubkey ?? const []),
          online: lanMatch != null,
          isSaved: true,
          discovered: lanMatch,
        ),
      );
    }

    for (var i = 0; i < _lanHosts.length; i++) {
      if (matchedLan.contains(i)) continue;
      final h = _lanHosts[i];
      entries.add(
        _HostListEntry(
          name: h.name,
          addrs: h.addrs,
          port: h.port,
          hostPubkey: h.hostPubkey,
          online: true,
          isSaved: false,
          discovered: h,
        ),
      );
    }

    return entries;
  }

  Future<void> _connectHostEntry(_HostListEntry entry) async {
    if (_busy) return;
    if (!entry.online) {
      _reportError('${entry.name} is offline.');
      return;
    }
    final discovered = entry.discovered;
    if (discovered != null) {
      await _connectLanHost(discovered);
      return;
    }
    if (entry.isSaved) {
      await _beginSession(reconnect: true);
    }
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
    unawaited(_ensureMdnsLock());
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
    unawaited(_releaseMdnsLock());
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
    appImmersive.value = false;
    _reconnectPending = false;
    _surfaceReady = false;
    _pendingWindow = null;
    _playInFlight = false;
  }

  Future<void> _ensureMdnsLock() async {
    if (!Platform.isAndroid) return;
    try {
      await _discoveryChannel.invokeMethod<void>('acquireMulticastLock');
    } catch (_) {
      // Best-effort; discovery may still work on some devices.
    }
  }

  Future<void> _releaseMdnsLock() async {
    if (!Platform.isAndroid) return;
    try {
      await _discoveryChannel.invokeMethod<void>('releaseMulticastLock');
    } catch (_) {}
  }

  Future<void> _discoverHosts() async {
    if (_discoveryBusy) return;
    if (mounted) {
      setState(() {
        _discoveryBusy = true;
        _discoveryError = null;
      });
    }
    try {
      await _ensureMdnsLock();
      final hosts = await discoverHosts(timeoutMs: 6000);
      if (!mounted) return;
      setState(() {
        _lanHosts = hosts;
        _discoveryBusy = false;
      });
    } catch (e) {
      if (mounted) {
        setState(() {
          _discoveryBusy = false;
          _discoveryError = e.toString();
        });
      }
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
      appImmersive.value = true;
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
    for (final addr in host.addrs) {
      if (!addr.startsWith('127.') && addr != '::1') {
        return '$addr:${host.port}';
      }
    }
    return '${host.addrs.first}:${host.port}';
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
      appBar: streamBeamAppBar(),
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
                // Devices first — the whole point of the screen.
                _DevicesSection(
                  busy: _busy,
                  hosts: _hostEntries(),
                  discovering: _discoveryBusy,
                  discoveryError: _discoveryError,
                  onHostTap: _busy
                      ? null
                      : (h) => unawaited(_connectHostEntry(h)),
                  onForgetHost: _busy
                      ? null
                      : (_) => unawaited(_forgetSavedHost()),
                ),
                if (_status != null) ...[
                  const SizedBox(height: 12),
                  _StatusNotice(
                    message: _status!,
                    isError: _statusIsError,
                    onCopy: () => unawaited(_copyStatus()),
                    onDismiss: _statusIsError ? _dismissStatus : null,
                  ),
                ],
                const SizedBox(height: 16),
                // Add a host by QR or pasted code.
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
                      Expanded(
                        child: FilledButton.tonalIcon(
                          onPressed: _busy ? null : _toggleQrPreview,
                          icon: Icon(
                            _qrPreviewOpen
                                ? Icons.close
                                : Icons.qr_code_scanner,
                          ),
                          label: Text(_qrPreviewOpen ? 'Hide scanner' : 'Scan QR'),
                        ),
                      ),
                    ],
                  ],
                ),
                const SizedBox(height: 24),
                // Onboarding / "get the host" info, de-emphasised below.
                _StreamFromPromo(compact: _hostEntries().isNotEmpty),
                const SizedBox(height: 20),
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

class _HostListEntry {
  const _HostListEntry({
    required this.name,
    required this.addrs,
    required this.port,
    required this.hostPubkey,
    required this.online,
    required this.isSaved,
    this.discovered,
  });

  final String name;
  final List<String> addrs;
  final int port;
  final List<int> hostPubkey;
  final bool online;
  final bool isSaved;
  final DiscoveredHost? discovered;

  String get addressLabel {
    if (addrs.isEmpty) return online ? 'On LAN' : 'Not on LAN';
    final ips = addrs.join(', ');
    return online ? '$ips:$port' : 'Last seen at $ips:$port';
  }
}

/// The saved + discovered PCs, shown at the TOP of the Watch screen — the
/// primary thing you came here to do.
class _DevicesSection extends StatelessWidget {
  const _DevicesSection({
    required this.busy,
    required this.hosts,
    required this.discovering,
    this.discoveryError,
    this.onHostTap,
    this.onForgetHost,
  });

  final bool busy;
  final List<_HostListEntry> hosts;
  final bool discovering;
  final String? discoveryError;
  final void Function(_HostListEntry host)? onHostTap;
  final void Function(_HostListEntry host)? onForgetHost;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final onlineCount = hosts.where((h) => h.online).length;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Row(
          children: [
            Text('Your devices', style: theme.textTheme.titleMedium),
            const Spacer(),
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
            Text(
              discovering
                  ? 'Searching…'
                  : hosts.isEmpty
                      ? 'None found'
                      : '$onlineCount online · ${hosts.length} listed',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ],
        ),
        const SizedBox(height: 10),
        if (hosts.isEmpty)
          _EmptyDevices(discovering: discovering)
        else
          ...hosts.map(
            (h) => Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: _HostCard(
                host: h,
                busy: busy,
                onTap: onHostTap,
                onForget: onForgetHost,
              ),
            ),
          ),
        if (discoveryError != null) ...[
          const SizedBox(height: 4),
          Text(
            discoveryError!,
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.error,
            ),
          ),
        ],
        if (hosts.isNotEmpty && hosts.any((h) => h.hostPubkey.length != 32)) ...[
          const SizedBox(height: 4),
          Text(
            'Update StreamBeam on that device for tap-to-connect.',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
        ],
        if (busy) ...[
          const SizedBox(height: 16),
          const Center(child: CircularProgressIndicator()),
        ],
      ],
    );
  }
}

/// One saved/discovered PC. Online hosts are highlighted and tappable.
class _HostCard extends StatelessWidget {
  const _HostCard({
    required this.host,
    required this.busy,
    this.onTap,
    this.onForget,
  });

  final _HostListEntry host;
  final bool busy;
  final void Function(_HostListEntry host)? onTap;
  final void Function(_HostListEntry host)? onForget;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final canTap = onTap != null && host.online;
    return Material(
      color: host.online
          ? theme.colorScheme.primaryContainer.withValues(alpha: 0.45)
          : theme.colorScheme.surfaceContainerHighest,
      borderRadius: BorderRadius.circular(12),
      clipBehavior: Clip.antiAlias,
      child: ListTile(
        enabled: canTap,
        leading: Icon(
          Icons.computer,
          color: host.online
              ? theme.colorScheme.primary
              : theme.colorScheme.onSurfaceVariant,
        ),
        title: Text(host.name),
        subtitle: Text(host.addressLabel),
        trailing: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            _HostStatusChip(online: host.online),
            if (host.isSaved && onForget != null)
              IconButton(
                icon: const Icon(Icons.delete_outline),
                tooltip: 'Forget ${host.name}',
                onPressed: busy ? null : () => onForget!(host),
              )
            else if (canTap)
              Icon(Icons.play_circle_fill, color: theme.colorScheme.primary),
          ],
        ),
        onTap: canTap ? () => onTap!(host) : null,
      ),
    );
  }
}

/// Shown in place of the device list when nothing has been found yet.
class _EmptyDevices extends StatelessWidget {
  const _EmptyDevices({required this.discovering});

  final bool discovering;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(vertical: 24, horizontal: 16),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(12),
      ),
      child: Column(
        children: [
          Icon(
            Icons.wifi_find,
            size: 36,
            color: theme.colorScheme.onSurfaceVariant,
          ),
          const SizedBox(height: 12),
          Text(
            discovering
                ? 'Searching your Wi‑Fi for devices…'
                : 'No devices found yet',
            style: theme.textTheme.bodyMedium,
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 4),
          Text(
            'Run StreamBeam on another device (PC or Mac) and keep both on the '
            'same Wi‑Fi (not guest). Pull down to refresh, or scan its QR below.',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
            textAlign: TextAlign.center,
          ),
        ],
      ),
    );
  }
}

/// "Get the host" onboarding, shown BELOW the device list: a full hero when no
/// PCs are listed yet, or a slim reminder once some are.
class _StreamFromPromo extends StatelessWidget {
  const _StreamFromPromo({required this.compact});

  final bool compact;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    if (compact) {
      return Row(
        children: [
          Icon(
            Icons.info_outline,
            size: 18,
            color: theme.colorScheme.onSurfaceVariant,
          ),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              'Don\'t see your device? Make sure StreamBeam is running on it, '
              'on the same Wi‑Fi.',
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ),
          TextButton(
            onPressed: openGithubReleases,
            child: const Text('Get app'),
          ),
        ],
      );
    }

    return Container(
      padding: const EdgeInsets.symmetric(vertical: 28, horizontal: 16),
      decoration: BoxDecoration(
        color: theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(12),
      ),
      child: Column(
        children: [
          Icon(
            Icons.cast_connected,
            size: 56,
            color: theme.colorScheme.primary,
          ),
          const SizedBox(height: 16),
          Text(
            'Stream from another device',
            style: theme.textTheme.titleMedium,
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 8),
          Text(
            'Install StreamBeam on the device you want to watch — a PC or Mac — '
            'and run it. It shows up here on the same Wi‑Fi, or scan its QR to '
            'pair.',
            style: theme.textTheme.bodyMedium?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 16),
          OutlinedButton.icon(
            onPressed: openGithubReleases,
            icon: const Icon(Icons.download),
            label: const Text('Get StreamBeam for desktop'),
          ),
        ],
      ),
    );
  }
}

class _HostStatusChip extends StatelessWidget {
  const _HostStatusChip({required this.online});

  final bool online;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final bg = online
        ? theme.colorScheme.primaryContainer
        : theme.colorScheme.surfaceContainerHighest;
    final fg = online
        ? theme.colorScheme.onPrimaryContainer
        : theme.colorScheme.onSurfaceVariant;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        color: bg,
        borderRadius: BorderRadius.circular(12),
      ),
      child: Text(
        online ? 'Online' : 'Offline',
        style: theme.textTheme.labelSmall?.copyWith(color: fg),
      ),
    );
  }
}
