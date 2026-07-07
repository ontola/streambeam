import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

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
import 'ui.dart';

final _githubRepoUrl = Uri.parse('https://github.com/joepio/streambeam');
final _githubReleasesUrl = Uri.parse('https://github.com/joepio/streambeam/releases');
final _ontolaUrl = Uri.parse('https://ontola.io');
const _logoAsset = 'assets/streambeam-logo.svg';

/// Computed the same way as `MaterialApp`'s theme below (same seed color),
/// but standalone — no `BuildContext` needed — so `main()` can also use it
/// to color the system nav bar before the first frame. Explicitly set as
/// `streamBeamAppBar`'s own background too, rather than trusting Material's
/// default elevation-tinted app bar color to land on the exact same value.
final _appBarColor = ColorScheme.fromSeed(
  seedColor: Colors.indigo,
).surfaceContainer;

Future<void> openGithubRepo() async {
  await launchUrl(_githubRepoUrl, mode: LaunchMode.externalApplication);
}

Future<void> openGithubReleases() async {
  await launchUrl(_githubReleasesUrl, mode: LaunchMode.externalApplication);
}

Future<void> openOntola() async {
  await launchUrl(_ontolaUrl, mode: LaunchMode.externalApplication);
}

Widget _appBarTitle() => Row(
  mainAxisSize: MainAxisSize.min,
  children: [
    ClipRRect(
      borderRadius: BorderRadius.circular(8),
      child: SvgPicture.asset(_logoAsset, width: 28, height: 28),
    ),
    const SizedBox(width: 10),
    const Text('StreamBeam'),
  ],
);

List<Widget> _appBarActions() => [
  TextButton.icon(
    onPressed: openGithubRepo,
    icon: const Icon(Icons.open_in_new, size: 18),
    label: const Text('GitHub'),
  ),
];

/// The shared top bar (logo + "StreamBeam" + GitHub link), identical on every
/// tab so the header doesn't change as you switch between Watch and Beam.
AppBar streamBeamAppBar() {
  return AppBar(
    backgroundColor: _appBarColor,
    title: _appBarTitle(),
    actions: _appBarActions(),
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
  await _applyDeviceName();
  if (Platform.isAndroid) {
    // Without this, the system nav bar (back/home buttons) defaults to a
    // plain white background — indistinguishable from this app's white
    // body behind it, so the (also white-ish) nav icons effectively
    // disappear. Match it to the same surface tone the app bar uses (see
    // `streamBeamAppBar`'s `backgroundColor`) so the two bars read as one
    // continuous grey strip, with dark icons for contrast against it.
    SystemChrome.setSystemUIOverlayStyle(
      SystemUiOverlayStyle(
        systemNavigationBarColor: _appBarColor,
        systemNavigationBarIconBrightness: Brightness.dark,
      ),
    );
    // Keep the system nav (back/home) visible; the fullscreen player goes
    // immersive on its own while watching.
    SystemChrome.setEnabledSystemUIMode(
      SystemUiMode.manual,
      overlays: SystemUiOverlay.values,
    );
  }
  runApp(const StreamBeamApp());
}

/// Tell the Rust core this device's name so peers see it instead of a generic
/// "StreamBeam". Android and macOS answer the `streambeam/device` channel
/// with the user-facing device name ("Xiaomi 15", "Joep's MacBook Pro");
/// platforms without a native handler fall back to the OS hostname.
Future<void> _applyDeviceName() async {
  String? name;
  try {
    name = await const MethodChannel('streambeam/device')
        .invokeMethod<String>('getName');
  } catch (_) {}
  if ((name == null || name.trim().isEmpty) && !Platform.isAndroid) {
    // gethostname often carries an mDNS suffix ("Joeps-MacBook-Pro.local").
    name = Platform.localHostname.replaceFirst(RegExp(r'\.local$'), '');
  }
  if (name != null && name.trim().isNotEmpty) {
    setDeviceName(name: name.trim());
  }
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
  MobileScannerController? _scannerController;
  String? _lastQrPayload;
  bool _qrPreviewOpen = false;
  bool _clipboardReady = false;
  Timer? _clipboardTimer;
  Timer? _surfaceTimeout;
  int _sessionId = 0;
  bool _playInFlight = false;
  StreamMode _streamMode = StreamMode.game;
  CustomStreamSettings _customSettings = const CustomStreamSettings();
  int _streamWidth = 640;
  int _streamHeight = 360;

  // Desktop (macOS) watch: Rust streams H.264 frames, a VideoToolbox decoder in
  // the native Runner turns them into a Flutter texture.
  static const _desktopVideoChannel = MethodChannel('streambeam/video_desktop');
  int? _desktopTextureId;
  double _desktopAspect = 16 / 9;
  int _desktopWidth = 0;
  int _desktopHeight = 0;
  StreamSubscription<DesktopStreamEvent>? _desktopSub;
  // MetalFX upscaling (macOS): on by default; the native side reports back
  // whether it's actually available on this machine.
  bool _upscaleEnabled = true;
  bool _upscaleActive = false;
  bool? _upscaleSupported;

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
    if (lower.contains('pair rejected')) {
      // Already specific (the host's own stated reason) — don't flatten it.
      return m;
    }
    if (lower.contains('timed out waiting for video')) {
      return 'Connected, but no video arrived. The PC\'s screen may be locked or '
          'asleep — StreamBeam needs it awake to capture — or streaming failed '
          'to start on that end. Check the host\'s logs.';
    }
    if (lower.contains('stream closed')) {
      return 'The host closed the connection. If this device is not paired yet, scan the host QR or use Paste & play with its sbm1: code.';
    }
    // Rust's describe_connect_failure (core/src/session.rs) already produces
    // a specific, actionable message per failure kind (timeout vs reset vs
    // version mismatch vs application-closed) — pass it through rather than
    // flattening every connect failure into one generic sentence like this
    // used to. Only degenerate/unrecognized text falls through to the
    // fallback `return m` below.
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
    if (Platform.isAndroid) {
      _scannerController = MobileScannerController(
        autoStart: false,
        detectionSpeed: DetectionSpeed.noDuplicates,
        facing: CameraFacing.back,
      );
    }
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
    _scannerController?.dispose();
    unawaited(_desktopSub?.cancel());
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
        await _scannerController?.start();
      } else {
        await _scannerController?.stop();
      }
    } catch (_) {
      // Camera may be unavailable briefly while streaming stops.
    }
  }

  Future<void> _toggleQrPreview() async {
    if (_busy) return;
    final opening = !_qrPreviewOpen;
    setState(() => _qrPreviewOpen = opening);
    if (opening) {
      // Start the camera only after the MobileScanner widget is actually in the
      // tree — calling start() immediately races the not-yet-attached platform
      // view, leaving the preview blank until you toggle again.
      WidgetsBinding.instance.addPostFrameCallback((_) => unawaited(_syncScanner()));
    } else {
      unawaited(_syncScanner());
    }
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
    unawaited(_setStreamingWifiLock(false));
    _showVideo = false;
    appImmersive.value = false;
    _reconnectPending = false;
    _surfaceReady = false;
    _pendingWindow = null;
    _playInFlight = false;
  }

  /// Hold Android's low-latency WifiLock while a stream is active. Without
  /// it the phone's WiFi power-save adds tens of ms of latency (measured
  /// ping RTT swinging 5-131ms) and drops bursts of large video datagrams
  /// while small audio packets survive — the "audio flows, video stuck on
  /// first frame" signature. Same lock Moonlight holds while streaming.
  Future<void> _setStreamingWifiLock(bool active) async {
    if (!Platform.isAndroid) return;
    try {
      await _discoveryChannel.invokeMethod<void>(
        active ? 'acquireStreamingWifiLock' : 'releaseStreamingWifiLock',
      );
    } catch (_) {
      // Best-effort: streaming still works without it, just worse.
    }
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

  static const _nativeDiscoveryChannel =
      MethodChannel('streambeam/native_discovery');

  /// Desktop host discovery via native Bonjour (macOS). The browser runs
  /// continuously once started, so results accumulate across refreshes.
  Future<List<DiscoveredHost>> _discoverHostsNative() async {
    await _nativeDiscoveryChannel.invokeMethod('start');
    await Future<void>.delayed(const Duration(seconds: 1));
    final raw =
        await _nativeDiscoveryChannel.invokeMethod<List<dynamic>>('getHosts');
    return (raw ?? const []).map((entry) {
      final map = entry as Map;
      final addrs = (map['addrs'] as List?) ?? const [];
      return DiscoveredHost(
        name: map['name'] as String? ?? '',
        port: map['port'] as int? ?? 0,
        addrs: addrs.map((e) => e as String).toList(),
        hostPubkey: (map['pubkey'] as Uint8List?) ?? Uint8List(0),
      );
    }).toList();
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
      final List<DiscoveredHost> hosts;
      if (Platform.isAndroid) {
        await _ensureMdnsLock();
        hosts = await discoverHosts(timeoutMs: 6000);
      } else {
        // macOS: Rust mdns-sd can't reliably receive (mDNSResponder owns 5353),
        // so browse via native Bonjour instead.
        hosts = await _discoverHostsNative();
      }
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
      if (!Platform.isAndroid) {
        await _startDesktopPlayback(normalized);
        return;
      }
      await _play();
    } catch (e) {
      _reportError(e.toString());
    }
  }

  /// Whether the platform decoder can hardware-decode AV1 (VideoToolbox on
  /// Apple silicon M3+). Probed once; advertised to the host so it only
  /// encodes AV1 for clients that can actually play it.
  bool? _desktopAv1;

  Future<bool> _desktopCanAv1() async {
    final cached = _desktopAv1;
    if (cached != null) return cached;
    bool can;
    try {
      can = await _desktopVideoChannel.invokeMethod<bool>('canDecodeAV1') ??
          false;
    } catch (_) {
      can = false;
    }
    _desktopAv1 = can;
    return can;
  }

  /// Desktop watch: drive the Rust frame stream into the native VideoToolbox
  /// decoder (macOS) and show its texture.
  Future<void> _startDesktopPlayback(String payload) async {
    await _stopDesktopPlayback();
    setState(() {
      _busy = true;
      _status = 'Connecting…';
      _statusIsError = false;
    });
    _attachDesktopStream(
      playStreamDesktop(payload: payload, allowAv1: await _desktopCanAv1()),
    );
  }

  /// Watch a discovered/saved host by its pinned key (no QR needed; the host
  /// prompts to approve on first contact).
  Future<void> _startDesktopPlaybackKnown(DiscoveredHost host, String addr) async {
    await _stopDesktopPlayback();
    setState(() {
      _busy = true;
      _status = 'Connecting…';
      _statusIsError = false;
    });
    _attachDesktopStream(playStreamDesktopKnown(
      hostName: host.name,
      hostPubkey: host.hostPubkey,
      addr: addr,
      allowAv1: await _desktopCanAv1(),
    ));
  }

  void _attachDesktopStream(Stream<DesktopStreamEvent> stream) {
    _desktopSub = stream.listen(
      (ev) async {
        if (!mounted) return;
        if (ev.isConfig) {
          try {
            final id = await _desktopVideoChannel.invokeMethod<int>(
              'configure',
              {
                'codecConfig': ev.codecConfig,
                'codec': ev.codec,
                'width': ev.width,
                'height': ev.height,
              },
            );
            if (id != null && mounted) {
              setState(() {
                _desktopTextureId = id;
                _desktopAspect = ev.height > 0 ? ev.width / ev.height : 16 / 9;
                _desktopWidth = ev.width;
                _desktopHeight = ev.height;
                _status = 'Streaming…';
                _showVideo = true;
                _busy = false;
              });
              appImmersive.value = true;
              unawaited(_applyUpscale());
            }
          } catch (e) {
            _reportError(e.toString());
          }
        } else {
          _desktopVideoChannel.invokeMethod('frame', {'data': ev.data});
        }
      },
      onError: (e) {
        _reportError(_humanizeError(e.toString()));
        unawaited(_stopDesktopPlayback());
      },
      onDone: () => unawaited(_stopDesktopPlayback()),
    );
  }

  /// Push the MetalFX preference to the native decoder; it answers whether
  /// upscaling is actually running (false on unsupported hardware).
  Future<void> _applyUpscale() async {
    try {
      final active = await _desktopVideoChannel.invokeMethod<bool>(
        'setUpscale',
        {'enabled': _upscaleEnabled},
      );
      if (!mounted) return;
      setState(() {
        _upscaleActive = active ?? false;
        // Only an enabled request is evidence about hardware support.
        if (_upscaleEnabled) _upscaleSupported = active ?? false;
      });
    } catch (_) {}
  }

  void _toggleUpscale() {
    _upscaleEnabled = !_upscaleEnabled;
    unawaited(_applyUpscale());
  }

  Future<void> _stopDesktopPlayback() async {
    // Leave the player screen immediately; the actual teardown (stream
    // cancel + native decoder dispose) continues in the background — the
    // dispose call queues behind any backlog of in-flight 'frame' messages
    // on the platform channel and can take a moment.
    final sub = _desktopSub;
    _desktopSub = null;
    appImmersive.value = false;
    if (mounted) {
      setState(() {
        _desktopTextureId = null;
        _showVideo = false;
        _busy = false;
        _upscaleActive = false;
        if (_status == 'Streaming…' || _status == 'Connecting…') _status = null;
      });
    }
    await sub?.cancel();
    try {
      await _desktopVideoChannel.invokeMethod('dispose');
    } catch (_) {}
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
      // Desktop watch: connect to the tapped host by its pinned key.
      final lanHost = _pendingLanHost;
      final lanAddr = _pendingLanAddr;
      _pendingLanHost = null;
      _pendingLanAddr = null;
      if (lanHost != null && lanAddr != null) {
        await _startDesktopPlaybackKnown(lanHost, lanAddr);
      } else {
        _setStatus('Paste or scan the host\'s link to watch on this device.');
      }
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
    unawaited(_setStreamingWifiLock(true));
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

  /// This device's real display capability — sent as `SessionRequest.video`
  /// so the host encodes at a resolution/frame rate actually worth sending
  /// instead of a fixed 1080p/60fps guess. `Display.size` is already in
  /// physical pixels (not logical/DP), matching what the host's encoder
  /// resolution means. Falls back to 0s (the Rust side substitutes its own
  /// sane default) if no display info is available yet, which shouldn't
  /// normally happen once the app's UI is actually visible and calling this.
  (int, int, int) _deviceVideoCaps() {
    final views = ui.PlatformDispatcher.instance.views;
    if (views.isEmpty) return (0, 0, 0);
    final display = views.first.display;
    // The display's raw refresh rate is the wrong signal for encode fps:
    // a 120Hz screen says nothing about whether this device's *video
    // decoder* can actually sustain 120 full decoded frames/sec at this
    // resolution — that's a much lower, separate hardware limit, and
    // asking for more than the decoder can do doesn't get skipped
    // gracefully. It makes the client permanently "behind" (every batch
    // completes multiple frames before the decoder drains one), so it
    // skips nearly all of them forever — measured on a real 120Hz device
    // at ~2670px wide: 95-99% of frames skipped, render stuck at 0-2fps,
    // despite the host delivering cleanly with zero network loss. 60fps
    // is comfortably within real-world hardware decoder throughput at
    // desktop-capture resolutions; this is a ceiling; the actual encode
    // rate still adapts to network conditions in the same way bitrate
    // does. `StreamMode` (see stream_player.dart) applies its own tighter
    // caps on top of this ceiling below.
    const maxSustainableDecodeFps = 60;
    final (rawWidth, rawHeight, rawFps) = (
      display.size.width.round(),
      display.size.height.round(),
      display.refreshRate.round().clamp(1, maxSustainableDecodeFps),
    );
    return _streamMode.apply(
      rawWidth,
      rawHeight,
      rawFps,
      customSettings: _customSettings,
    );
  }

  /// Changing mode only takes effect at connect time (it's sent once, as
  /// part of `SessionRequest.video`) — if a stream is already running,
  /// reconnect through the same saved-host path `_connectHostEntry` already
  /// uses so the new caps actually apply, rather than silently doing
  /// nothing until the user manually reconnects.
  ///
  /// Switching *to* Custom is the one exception: it has nothing to apply
  /// yet (just the current slider defaults) and reconnecting immediately
  /// tears down and remounts the whole stream screen — including the side
  /// menu the user just used to pick it — before they ever get to see or
  /// touch the sliders that selection was supposed to reveal. Reconnect
  /// happens once they actually commit a slider value instead (see
  /// `_commitCustomSettings`).
  void _changeStreamMode(StreamMode mode) {
    if (mode == _streamMode) return;
    setState(() => _streamMode = mode);
    if (mode != StreamMode.custom) _reconnectForNewCaps();
  }

  /// Live slider drag — update the displayed value only. Reconnecting on
  /// every `onChanged` tick would fire the whole stop/reconnect dance dozens
  /// of times per drag; `_commitCustomSettings` (called on slider release)
  /// is what actually applies it.
  void _updateCustomSettings(CustomStreamSettings settings) {
    setState(() => _customSettings = settings);
  }

  void _commitCustomSettings() {
    if (_streamMode == StreamMode.custom) _reconnectForNewCaps();
  }

  void _reconnectForNewCaps() {
    if (_showVideo) {
      _stopStreaming();
      // `_stopStreaming` only mutates state (`_showVideo = false` etc.) —
      // Flutter batches that with whatever runs next in the same tick, so
      // calling `_beginSession` immediately here never actually renders a
      // frame with the video surface torn down first. The native platform
      // view backing it is then never disposed and recreated, and
      // `_beginSession` waits forever for a surface-ready callback that has
      // no reason to ever fire (this is what "stuck on Reconnecting…" was).
      // A real post-frame callback, plus a short buffer for the platform
      // view's own async teardown, gives the old surface time to actually
      // go away before asking for a new one.
      WidgetsBinding.instance.addPostFrameCallback((_) {
        Future.delayed(const Duration(milliseconds: 300), () {
          if (mounted) unawaited(_beginSession(reconnect: true));
        });
      });
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
      final (maxWidth, maxHeight, maxFps) = _deviceVideoCaps();
      final StreamStats stats;
      if (lanHost != null && lanAddr != null) {
        stats = await playDiscoveredHost(
          hostName: lanHost.name,
          hostPubkey: lanHost.hostPubkey,
          addr: lanAddr,
          nativeWindow: nativeWindow,
          maxVideoFrames: 0,
          maxWidth: maxWidth,
          maxHeight: maxHeight,
          maxFps: maxFps,
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
          maxWidth: maxWidth,
          maxHeight: maxHeight,
          maxFps: maxFps,
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
      final (maxWidth, maxHeight, maxFps) = _deviceVideoCaps();
      final stats = await reconnectPlayStream(
        nativeWindow: nativeWindow,
        maxVideoFrames: 0,
        maxWidth: maxWidth,
        maxHeight: maxHeight,
        maxFps: maxFps,
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

  // Bracket an IPv6 literal for `ip:port` syntax (`fe80::1` -> `[fe80::1]`);
  // IPv4 and hostnames pass through untouched. Rust's SocketAddr parser
  // requires the brackets — "$addr:$port" with a bare IPv6 address produces
  // a string with too many colons and fails with "invalid socket address
  // syntax" (e.g. `fe80::64d5:4ff:fe62:9fa2:47989` instead of
  // `[fe80::64d5:4ff:fe62:9fa2]:47989`).
  String _hostPort(String addr, int port) {
    final isIpv6 = addr.contains(':');
    return isIpv6 ? '[$addr]:$port' : '$addr:$port';
  }

  String? _bestLanAddr(DiscoveredHost host) {
    if (host.addrs.isEmpty) return null;
    // mDNS advertises both A (IPv4) and AAAA (IPv6) records; prefer IPv4 —
    // link-local IPv6 addresses (`fe80::...`) are common on LANs and need a
    // zone/scope id to actually route that mDNS doesn't carry, so an IPv6
    // pick here is more likely to be unreachable even once correctly
    // formatted. Any non-loopback IPv4 first, then any non-loopback address,
    // then whatever's there.
    for (final addr in host.addrs) {
      if (!addr.contains(':') && !addr.startsWith('127.')) {
        return _hostPort(addr, host.port);
      }
    }
    for (final addr in host.addrs) {
      if (!addr.startsWith('127.') && addr != '::1') {
        return _hostPort(addr, host.port);
      }
    }
    return _hostPort(host.addrs.first, host.port);
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
    if (_desktopTextureId != null) {
      // Desktop watch — same player UI as Android, with the platform texture
      // injected and capabilities declared (no input back-channel yet).
      return StreamPlayerScreen(
        status: _status,
        statusIsError: _statusIsError,
        isStreaming: _status == 'Streaming…',
        aspectRatio: _desktopAspect,
        streamWidth: _desktopWidth,
        streamHeight: _desktopHeight,
        video: Texture(textureId: _desktopTextureId!),
        touchControls: false,
        upscaleSupported: _upscaleSupported ?? false,
        upscaleActive: _upscaleActive,
        onToggleUpscale: _toggleUpscale,
        onStop: () => unawaited(_stopDesktopPlayback()),
        streamMode: _streamMode,
        // Desktop watch reconnects through a different path than Android's
        // `_beginSession`/`_stopStreaming` (see `_stopDesktopPlayback` and
        // its own connect flow) — `_reconnectForNewCaps` assumes the
        // Android teardown, so only update the displayed state here rather
        // than wiring it to trigger a reconnect. Applying a new mode/custom
        // setting on desktop currently needs a manual stop + reconnect.
        onStreamModeChanged: (mode) => setState(() => _streamMode = mode),
        customSettings: _customSettings,
        onCustomSettingsChanged: (settings) =>
            setState(() => _customSettings = settings),
        onCustomSettingsCommitted: () {},
      );
    }
    if (_showVideo && Platform.isAndroid) {
      return StreamPlayerScreen(
        status: _status,
        statusIsError: _statusIsError,
        isStreaming: _status == 'Streaming…',
        aspectRatio: _streamAspectRatio,
        streamWidth: _streamWidth,
        streamHeight: _streamHeight,
        video: const AndroidView(
          viewType: 'streambeam/video',
          layoutDirection: TextDirection.ltr,
        ),
        onStop: _stopStreaming,
        streamMode: _streamMode,
        onStreamModeChanged: _changeStreamMode,
        customSettings: _customSettings,
        onCustomSettingsChanged: _updateCustomSettings,
        onCustomSettingsCommitted: _commitCustomSettings,
      );
    }

    // The header eats a big chunk of the (already scarce, especially in
    // landscape) vertical space if pinned, so it floats away on scroll down
    // and reappears on scroll up instead — see AppShell in beam_page.dart for
    // the Watch/Beam tab bar's own landscape-rail treatment.
    final scrollView = CustomScrollView(
      physics: const AlwaysScrollableScrollPhysics(),
      slivers: [
        SliverAppBar(
          backgroundColor: _appBarColor,
          title: _appBarTitle(),
          actions: _appBarActions(),
          floating: true,
          snap: true,
        ),
        SliverPadding(
          padding: const EdgeInsets.all(16),
          sliver: SliverToBoxAdapter(
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
                  StatusNotice(
                    message: _status!,
                    isError: _statusIsError,
                    onCopy: _statusIsError
                        ? () => unawaited(_copyStatus())
                        : null,
                    onDismiss: _statusIsError ? _dismissStatus : null,
                  ),
                ],
                const SizedBox(height: 16),
                // Add a host by QR or pasted code.
                if (Platform.isAndroid && _qrPreviewOpen) ...[
                  _QrScanPreview(
                    controller: _scannerController!,
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
      ],
    );

    return Scaffold(
      body: SafeArea(
        top: false,
        child: RefreshIndicator(
          onRefresh: () async {
            await Future.wait([_discoverHosts(), _refreshClipboard()]);
          },
          child: scrollView,
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
        SectionHeader(
          'Your devices',
          trailing: Row(
            mainAxisSize: MainAxisSize.min,
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
        ),
        if (hosts.isEmpty)
          _EmptyDevices(discovering: discovering)
        else
          ...hosts.map((h) {
            final canTap = onHostTap != null && h.online;
            return DeviceTile(
              icon: Icons.computer,
              title: h.name,
              subtitle: h.addressLabel,
              active: h.online,
              onTap: canTap ? () => onHostTap!(h) : null,
              trailing: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  StatusChip(
                    label: h.online ? 'Online' : 'Offline',
                    highlight: h.online,
                  ),
                  if (h.isSaved && onForgetHost != null)
                    IconButton(
                      icon: const Icon(Icons.delete_outline),
                      tooltip: 'Forget ${h.name}',
                      onPressed: busy ? null : () => onForgetHost!(h),
                    )
                  else if (canTap)
                    Icon(Icons.play_circle_fill,
                        color: theme.colorScheme.primary),
                ],
              ),
            );
          }),
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

