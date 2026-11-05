import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart' show PlatformViewHitTestBehavior;
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import 'package:tray_manager/tray_manager.dart';
import 'package:url_launcher/url_launcher.dart';
import 'package:window_manager/window_manager.dart';

import 'package:flutter/foundation.dart' show kDebugMode;

import 'beam_page.dart';
import 'hdr_probe.dart';
import 'hdr_status.dart';
import 'host_discovery.dart';
import 'decode_watchdog.dart';
import 'pairing_help.dart';
import 'src/rust/api.dart';
import 'src/rust/frb_generated.dart';
import 'stream_player.dart';
import 'ui.dart';

final _githubRepoUrl = Uri.parse('https://github.com/ontola/streambeam');
final _githubReleasesUrl = Uri.parse(
  'https://github.com/ontola/streambeam/releases',
);
final _ontolaUrl = Uri.parse('https://ontola.io');
const _logoAsset = 'assets/streambeam-logo.svg';

ThemeData _streamBeamTheme(Brightness brightness) {
  final colors = ColorScheme.fromSeed(
    seedColor: Colors.indigo,
    brightness: brightness,
  );
  return ThemeData(
    colorScheme: colors,
    useMaterial3: true,
    scaffoldBackgroundColor: colors.surface,
    appBarTheme: AppBarTheme(
      backgroundColor: colors.surfaceContainer,
      foregroundColor: colors.onSurface,
    ),
  );
}

SystemUiOverlayStyle _systemUiStyle(ColorScheme colors) {
  final iconBrightness = colors.brightness == Brightness.dark
      ? Brightness.light
      : Brightness.dark;
  return SystemUiOverlayStyle(
    statusBarColor: colors.surfaceContainer,
    statusBarIconBrightness: iconBrightness,
    statusBarBrightness: colors.brightness,
    systemNavigationBarColor: colors.surfaceContainer,
    systemNavigationBarDividerColor: colors.outlineVariant,
    systemNavigationBarIconBrightness: iconBrightness,
    systemNavigationBarContrastEnforced: false,
  );
}

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
  return AppBar(title: _appBarTitle(), actions: _appBarActions());
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

  /// Windows shows the context menu on right-click rather than opening it for
  /// us, so ask for it explicitly. Without this, right-clicking the icon did
  /// nothing at all.
  @override
  void onTrayIconRightMouseUp() {
    unawaited(trayManager.popUpContextMenu());
  }

  @override
  void onTrayMenuItemClick(MenuItem menuItem) {
    switch (menuItem.key) {
      case 'show':
        unawaited(windowManager.show());
        unawaited(windowManager.focus());
      case 'quit':
        unawaited(windowManager.destroy());
    }
  }
}

final _trayClickHandler = _TrayClickHandler();
final streamBeamMessengerKey = GlobalKey<ScaffoldMessengerState>();

/// Show an app-wide, floating error overlay. It belongs to MaterialApp rather
/// than either tab, so errors remain visible while switching tabs and over the
/// fullscreen player. A newer error replaces the current one instead of
/// building a stale queue of notices.
/// Put an error on the clipboard and say so, since the snackbar it came from
/// is replaced by the confirmation.
void _copyErrorToClipboard(String text) {
  unawaited(Clipboard.setData(ClipboardData(text: text)));
  final messenger = streamBeamMessengerKey.currentState;
  if (messenger == null) return;
  try {
    messenger
      ..hideCurrentSnackBar()
      ..showSnackBar(
        const SnackBar(
          behavior: SnackBarBehavior.floating,
          duration: Duration(seconds: 2),
          content: Text('Error copied to clipboard'),
        ),
      );
  } catch (_) {
    // Nowhere to confirm; the clipboard write still happened.
  }
}

void showErrorToast(String message) {
  final text = message.trim();
  if (text.isEmpty) return;
  // Log before attempting any UI. The toast is best-effort presentation and it
  // has already destroyed evidence once: when a stream error arrives while the
  // player screen is being torn down, `showSnackBar` asserts
  // ('_scaffolds.isNotEmpty') and the assertion replaces the only copy of the
  // real error, leaving "the stream ended" with no reason anywhere.
  debugPrint('StreamBeam error: $text');
  final messenger = streamBeamMessengerKey.currentState;
  if (messenger == null) {
    WidgetsBinding.instance.addPostFrameCallback((_) => showErrorToast(text));
    return;
  }
  final context = streamBeamMessengerKey.currentContext;
  final colors = context == null
      ? ColorScheme.fromSeed(seedColor: Colors.indigo)
      : Theme.of(context).colorScheme;
  try {
    messenger
      ..hideCurrentSnackBar()
      ..showSnackBar(
        SnackBar(
          behavior: SnackBarBehavior.floating,
          duration: const Duration(seconds: 8),
          backgroundColor: colors.errorContainer,
          showCloseIcon: true,
          closeIconColor: colors.onErrorContainer,
          content: Row(
            children: [
              Icon(Icons.error_outline, color: colors.onErrorContainer),
              const SizedBox(width: 10),
              Expanded(
                child: Text(
                  text,
                  style: TextStyle(color: colors.onErrorContainer),
                ),
              ),
            ],
          ),
          // These messages are the useful half of a bug report — decoder
          // statuses, host errors, addresses — and they vanish after 8
          // seconds. Copying beats retyping from a photo of the screen.
          action: SnackBarAction(
            label: 'Copy',
            textColor: colors.onErrorContainer,
            onPressed: () => _copyErrorToClipboard(text),
          ),
        ),
      );
  } catch (_) {
    // No Scaffold is mounted to present into (mid-teardown). Retry once the
    // next frame has built one; the message is already logged either way.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      final retryMessenger = streamBeamMessengerKey.currentState;
      if (retryMessenger == null) return;
      try {
        retryMessenger.showSnackBar(
          SnackBar(
            behavior: SnackBarBehavior.floating,
            duration: const Duration(seconds: 8),
            showCloseIcon: true,
            content: Text(text),
            action: SnackBarAction(
              label: 'Copy',
              onPressed: () => _copyErrorToClipboard(text),
            ),
          ),
        );
      } catch (_) {
        // Still nowhere to show it — the log line above is the record.
      }
    });
  }
}

Future<void> _initTray() async {
  // Best-effort: a tray failure must not stop the app from starting, and
  // saying so beats an icon that silently never appears.
  try {
    await trayManager.setIcon('assets/tray_icon.ico');
    await trayManager.setToolTip('StreamBeam');
    // A tray icon with no menu is close to useless — right-click did nothing,
    // so there was no way to show or quit the app from it.
    await trayManager.setContextMenu(
      Menu(
        items: [
          MenuItem(key: 'show', label: 'Show StreamBeam'),
          MenuItem.separator(),
          MenuItem(key: 'quit', label: 'Quit'),
        ],
      ),
    );
    _trayClickHandler.register();
    debugPrint('StreamBeam: tray icon ready');
  } catch (e) {
    debugPrint('StreamBeam: tray icon unavailable ($e)');
  }
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
  // The bridge schedules this on Rust’s worker pool. Storage must be ready
  // before the home screen reads pairing data or starts the local host.
  await setAppDataDir(dir: dir);
  await _applyDeviceName();
  if (Platform.isAndroid) {
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
    name = await const MethodChannel(
      'streambeam/device',
    ).invokeMethod<String>('getName');
  } catch (_) {}
  if ((name == null || name.trim().isEmpty) && !Platform.isAndroid) {
    // gethostname often carries an mDNS suffix ("Joeps-MacBook-Pro.local").
    name = Platform.localHostname.replaceFirst(RegExp(r'\.local$'), '');
  }
  if (name != null && name.trim().isNotEmpty) {
    await setDeviceName(name: name.trim());
  }
}

class StreamBeamApp extends StatelessWidget {
  const StreamBeamApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'StreamBeam',
      scaffoldMessengerKey: streamBeamMessengerKey,
      theme: _streamBeamTheme(Brightness.light),
      darkTheme: _streamBeamTheme(Brightness.dark),
      themeMode: ThemeMode.system,
      builder: (context, child) {
        final colors = Theme.of(context).colorScheme;
        return AnnotatedRegion<SystemUiOverlayStyle>(
          value: _systemUiStyle(colors),
          child: child ?? const SizedBox.shrink(),
        );
      },
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
  static const _clipboardPollInterval = Duration(milliseconds: 750);

  final _payloadCtrl = TextEditingController();
  final Set<String> _needsPairing = {};
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

  /// Alternate endpoints for `_pendingLanHost`'s identity, handed to the
  /// playback path so a non-answering endpoint fails over instead of erroring.
  List<DiscoveredHost> _pendingLanFailover = const [];
  String? _pendingLanAddr;
  bool _discoveryBusy = false;
  String? _discoveryError;
  Timer? _discoveryTimer;
  Timer? _viewerTimer;
  List<HostClientInfo> _viewers = [];
  MobileScannerController? _scannerController;
  String? _lastQrPayload;
  bool _qrPreviewOpen = false;
  bool _clipboardReady = false;
  Timer? _clipboardTimer;
  String? _lastSyncedClipboard;
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

  /// The stream is HDR10, so frames render on the native EDR layer instead of
  /// the Flutter texture — Flutter's compositor is SDR and would flatten the
  /// PQ signal (docs/HDR.md).
  bool _desktopHdr = false;
  double _desktopAspect = 16 / 9;
  int _desktopWidth = 0;
  int _desktopHeight = 0;
  StreamSubscription<DesktopStreamEvent>? _desktopSub;
  int _desktopPlaybackAttempt = 0;
  // Last desktop connection target, so a stream-mode/custom-settings change
  // can tear down and re-dial with the new caps (the Android path's
  // `_reconnectForNewCaps` uses `_beginSession`, which desktop doesn't).
  /// Platform of the host being watched, for OS-specific actions in the
  /// player (Ctrl+Alt+Del). Kept separately from the connect state because
  /// that is consumed when the session starts, and both the desktop and
  /// Android paths need it while streaming.
  String _watchedHostPlatform = '';
  DiscoveredHost? _lastDesktopHost;
  String? _lastDesktopAddr;

  /// Endpoints for the same identity still worth trying if the current one
  /// doesn't answer — one machine can run several host processes (see
  /// `_HostListEntry.alternates`), and only some of them may be reachable.
  List<DiscoveredHost> _desktopFailoverQueue = const [];
  String? _lastDesktopPayload;

  /// Re-dials spent on the current endpoint. Bounded so a genuinely dead host
  /// still reports itself as dead instead of retrying forever behind a
  /// hopeful spinner.
  int _desktopRetries = 0;
  static const _maxDesktopRetries = 3;
  // MetalFX upscaling (macOS): on by default; the native side reports back
  // whether it's actually available on this machine.
  bool _upscaleEnabled = true;
  bool _upscaleActive = false;
  bool? _upscaleSupported;

  void _setStatus(String message, {bool error = false}) {
    if (!mounted) return;
    if (error) {
      showErrorToast(_humanizeError(message));
      setState(() {
        _status = null;
        _statusIsError = false;
      });
      return;
    }
    setState(() {
      _status = message;
      _statusIsError = false;
    });
  }

  void _reportError(String message) {
    _setStatus(_humanizeError(message), error: true);
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
      return 'The connection ended before streaming could start. Try connecting again.';
    }
    // Rust's describe_connect_failure (core/src/session.rs) already produces
    // a specific, actionable message per failure kind (timeout vs reset vs
    // version mismatch vs application-closed) — pass it through rather than
    // flattening every connect failure into one generic sentence like this
    // used to. Only degenerate/unrecognized text falls through to the
    // fallback `return m` below.
    return m;
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

  void _wakeSavedHost(_HostListEntry host) {
    // Fire the magic packets; discovery keeps polling, so the tile flips to
    // Online by itself once the machine is up (S3 resume is typically a few
    // seconds; a cold machine may not support wake at all — see docs/WOL.md).
    final sent = wakeSavedHost();
    setState(() {
      _status = sent
          ? 'Wake signal sent to ${host.name} — it should appear online in a few seconds.'
          : 'No Wake-on-LAN info for ${host.name} yet. Connect to it once while it is on.';
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

  /// Identity key for grouping: the 32-byte host pubkey when present,
  /// otherwise the name. Two advertisements sharing a pubkey are the same
  /// machine — they read one `host_identity.pkcs8` from one data directory.
  static String _identityKey(List<int> pubkey, String name) =>
      pubkey.length == 32
      ? pubkey.map((b) => b.toRadixString(16).padLeft(2, '0')).join()
      : 'name:$name';

  List<_HostListEntry> _hostEntries() {
    final entries = <_HostListEntry>[];
    final localHostPubkey = hostPublicKey();

    // One group per identity, preserving discovery order within a group so the
    // first-seen endpoint stays the primary.
    final groups = <String, List<DiscoveredHost>>{};
    for (final h in _lanHosts) {
      groups.putIfAbsent(_identityKey(h.hostPubkey, h.name), () => []).add(h);
    }

    final saved = _savedHostInfo;
    final savedIsLocal =
        saved != null &&
        saved.hostPubkey.length == 32 &&
        localHostPubkey.length == 32 &&
        _pubkeysEqual(saved.hostPubkey, localHostPubkey);

    String? savedKey;
    if (saved != null && !savedIsLocal) {
      // Match the saved record to a live group by identity first, name second
      // — a renamed host keeps its key, and a rebuilt host keeps its name.
      savedKey = _identityKey(saved.hostPubkey, saved.name);
      if (!groups.containsKey(savedKey)) {
        final byName = groups.entries
            .where((e) => e.value.any((h) => h.name == saved.name))
            .map((e) => e.key)
            .firstOrNull;
        savedKey = byName;
      }
      final group = savedKey == null ? null : groups[savedKey];
      final primary = group?.first;
      entries.add(
        _HostListEntry(
          name: saved.name,
          addrs: primary?.addrs ?? saved.addrs,
          port: primary?.port ?? saved.port,
          hostPubkey: saved.hostPubkey.isNotEmpty
              ? saved.hostPubkey
              : (primary?.hostPubkey ?? const []),
          online: primary != null,
          isSaved: true,
          needsPairing: _needsPairing.contains(savedKey),
          discovered: primary,
          alternates: group == null ? const [] : group.skip(1).toList(),
        ),
      );
    }

    for (final entry in groups.entries) {
      if (entry.key == savedKey) continue;
      final group = entry.value;
      final primary = group.first;
      entries.add(
        _HostListEntry(
          name: primary.name,
          addrs: primary.addrs,
          port: primary.port,
          hostPubkey: primary.hostPubkey,
          online: true,
          isSaved: false,
          needsPairing: _needsPairing.contains(entry.key),
          discovered: primary,
          alternates: group.skip(1).toList(),
        ),
      );
    }

    return entries;
  }

  Future<void> _showPairingHelp(_HostListEntry entry) async {
    final action = await showDialog<PairingAction>(
      context: context,
      builder: (_) => PairingHelp(
        hostName: entry.name,
        canScan: Platform.isAndroid || Platform.isIOS,
        needsCode: entry.needsPairing,
      ),
    );
    if (!mounted) return;
    switch (action) {
      case PairingAction.requestAccess:
        await _connectHostEntry(entry, skipSetup: true);
      case PairingAction.scan:
        if (!_qrPreviewOpen) await _toggleQrPreview();
      case PairingAction.paste:
        await _pasteFromClipboard();
      case null:
        break;
    }
  }

  void _handleConnectionError(Object error, DiscoveredHost? host) {
    final message = error.toString();
    // Only an explicit host rejection proves pairing is needed. A network
    // close can have many other causes and must not be called a pairing error.
    if (host != null &&
        message.toLowerCase().contains('pair rejected') &&
        message.toLowerCase().contains('pairing requires')) {
      setState(
        () => _needsPairing.add(_identityKey(host.hostPubkey, host.name)),
      );
      final entry = _hostEntries()
          .where(
            (h) =>
                _identityKey(h.hostPubkey, h.name) ==
                _identityKey(host.hostPubkey, host.name),
          )
          .firstOrNull;
      if (entry != null) {
        unawaited(_showPairingHelp(entry));
        return;
      }
    }
    _reportError(message);
  }

  Future<void> _connectHostEntry(
    _HostListEntry entry, {
    bool skipSetup = false,
  }) async {
    if (_busy) return;
    if (!entry.online) {
      _reportError('${entry.name} is offline.');
      return;
    }
    if (!skipSetup && (!entry.isSaved || entry.needsPairing)) {
      await _showPairingHelp(entry);
      return;
    }
    final discovered = entry.discovered;
    if (discovered != null) {
      await _connectLanHost(discovered, failover: entry.alternates);
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
    _primeHdrCapability();
    if (Platform.isAndroid || Platform.isIOS) {
      _scannerController = MobileScannerController(
        autoStart: false,
        detectionSpeed: DetectionSpeed.noDuplicates,
        facing: CameraFacing.back,
        formats: const [BarcodeFormat.qrCode],
      );
    }
    unawaited(_ensureMdnsLock());
    unawaited(_discoverHosts());
    _refreshViewers();
    unawaited(_refreshClipboard());
    _clipboardTimer = Timer.periodic(_clipboardPollInterval, (_) {
      if (mounted) unawaited(_refreshClipboard());
    });
    _discoveryTimer = Timer.periodic(_discoveryInterval, (_) {
      if (!_busy && mounted) {
        unawaited(_discoverHosts());
      }
    });
    _viewerTimer = Timer.periodic(
      const Duration(milliseconds: 250),
      (_) => _refreshViewers(),
    );
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
    _viewerTimer?.cancel();
    _clipboardTimer?.cancel();
    _cancelSurfaceTimeout();
    unawaited(_releaseMdnsLock());
    _scannerController?.dispose();
    unawaited(_desktopSub?.cancel());
    _payloadCtrl.dispose();
    super.dispose();
  }

  void _refreshViewers() {
    if (!mounted || !hostPlatformSupported()) return;
    final next = hostSnapshot().viewers;
    final currentKey = _viewers
        .map((viewer) => '${viewer.name}\u0000${viewer.addr}')
        .join('\u0001');
    final nextKey = next
        .map((viewer) => '${viewer.name}\u0000${viewer.addr}')
        .join('\u0001');
    if (currentKey != nextKey) {
      setState(() => _viewers = next);
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // iOS suspends networking/audio in the background. End the session before
    // suspension so returning cannot leave a disposed texture marked active.
    if (Platform.isIOS && state == AppLifecycleState.paused && _busy) {
      unawaited(_stopDesktopPlayback());
    }
    if (state == AppLifecycleState.resumed && !_busy) {
      unawaited(_refreshClipboard());
    }
  }

  Future<void> _refreshClipboard() async {
    final remote = takeRemoteClipboard() ?? hostTakeRemoteClipboard();
    if (remote != null && remote != _lastSyncedClipboard) {
      _lastSyncedClipboard = remote;
      await Clipboard.setData(ClipboardData(text: remote));
      debugPrint(
        'StreamBeam clipboard: applied remote text (${remote.length} chars)',
      );
    }

    // iOS asks before reading another app's clipboard; only the explicit Paste
    // action may read it, rather than polling and repeatedly prompting.
    if (Platform.isIOS) return;
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text;
    final clientActive = _showVideo && _status == 'Streaming…';
    final hostActive = _viewers.isNotEmpty;
    if (text != null &&
        text.length <= 60000 &&
        text != _lastSyncedClipboard &&
        (clientActive || hostActive)) {
      _lastSyncedClipboard = text;
      if (clientActive) {
        sendClipboardText(text: text);
        debugPrint(
          'StreamBeam clipboard: sent client text (${text.length} chars)',
        );
      }
      if (hostActive) {
        hostPublishClipboard(text: text);
        debugPrint(
          'StreamBeam clipboard: sent host text (${text.length} chars)',
        );
      }
    }

    final payload = text == null ? null : _extractPayload(text);
    final ready = payload != null;
    if (!mounted || ready == _clipboardReady) return;
    setState(() => _clipboardReady = ready);
  }

  Future<void> _syncScanner() async {
    if (!Platform.isAndroid && !Platform.isIOS) return;
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
      WidgetsBinding.instance.addPostFrameCallback(
        (_) => unawaited(_syncScanner()),
      );
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
    // Native playback saves the host after authentication. Manual stop changes
    // _sessionId before its Future completes, so that completion cannot refresh
    // the list. Reload the persisted identity whenever we leave the player.
    _savedHostInfo = loadSavedHostInfo();
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

  static const _nativeDiscoveryChannel = MethodChannel(
    'streambeam/native_discovery',
  );

  /// Desktop host discovery via native Bonjour (macOS). The browser runs
  /// continuously once started, so results accumulate across refreshes.
  Future<List<DiscoveredHost>> _discoverHostsNative() async {
    await _nativeDiscoveryChannel.invokeMethod('start');
    await Future<void>.delayed(const Duration(seconds: 1));
    final raw = await _nativeDiscoveryChannel.invokeMethod<List<dynamic>>(
      'getHosts',
    );
    return (raw ?? const []).map((entry) {
      final map = entry as Map;
      final addrs = (map['addrs'] as List?) ?? const [];
      return DiscoveredHost(
        name: map['name'] as String? ?? '',
        port: map['port'] as int? ?? 0,
        addrs: addrs.map((e) => e as String).toList(),
        hostPubkey: (map['pubkey'] as Uint8List?) ?? Uint8List(0),
        platform: map['platform'] as String? ?? '',
      );
    }).toList();
  }

  Future<void> _discoverHosts() async {
    if (_discoveryBusy) return;
    if (mounted) {
      setState(() {
        _discoveryBusy = true;
      });
    }
    try {
      if (Platform.isAndroid) await _ensureMdnsLock();
      final hosts = await discoverHostsOnPlatform(
        operatingSystem: Platform.operatingSystem,
        rustDiscovery: () => discoverHosts(timeoutMs: 6000),
        nativeDiscovery: _discoverHostsNative,
      );
      if (!mounted) return;
      final localHostPubkey = hostPublicKey();
      final visibleHosts = localHostPubkey.length == 32
          ? hosts
                .where(
                  (host) =>
                      host.hostPubkey.length != 32 ||
                      !_pubkeysEqual(host.hostPubkey, localHostPubkey),
                )
                .toList()
          : hosts;
      setState(() {
        _lanHosts = visibleHosts;
        _discoveryBusy = false;
        _discoveryError = null;
      });
    } catch (e) {
      if (mounted) {
        final message = _humanizeError(e.toString());
        if (_discoveryError != message) showErrorToast(message);
        setState(() {
          _discoveryBusy = false;
          // Retained only to deduplicate repeated discovery polling failures;
          // errors themselves are rendered by the app-wide toast.
          _discoveryError = message;
        });
      }
    }
  }

  Future<void> _applyPayload(String normalized) async {
    try {
      _payloadCtrl.text = normalized;
      await saveHostFromPayload(payload: normalized);
      _refreshSavedHost();
      final saved = _savedHostInfo;
      if (saved != null) {
        _needsPairing.remove(_identityKey(saved.hostPubkey, saved.name));
      }
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

  /// Ask the EDR layer whether HDR is really reaching the screen. Null on any
  /// error: a diagnostic must never be able to take a stream down.
  Future<HdrStatus?> _probeHdrStatus() async {
    try {
      return HdrStatus.fromMap(
        await _desktopVideoChannel.invokeMethod<Map<Object?, Object?>>(
          'hdrStatus',
        ),
      );
    } catch (_) {
      return null;
    }
  }

  Future<bool> _desktopCanAv1() async {
    final cached = _desktopAv1;
    if (cached != null) return cached;
    bool can;
    try {
      can =
          await _desktopVideoChannel.invokeMethod<bool>('canDecodeAV1') ??
          false;
    } catch (_) {
      can = false;
    }
    _desktopAv1 = can;
    return can;
  }

  bool? _desktopHdrCapable;

  /// Initial state of the HDR toggle. Off, deliberately.
  ///
  /// HDR10 passthrough is opt-in because the *display* end of it is not
  /// finished, and that is a judgement made from evidence rather than caution:
  /// every stage is verified correct — the host's encode, the capture, the
  /// decode, the colour conversion, the EDR layer — and the picture still
  /// looks worse than the SDR tone-mapped path on a laptop. The reason is
  /// exposure: a game's diffuse white sits near 73 nits (measured, HITMAN),
  /// which reads as white on a TV your eye has adapted to and as grey next to
  /// an app's own UI white. Until that is mapped properly, the default must
  /// not cost anyone their working SDR stream.
  ///
  /// `--dart-define=STREAMBEAM_HDR=true` just presets the toggle, for
  /// development.
  static const _hdrDefault = bool.fromEnvironment(
    'STREAMBEAM_HDR',
    defaultValue: false,
  );

  /// Whether the user has asked for HDR passthrough this session. Session
  /// scoped, like the stream mode and sound settings next to it — this app
  /// persists no playback preferences yet.
  bool _hdrEnabled = _hdrDefault;

  /// Whether to advertise HDR10 in `VideoCaps`. Requires hardware AV1 decode
  /// *and* an EDR-capable display — see the native side for why both.
  /// Non-blocking by design. This app runs with a merged UI/platform thread,
  /// so awaiting a platform-channel reply *inside* a tap handler deadlocks:
  /// the tap's future waits for a reply that needs the thread the tap is
  /// occupying. Observed as the whole app freezing on the Watch button. The
  /// capability is primed once at startup and read synchronously afterwards.
  bool _canDisplayHdrNow() => _hdrEnabled && (_desktopHdrCapable ?? false);

  /// Fire-and-forget probe, run once from `initState` so the answer is ready
  /// before anything needs it.
  void _primeHdrCapability() {
    // Probed even when the toggle is off: the answer decides whether the
    // toggle is offered at all, and asking later would mean asking from a tap
    // handler, which deadlocks on this app's merged UI/platform thread.
    if (_desktopHdrCapable != null) return;
    unawaited(() async {
      try {
        _desktopHdrCapable =
            await _desktopVideoChannel.invokeMethod<bool>('canDisplayHDR') ??
            false;
      } catch (_) {
        _desktopHdrCapable = false; // platform without the method: keep SDR.
      }
    }());
  }

  /// Desktop watch: drive the Rust frame stream into the native VideoToolbox
  /// decoder (macOS) and show its texture.
  Future<void> _startDesktopPlayback(String payload) async {
    await _stopDesktopPlayback();
    _lastDesktopPayload = payload;
    _watchedHostPlatform = '';
    _lastDesktopHost = null;
    _lastDesktopAddr = null;
    final attempt = _desktopPlaybackAttempt;
    setState(() {
      _busy = true;
      _status =
          'Connecting… If the other device is Android, choose what to share there and confirm its screen-sharing prompt.';
      _statusIsError = false;
    });
    final (maxWidth, maxHeight, maxFps) = _deviceVideoCaps();
    final allowAv1 = await _desktopCanAv1();
    final allowHdr = _canDisplayHdrNow();
    if (!mounted || attempt != _desktopPlaybackAttempt) return;
    _attachDesktopStream(
      playStreamDesktop(
        payload: payload,
        allowAv1: allowAv1,
        allowHdr: allowHdr,
        maxWidth: maxWidth,
        maxHeight: maxHeight,
        maxFps: maxFps,
      ),
      attempt,
    );
  }

  /// Whether an error means "this endpoint never answered", as opposed to a
  /// session that started and then failed. Only the former is worth retrying
  /// elsewhere — retrying a decode or capture failure on another port would
  /// just fail the same way, slower.
  static bool _looksUnreachable(String error) {
    final e = error.toLowerCase();
    return e.contains('no response from the host') ||
        e.contains('timed out') ||
        e.contains('connection refused') ||
        e.contains('unreachable');
  }

  /// Watch a discovered/saved host by its pinned key (no QR needed; the host
  /// prompts to approve on first contact).
  Future<void> _startDesktopPlaybackKnown(
    DiscoveredHost host,
    String addr, {
    List<DiscoveredHost> failover = const [],
  }) async {
    await _stopDesktopPlayback();
    _desktopFailoverQueue = failover;
    _watchedHostPlatform = host.platform;
    _lastDesktopHost = host;
    _lastDesktopAddr = addr;
    _lastDesktopPayload = null;
    final attempt = _desktopPlaybackAttempt;
    setState(() {
      _busy = true;
      // Only Android hosts require a per-session consent on their side; a
      // desktop host starts streaming without any prompt. Old hosts don't
      // advertise their platform, so those keep the conditional hint.
      _status = switch (host.platform) {
        'android' =>
          'Connecting to ${host.name}… On the Android device, choose what to share and confirm the screen-sharing prompt.',
        '' =>
          'Connecting to ${host.name}… If it\'s an Android device, choose what to share there and confirm its screen-sharing prompt.',
        _ => 'Connecting to ${host.name}…',
      };
      _statusIsError = false;
    });
    final (maxWidth, maxHeight, maxFps) = _deviceVideoCaps();
    final allowAv1 = await _desktopCanAv1();
    final allowHdr = _canDisplayHdrNow();
    if (!mounted || attempt != _desktopPlaybackAttempt) return;
    _attachDesktopStream(
      playStreamDesktopKnown(
        hostName: host.name,
        hostPubkey: host.hostPubkey,
        addr: addr,
        allowAv1: allowAv1,
        allowHdr: allowHdr,
        maxWidth: maxWidth,
        maxHeight: maxHeight,
        maxFps: maxFps,
      ),
      attempt,
    );
  }

  void _attachDesktopStream(Stream<DesktopStreamEvent> stream, int attempt) {
    _desktopSub = stream.listen(
      (ev) async {
        if (!mounted || attempt != _desktopPlaybackAttempt) return;
        if (ev.isConfig) {
          // Sent once at session start and again on an in-band
          // reconfiguration (the host device rotated): rebuild the native
          // decoder and adopt the new geometry.
          try {
            final res = await _desktopVideoChannel
                .invokeMethod<dynamic>('configure', {
                  'codecConfig': ev.codecConfig,
                  'codec': ev.codec,
                  'width': ev.width,
                  'height': ev.height,
                  'hdr10': ev.hdr,
                });
            int? id;
            // Prefer what the decoder read from the bitstream (reconfig
            // events carry 0×0 on the wire); fall back to the event, then
            // to whatever we already had.
            var w = ev.width > 0 ? ev.width : _desktopWidth;
            var h = ev.height > 0 ? ev.height : _desktopHeight;
            if (res is int) {
              // Legacy result shape (Windows runner): just the texture id.
              id = res;
            } else if (res is Map) {
              id = res['textureId'] as int?;
              final rw = res['width'] as int? ?? 0;
              final rh = res['height'] as int? ?? 0;
              if (rw > 0 && rh > 0) {
                w = rw;
                h = rh;
              }
            }
            if (id != null && mounted && attempt == _desktopPlaybackAttempt) {
              setState(() {
                _desktopHdr = ev.hdr;
                _desktopTextureId = id;
                _desktopAspect = h > 0 ? w / h : 16 / 9;
                _desktopWidth = w;
                _desktopHeight = h;
                _status = 'Streaming…';
                _showVideo = true;
                _busy = false;
              });
              // Media is flowing, so the endpoint is healthy: a later drop
              // deserves its own full retry budget rather than inheriting the
              // one spent getting here.
              _desktopRetries = 0;
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
      onError: (e) async {
        if (attempt != _desktopPlaybackAttempt) return;
        // One machine can advertise several host processes and only some of
        // them answer — the packaged service may be firewalled while a
        // hand-started build works, or vice versa. Try this identity's other
        // endpoints before telling the user their PC is unreachable, since
        // from their side it plainly isn't.
        final next = _desktopFailoverQueue.firstOrNull;
        debugPrint(
          'StreamBeam: stream error (failover queue: '
          '${_desktopFailoverQueue.length}, retryable: '
          '${_looksUnreachable(e.toString())})',
        );
        if (next != null && _looksUnreachable(e.toString())) {
          final rest = _desktopFailoverQueue.skip(1).toList();
          final addr = _bestLanAddr(next);
          if (addr != null) {
            debugPrint(
              'StreamBeam: ${_lastDesktopAddr ?? "endpoint"} did not answer, '
              'trying $addr',
            );
            unawaited(_startDesktopPlaybackKnown(next, addr, failover: rest));
            return;
          }
        }
        // Re-dial the *same* endpoint before giving up. The Windows host now
        // restarts its worker whenever the machine locks or unlocks — the
        // service swaps between a SYSTEM worker (the only one that can see the
        // login screen) and a user-token worker (the only one that can capture
        // a game). That tears the session down by design, so a client that
        // quits on the first drop turns a deliberate two-second handover into
        // "timed out waiting for media" and a dead end.
        if (_looksUnreachable(e.toString()) &&
            _desktopRetries < _maxDesktopRetries) {
          _desktopRetries++;
          final host = _lastDesktopHost;
          final addr = _lastDesktopAddr;
          final payload = _lastDesktopPayload;
          setState(() {
            _status = 'Reconnecting to the host…';
            _statusIsError = false;
          });
          Future.delayed(const Duration(seconds: 2), () {
            if (!mounted || attempt != _desktopPlaybackAttempt) return;
            if (host != null && addr != null) {
              unawaited(_startDesktopPlaybackKnown(host, addr));
            } else if (payload != null) {
              unawaited(_startDesktopPlayback(payload));
            }
          });
          return;
        }
        final host = _lastDesktopHost;
        await _stopDesktopPlayback();
        if (mounted) _handleConnectionError(e, host);
      },
      onDone: () {
        if (attempt == _desktopPlaybackAttempt) {
          unawaited(_stopDesktopPlayback());
        }
      },
    );
  }

  /// Read the native decoder's counters for the player's watchdog. Returns null
  /// when the platform has no `health` implementation, which disables the check
  /// rather than breaking playback.
  Future<DecodeHealth?> _probeDecodeHealth() async {
    final reply = await _desktopVideoChannel.invokeMethod<dynamic>('health');
    return DecodeHealth.fromChannel(reply);
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
    _desktopPlaybackAttempt++;
    final sub = _desktopSub;
    _desktopSub = null;
    stopPlayback();
    appImmersive.value = false;
    if (mounted) {
      setState(() {
        _savedHostInfo = loadSavedHostInfo();
        _desktopTextureId = null;
        _desktopHdr = false;
        _showVideo = false;
        _busy = false;
        _upscaleActive = false;
        if (_status == 'Streaming…' ||
            (_status?.startsWith('Connecting') ?? false)) {
          _status = null;
        }
      });
    }
    try {
      await sub?.cancel();
    } catch (e) {
      // Closing the QUIC connection can make FRB's stream cancellation
      // finish with `closed`. Teardown must still release the decoder and
      // return to the caller so a quality change can reconnect.
      debugPrint('StreamBeam: stream closed during cancellation: $e');
    }
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
        _reportError(
          'Clipboard empty. Copy sbm1: from streambeam-host on your PC, or scan the QR.',
        );
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
      final lanFailover = _pendingLanFailover;
      _pendingLanHost = null;
      _pendingLanAddr = null;
      _pendingLanFailover = const [];
      if (lanHost != null && lanAddr != null) {
        await _startDesktopPlaybackKnown(
          lanHost,
          lanAddr,
          failover: lanFailover,
        );
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
      // Same platform tailoring as the desktop path: only an Android host
      // shows a consent prompt the user must go confirm.
      _status = reconnect
          ? 'Reconnecting…'
          : switch (_pendingLanHost?.platform) {
              'android' =>
                'Connecting… On ${_pendingLanHost!.name}, choose what to share and confirm the screen-sharing prompt.',
              null || '' => 'Connecting…',
              _ => 'Connecting to ${_pendingLanHost!.name}…',
            };
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
  /// instead of a fixed 1080p/60fps guess. `Display.size` should be physical
  /// pixels, matching what the host's encoder resolution means — true on
  /// Windows/Android, but not on macOS (see the `dpr` correction below).
  /// Falls back to 0s (the Rust side substitutes its own sane default) if no
  /// display info is available yet, which shouldn't normally happen once the
  /// app's UI is actually visible and calling this.
  (int, int, int) _deviceVideoCaps() {
    final views = ui.PlatformDispatcher.instance.views;
    if (views.isEmpty) return (0, 0, 0);
    final display = views.first.display;
    // `Display.size` is documented as physical pixels, but on macOS it
    // actually reports logical points instead (a confirmed Flutter platform
    // bug: https://github.com/flutter/flutter/issues/142629) — a Retina
    // display then looks like roughly half its real resolution, silently
    // capping the encode far below what the client can actually show.
    // Windows/Android aren't affected (their `Display.size` really is
    // physical), so only correct for it on macOS specifically.
    final dpr = Platform.isMacOS ? display.devicePixelRatio : 1.0;
    // Previously hardcapped at 60: a 120Hz device was observed skipping
    // 95-99% of frames (stuck at 0-2fps) at 120fps, which looked like a
    // hard decoder-throughput ceiling. Root-caused since to a separate bug
    // (self-inflicted packet loss from unpaced FEC shard bursts, fixed in
    // the host's send loop — see docs/PROGRESS.md 2026-07-07) rather than a
    // real decode limit, so raised to match the host's own ceiling
    // (`MAX_CLIENT_FPS` in crates/host/src/server.rs) — unverified above
    // 60fps on real hardware post-fix; watch for the same symptom
    // reappearing on any given device/decoder.
    const maxSustainableDecodeFps = 120;
    final (rawWidth, rawHeight, rawFps) = (
      (display.size.width * dpr).round(),
      (display.size.height * dpr).round(),
      display.refreshRate.round().clamp(1, maxSustainableDecodeFps),
    );
    return _streamMode.apply(
      rawWidth,
      rawHeight,
      rawFps,
      customSettings: _customSettings,
      isAndroid: Platform.isAndroid,
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

  /// Desktop watch equivalent of `_reconnectForNewCaps`: stop and re-dial
  /// the remembered target so the new mode/custom caps reach the host.
  /// (`_startDesktopPlayback*` recompute `_deviceVideoCaps()` on entry.)
  Future<void> _reconnectDesktopForNewCaps() async {
    final host = _lastDesktopHost;
    final addr = _lastDesktopAddr;
    final payload = _lastDesktopPayload;
    if (host != null && addr != null) {
      await _startDesktopPlaybackKnown(host, addr);
    } else if (payload != null) {
      await _startDesktopPlayback(payload);
    }
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
    // The Android surface path has no endpoint failover (it connects through
    // `playDiscoveredHost`, not the desktop stream); just don't leave the
    // queue set for whatever session comes next.
    _pendingLanFailover = const [];
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
        _status = null;
        _statusIsError = false;
        _teardownVideo();
      });
    } catch (e) {
      if (!_sessionAlive(sessionId)) return;
      setState(() {
        _status = null;
        _statusIsError = false;
        _teardownVideo();
      });
      _handleConnectionError(e, lanHost);
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
        _status = null;
        _statusIsError = false;
        _teardownVideo();
      });
    } catch (e) {
      if (!_sessionAlive(sessionId)) return;
      showErrorToast(_humanizeError(e.toString()));
      setState(() {
        _status = null;
        _statusIsError = false;
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
      _reportError(
        'No pairing code found. Copy sbm1: from streambeam-host or scan the QR.',
      );
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

  Future<void> _connectLanHost(
    DiscoveredHost host, {
    List<DiscoveredHost> failover = const [],
  }) async {
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
    debugPrint(
      'StreamBeam: connecting to ${host.name} at $addr'
      '${failover.isEmpty ? " (no alternates)" : ", ${failover.length} alternate endpoint(s)"}',
    );
    _pendingLanHost = host;
    _pendingLanAddr = addr;
    _pendingLanFailover = failover;
    _watchedHostPlatform = host.platform;
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
    stopPlayback();
    setState(() {
      _teardownVideo();
      _busy = false;
      _status = null;
      _statusIsError = false;
    });
    unawaited(_syncScanner());
  }

  void _cancelConnectionAttempt() {
    if (Platform.isAndroid) {
      _stopStreaming();
      return;
    }
    unawaited(_stopDesktopPlayback());
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
        video: _desktopHdr
            ? const AppKitView(
                viewType: 'streambeam/video_hdr',
                layoutDirection: TextDirection.ltr,
              )
            : Texture(textureId: _desktopTextureId!),
        hostAcceptsTouch: playbackHostAcceptsTouch(),
        touchControls: Platform.isIOS,
        upscaleSupported: _upscaleSupported ?? false,
        upscaleActive: _upscaleActive,
        onToggleUpscale: _toggleUpscale,
        probeDecodeHealth: _probeDecodeHealth,
        probeHdrStatus: _desktopHdr ? _probeHdrStatus : null,
        hdrSupported: _desktopHdrCapable ?? false,
        hdrEnabled: _hdrEnabled,
        onHdrEnabledChanged: (on) {
          if (on == _hdrEnabled) return;
          setState(() => _hdrEnabled = on);
          // HDR is negotiated in the client's `VideoCaps` at connect time, so
          // changing it mid-session means re-dialling — same as the stream
          // mode and resolution controls above.
          unawaited(_reconnectDesktopForNewCaps());
        },
        hostPlatform: _watchedHostPlatform,
        onStop: () => unawaited(_stopDesktopPlayback()),
        streamMode: _streamMode,
        // Same semantics as Android's `_changeStreamMode`/`_commitCustom-
        // Settings`, but through the desktop reconnect path: switching *to*
        // Custom only reveals the sliders (nothing to apply yet); committing
        // a slider or picking any other mode re-dials with the new caps.
        onStreamModeChanged: (mode) {
          if (mode == _streamMode) return;
          setState(() => _streamMode = mode);
          if (mode != StreamMode.custom) {
            unawaited(_reconnectDesktopForNewCaps());
          }
        },
        customSettings: _customSettings,
        onCustomSettingsChanged: (settings) =>
            setState(() => _customSettings = settings),
        onCustomSettingsCommitted: () {
          if (_streamMode == StreamMode.custom) {
            unawaited(_reconnectDesktopForNewCaps());
          }
        },
        onDisplayChanged: () => unawaited(_reconnectDesktopForNewCaps()),
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
        video: const AndroidStreamView(),
        hostAcceptsTouch: playbackHostAcceptsTouch(),
        hostPlatform: _watchedHostPlatform,
        onStop: _stopStreaming,
        streamMode: _streamMode,
        onStreamModeChanged: _changeStreamMode,
        customSettings: _customSettings,
        onCustomSettingsChanged: _updateCustomSettings,
        onCustomSettingsCommitted: _commitCustomSettings,
        onDisplayChanged: _reconnectForNewCaps,
      );
    }

    final showConnectingOverlay =
        _busy && !_showVideo && _desktopTextureId == null;

    // The header eats a big chunk of the (already scarce, especially in
    // landscape) vertical space if pinned, so it floats away on scroll down
    // and reappears on scroll up instead — see AppShell in beam_page.dart for
    // the Watch/Beam tab bar's own landscape-rail treatment.
    final scrollView = CustomScrollView(
      physics: const AlwaysScrollableScrollPhysics(),
      slivers: [
        SliverAppBar(
          title: _appBarTitle(),
          actions: _appBarActions(),
          floating: true,
          snap: true,
        ),
        SliverPadding(
          padding: const EdgeInsets.all(16),
          sliver: SliverToBoxAdapter(
            child: CenteredContent(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  // Devices first — the whole point of the screen.
                  _DevicesSection(
                    // The connecting modal already shows a spinner; a second
                    // one in the (dimmed) list behind it reads as two
                    // competing loading states.
                    busy: _busy && !showConnectingOverlay,
                    hosts: _hostEntries(),
                    discovering: _discoveryBusy,
                    onHostTap: _busy
                        ? null
                        : (h) => unawaited(_connectHostEntry(h)),
                    onForgetHost: _busy
                        ? null
                        : (_) => unawaited(_forgetSavedHost()),
                    canWakeSaved: savedHostCanWake(),
                    onWakeHost: _busy ? null : (h) => _wakeSavedHost(h),
                  ),
                  // The connecting modal shows _status itself; repeating it
                  // in a card behind the dimmed barrier just doubles up.
                  if (_status != null && !showConnectingOverlay) ...[
                    const SizedBox(height: 12),
                    Semantics(
                      liveRegion: true,
                      child: Padding(
                        padding: const EdgeInsets.symmetric(horizontal: 4),
                        child: Text(
                          _status!,
                          style: Theme.of(context).textTheme.bodySmall
                              ?.copyWith(
                                color: Theme.of(
                                  context,
                                ).colorScheme.onSurfaceVariant,
                              ),
                        ),
                      ),
                    ),
                  ],
                  const SizedBox(height: 16),
                  // Add a host by QR or pasted code.
                  if ((Platform.isAndroid || Platform.isIOS) &&
                      _qrPreviewOpen) ...[
                    _QrScanPreview(
                      controller: _scannerController!,
                      paused: _busy,
                      onRetry: () => unawaited(_syncScanner()),
                      onDetect: _handleQrDetect,
                    ),
                    const SizedBox(height: 8),
                  ],
                  Row(
                    children: [
                      Expanded(
                        child: (_clipboardReady || Platform.isIOS)
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
                      if (Platform.isAndroid || Platform.isIOS) ...[
                        const SizedBox(width: 8),
                        Expanded(
                          child: FilledButton.tonalIcon(
                            onPressed: _busy ? null : _toggleQrPreview,
                            icon: Icon(
                              _qrPreviewOpen
                                  ? Icons.close
                                  : Icons.qr_code_scanner,
                            ),
                            label: Text(
                              _qrPreviewOpen ? 'Hide scanner' : 'Scan QR',
                            ),
                          ),
                        ),
                      ],
                    ],
                  ),
                  const SizedBox(height: 24),
                  const BeamPage(embedded: true),
                  // Debug builds only: the EDR probe behind HDR passthrough
                  // (docs/HDR.md). Not a user-facing feature — it answers
                  // whether HDR can reach the screen at all.
                  if (kDebugMode && Platform.isMacOS) ...[
                    const SizedBox(height: 12),
                    Center(
                      child: TextButton.icon(
                        icon: const Icon(Icons.hdr_on, size: 18),
                        onPressed: () => Navigator.of(context).push(
                          MaterialPageRoute<void>(
                            builder: (_) => const HdrProbeScreen(),
                          ),
                        ),
                        label: const Text('HDR (EDR) probe'),
                      ),
                    ),
                  ],
                  const SizedBox(height: 20),
                  Center(
                    child: TextButton(
                      onPressed: openOntola,
                      style: TextButton.styleFrom(
                        foregroundColor: Theme.of(
                          context,
                        ).colorScheme.onSurfaceVariant,
                        textStyle: Theme.of(context).textTheme.bodySmall,
                        enabledMouseCursor: SystemMouseCursors.click,
                      ),
                      child: const Text('Made with <3 by Ontola.io'),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ],
    );

    return PopScope(
      canPop: !showConnectingOverlay,
      onPopInvokedWithResult: (didPop, result) {
        if (!didPop && showConnectingOverlay) {
          _cancelConnectionAttempt();
        }
      },
      child: Scaffold(
        body: Stack(
          children: [
            SafeArea(
              top: false,
              child: RefreshIndicator(
                onRefresh: () async {
                  await Future.wait([_discoverHosts(), _refreshClipboard()]);
                },
                child: scrollView,
              ),
            ),
            if (showConnectingOverlay) ...[
              const Positioned.fill(
                child: ModalBarrier(
                  dismissible: false,
                  color: Color(0x99000000),
                ),
              ),
              Positioned.fill(
                child: SafeArea(
                  child: Center(
                    child: Padding(
                      padding: const EdgeInsets.all(24),
                      child: Card(
                        elevation: 12,
                        child: ConstrainedBox(
                          constraints: const BoxConstraints(maxWidth: 460),
                          child: Padding(
                            padding: const EdgeInsets.all(24),
                            child: Column(
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                const CircularProgressIndicator(),
                                const SizedBox(height: 20),
                                // One line, not two: the status already says
                                // "Connecting to <host>…" and often more, so a
                                // separate "Connecting…" title just repeated
                                // the least informative half of it.
                                Text(
                                  _status ?? 'Connecting…',
                                  textAlign: TextAlign.center,
                                  style: Theme.of(
                                    context,
                                  ).textTheme.titleMedium,
                                ),
                                const SizedBox(height: 24),
                                FilledButton.tonalIcon(
                                  onPressed: _cancelConnectionAttempt,
                                  icon: const Icon(Icons.close),
                                  label: const Text('Cancel'),
                                ),
                              ],
                            ),
                          ),
                        ),
                      ),
                    ),
                  ),
                ),
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
    required this.onRetry,
  });

  final MobileScannerController controller;
  final void Function(BarcodeCapture capture) onDetect;
  final bool paused;
  final VoidCallback onRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return ClipRRect(
      borderRadius: BorderRadius.circular(12),
      child: SizedBox(
        height: 180,
        child: Stack(
          fit: StackFit.expand,
          children: [
            MobileScanner(
              controller: controller,
              onDetect: paused ? null : onDetect,
              fit: BoxFit.cover,
              errorBuilder: (context, error) => ColoredBox(
                color: theme.colorScheme.surfaceContainerHighest,
                child: Center(
                  child: SingleChildScrollView(
                    padding: const EdgeInsets.all(12),
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Text(
                          error.errorCode ==
                                  MobileScannerErrorCode.permissionDenied
                              ? 'Allow camera access to scan a QR code'
                              : 'Couldn’t start the QR scanner',
                          textAlign: TextAlign.center,
                          style: theme.textTheme.titleSmall,
                        ),
                        const SizedBox(height: 6),
                        Text(
                          error.errorCode ==
                                  MobileScannerErrorCode.permissionDenied
                              ? 'Enable Camera in Android Settings → Apps → StreamBeam → Permissions.'
                              : 'Try again, or copy the host’s pairing link and use Paste & play.',
                          textAlign: TextAlign.center,
                          style: theme.textTheme.bodySmall,
                        ),
                        TextButton.icon(
                          onPressed: onRetry,
                          icon: const Icon(Icons.refresh),
                          label: const Text('Retry scanner'),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
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
              child: ValueListenableBuilder<MobileScannerState>(
                valueListenable: controller,
                builder: (context, state, child) =>
                    state.error != null ? const SizedBox.shrink() : child!,
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
                      paused
                          ? 'Scanner paused while streaming'
                          : 'Point at host QR',
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: Colors.white,
                      ),
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
    this.needsPairing = false,
    this.discovered,
    this.alternates = const [],
  });

  final String name;
  final List<String> addrs;
  final int port;
  final List<int> hostPubkey;
  final bool online;
  final bool isSaved;
  final bool needsPairing;
  final DiscoveredHost? discovered;

  /// Other endpoints advertising this *same identity* — one machine running
  /// more than one host process (the packaged service plus a hand-started dev
  /// build, say) advertises once per process. They share a `host_identity`
  /// keypair, so they are one device with several ways in, not several
  /// devices; showing a tile each was confusing and made it easy to pick the
  /// one that happens not to work.
  final List<DiscoveredHost> alternates;

  /// Every endpoint for this identity, the primary first. Used to fail over
  /// when the first one doesn't answer.
  List<DiscoveredHost> get allEndpoints => [?discovered, ...alternates];

  /// Host platform from mDNS (`macos`/`windows`/`android`/`linux`/`ios`);
  /// empty for old hosts and for saved hosts that aren't currently online.
  String get platform => discovered?.platform ?? '';

  IconData get platformIcon => switch (platform) {
    'android' => Icons.smartphone,
    'ios' => Icons.phone_iphone,
    'macos' => Icons.laptop_mac,
    'windows' => Icons.desktop_windows,
    'linux' => Icons.computer,
    _ => Icons.computer,
  };

  /// The one address worth showing. Hosts advertise several (IPv4 plus a
  /// couple of IPv6 temporaries), and listing them all turned the tile into
  /// three lines of noise that also read as if the port belonged to the last
  /// IPv6 — the port is appended once, after the whole joined list.
  /// IPv4 is preferred because it's the one a human can recognise.
  String get preferredAddr {
    if (addrs.isEmpty) return '';
    return addrs.firstWhere((a) => !a.contains(':'), orElse: () => addrs.first);
  }

  String get addressLabel {
    final addr = preferredAddr;
    if (addr.isEmpty) return online ? 'On LAN' : 'Not on LAN';
    // Bracket IPv6 so `addr:port` isn't ambiguous with the address's own colons.
    final endpoint = addr.contains(':') ? '[$addr]:$port' : '$addr:$port';
    return online ? endpoint : 'Last seen at $endpoint';
  }
}

/// The saved + discovered PCs, shown at the TOP of the Watch screen — the
/// primary thing you came here to do.
class _DevicesSection extends StatelessWidget {
  const _DevicesSection({
    required this.busy,
    required this.hosts,
    required this.discovering,
    this.onHostTap,
    this.onForgetHost,
    this.onWakeHost,
    this.canWakeSaved = false,
  });

  final bool busy;
  final List<_HostListEntry> hosts;
  final bool discovering;
  final void Function(_HostListEntry host)? onHostTap;
  final void Function(_HostListEntry host)? onForgetHost;

  /// Wake-on-LAN: shown on the saved host while it's offline, when the app
  /// has cached wake facts for it (see `savedHostCanWake`).
  final void Function(_HostListEntry host)? onWakeHost;
  final bool canWakeSaved;

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
              icon: h.platformIcon,
              title: h.name,
              subtitle: h.online
                  ? h.needsPairing
                        ? 'Pair this device to start streaming'
                        : h.isSaved
                        ? 'Tap to start streaming'
                        : 'Set up a connection'
                  : h.addressLabel,
              active: h.online,
              onTap: canTap ? () => onHostTap!(h) : null,
              trailing: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  StatusChip(
                    // "Offline" during the very first discovery sweep is a
                    // lie that costs the user an action: a saved host reads as
                    // offline until mDNS answers, which prompted a
                    // Wake-on-LAN packet at a machine that was awake all along
                    // (2026-08-18). Say we're still looking instead.
                    label: h.online
                        ? h.needsPairing
                              ? 'Pair device'
                              : h.isSaved
                              ? 'Saved'
                              : 'Nearby'
                        : discovering
                        ? 'Checking…'
                        : 'Offline',
                    highlight: h.online,
                  ),
                  // Not gated on !online: a sleeping host can linger as
                  // "Online" in the discovery snapshot (mDNS cache) long
                  // enough to hide the one button that could wake it —
                  // observed live 2026-07-19. Waking an awake machine is a
                  // harmless no-op, so just always offer it.
                  if (h.isSaved && canWakeSaved && onWakeHost != null)
                    IconButton(
                      icon: const Icon(Icons.power_settings_new),
                      color: theme.colorScheme.primary,
                      tooltip: 'Wake ${h.name} (Wake-on-LAN)',
                      onPressed: busy ? null : () => onWakeHost!(h),
                    ),
                  if (h.isSaved && onForgetHost != null)
                    IconButton(
                      icon: const Icon(Icons.delete_outline),
                      tooltip: 'Forget ${h.name}',
                      onPressed: busy ? null : () => onForgetHost!(h),
                    )
                  else if (canTap)
                    IconButton(
                      icon: Icon(
                        h.isSaved && !h.needsPairing
                            ? Icons.play_circle_fill
                            : Icons.add_link,
                      ),
                      color: theme.colorScheme.primary,
                      tooltip: h.isSaved && !h.needsPairing
                          ? 'Watch ${h.name}'
                          : 'Set up ${h.name}',
                      onPressed: busy ? null : () => onHostTap!(h),
                    ),
                ],
              ),
            );
          }),
        if (hosts.isNotEmpty &&
            hosts.any((h) => h.hostPubkey.length != 32)) ...[
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
          const SizedBox(height: 16),
          OutlinedButton.icon(
            onPressed: openGithubReleases,
            icon: const Icon(Icons.download),
            label: const Text('Install StreamBeam on another device'),
          ),
        ],
      ),
    );
  }
}

/// Hybrid composition keeps MediaCodec's TextureView on the activity window.
/// A texture-backed platform view can replace its consumer during rotation.
class AndroidStreamView extends StatelessWidget {
  const AndroidStreamView({super.key});

  @override
  Widget build(BuildContext context) => PlatformViewLink(
    viewType: 'streambeam/video',
    surfaceFactory: (context, controller) => AndroidViewSurface(
      controller: controller as AndroidViewController,
      gestureRecognizers: const {},
      hitTestBehavior: PlatformViewHitTestBehavior.transparent,
    ),
    onCreatePlatformView: (params) {
      return PlatformViewsService.initExpensiveAndroidView(
          id: params.id,
          viewType: 'streambeam/video',
          layoutDirection: TextDirection.ltr,
          onFocus: () => params.onFocusChanged(true),
        )
        ..addOnPlatformViewCreatedListener(params.onPlatformViewCreated)
        ..create();
    },
  );
}
