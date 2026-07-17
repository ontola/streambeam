import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:path_provider/path_provider.dart';
import 'package:qr_flutter/qr_flutter.dart';

import 'main.dart';
import 'src/rust/api.dart';
import 'ui.dart';

const _logoAsset = 'assets/streambeam-logo.svg';

/// What this StreamBeam build can do on the current OS.
///
/// `host` = can beam (send this screen). `watch` = can receive & play a stream.
class AppCapabilities {
  const AppCapabilities({
    required this.host,
    required this.watch,
    required this.platformLabel,
  });

  final bool host;
  final bool watch;
  final String platformLabel;

  bool get anySupported => host || watch;
}

AppCapabilities appCapabilities() {
  if (Platform.isAndroid) {
    // Android both watches and beams (screen capture via MediaProjection).
    return const AppCapabilities(
      host: true,
      watch: true,
      platformLabel: 'Android',
    );
  }
  final host = hostPlatformSupported();
  // Desktop decode/watch is not wired yet on any desktop OS.
  const watch = false;
  if (Platform.isWindows) {
    return AppCapabilities(host: host, watch: watch, platformLabel: 'Windows');
  }
  if (Platform.isMacOS) {
    return AppCapabilities(host: host, watch: true, platformLabel: 'macOS');
  }
  if (Platform.isLinux) {
    return const AppCapabilities(
      host: false,
      watch: false,
      platformLabel: 'Linux',
    );
  }
  return AppCapabilities(
    host: host,
    watch: watch,
    platformLabel: 'this platform',
  );
}

/// Shared host UI — pair via QR, manage connected viewers.
class BeamPage extends StatefulWidget {
  const BeamPage({super.key});

  @override
  State<BeamPage> createState() => _BeamPageState();
}

class _BeamPageState extends State<BeamPage> {
  Timer? _poll;
  HostSnapshot _snap = HostSnapshot(
    running: false,
    hostName: '',
    listenAddr: '',
    discoverable: false,
    acceptsConnections: false,
    clients: [],
    viewers: [],
  );
  bool _starting = false;
  String? _status;
  bool _statusError = false;
  BigInt? _shownApprovalId;
  String _notifiedViewers = '';

  static const _screencast = MethodChannel('streambeam/screencast');

  /// Android starts capture explicitly. On macOS the host stays ready and the
  /// native runner toggles capture on demand when a client starts/stops.
  bool _beaming = false;

  @override
  void initState() {
    super.initState();
    // Desktop hosts stay ready: a paired client initiates the actual session.
    // Android still needs its per-session MediaProjection picker.
    if (hostPlatformSupported() && (Platform.isWindows || Platform.isMacOS)) {
      unawaited(_ensureHostRunning());
    }
    if (Platform.isMacOS) {
      _screencast.setMethodCallHandler(_handleNativeCaptureEvent);
    }
    _poll = Timer.periodic(const Duration(seconds: 1), (_) => _refresh());
  }

  @override
  void dispose() {
    _poll?.cancel();
    if (Platform.isMacOS) {
      unawaited(
        _screencast.invokeMethod<void>('setActiveViewers', {
          'names': <String>[],
        }),
      );
      _screencast.setMethodCallHandler(null);
    }
    super.dispose();
  }

  Future<dynamic> _handleNativeCaptureEvent(MethodCall call) async {
    if (call.method != 'captureState' || !mounted) return;
    final args = Map<Object?, Object?>.from(call.arguments as Map);
    final active = args['active'] == true;
    final error = args['error'] as String?;
    setState(() => _beaming = active);
    if (error != null && error.isNotEmpty) {
      _setStatus(
        'Could not start this beam: $error. Allow Screen Recording for StreamBeam and retry from the client.',
        error: true,
      );
    } else if (active) {
      setState(() => _status = null);
    }
  }

  Future<void> _startBeaming() async {
    if (_starting) return;
    setState(() {
      _starting = true;
      _status = null;
    });
    try {
      final granted =
          await _screencast.invokeMethod<bool>('requestCapture') ?? false;
      if (!granted) {
        _setStatus('Screen capture permission was declined.', error: true);
        return;
      }
      final inputGranted =
          !Platform.isMacOS ||
          (await _screencast.invokeMethod<bool>('requestInputAccess') ?? false);
      // The capture service is now feeding frames; start the host server, which
      // picks up the live screen encoder.
      startHost(
        hostName: null,
        listen: null,
        fps: 30,
        maxEncodeDim: Platform.isMacOS ? 1920 : 1280,
      );
      _beaming = true;
      _refresh();
      if (!inputGranted) {
        _setStatus(
          'Streaming is live. Enable StreamBeam in System Settings > Privacy & Security > Accessibility for remote input.',
          error: true,
        );
      }
    } catch (e) {
      try {
        await _screencast.invokeMethod('stopCapture');
      } catch (_) {}
      _setStatus(e.toString(), error: true);
    } finally {
      if (mounted) setState(() => _starting = false);
    }
  }

  Future<void> _stopBeaming() async {
    try {
      stopHost();
    } catch (_) {}
    try {
      await _screencast.invokeMethod('stopCapture');
    } catch (_) {}
    if (mounted) {
      setState(() {
        _beaming = false;
        _status = null;
      });
    }
  }

  Future<void> _ensureHostRunning() async {
    if (_starting || _snap.running) return;
    setState(() => _starting = true);
    try {
      startHost(hostName: null, listen: null, fps: 30, maxEncodeDim: 1920);
      _refresh();
    } catch (e) {
      _setStatus(e.toString(), error: true);
    } finally {
      if (mounted) setState(() => _starting = false);
    }
  }

  void _refresh() {
    if (!mounted || !hostPlatformSupported()) return;
    final next = hostSnapshot();
    setState(() => _snap = next);
    _syncViewerNotification(next.viewers);
    if (next.error != null && next.error!.isNotEmpty) {
      _setStatus(next.error!, error: true);
    }
    final pending = next.pendingApproval;
    if (pending != null && pending.id != _shownApprovalId) {
      _shownApprovalId = pending.id;
      unawaited(_showApprovalDialog(pending));
    }
  }

  void _syncViewerNotification(List<HostClientInfo> viewers) {
    if (!Platform.isMacOS) return;
    final names = viewers.map((viewer) => viewer.name).toList()..sort();
    final key = names.join('\u0000');
    if (key == _notifiedViewers) return;
    _notifiedViewers = key;
    unawaited(
      _screencast.invokeMethod<void>('setActiveViewers', {'names': names}),
    );
  }

  void _setStatus(String message, {bool error = false}) {
    if (!mounted) return;
    setState(() {
      _status = message;
      _statusError = error;
    });
  }

  Future<void> _showApprovalDialog(HostPairRequest req) async {
    if (!mounted) return;
    final allow = await showDialog<bool>(
      context: context,
      barrierDismissible: false,
      builder: (ctx) => AlertDialog(
        title: const Text('Pair with this device?'),
        content: Text(
          '${req.clientName} at ${req.clientAddr} wants to pair.\n\n'
          'This is a one-time approval. Once paired, this device can start future beams without asking again.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Deny'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Pair'),
          ),
        ],
      ),
    );
    if (allow == null) return;
    try {
      hostRespondPairing(id: req.id, allow: allow);
      if (allow && Platform.isMacOS) {
        final inputGranted =
            await _screencast.invokeMethod<bool>('requestInputAccess') ?? false;
        if (!inputGranted) {
          _setStatus(
            'Paired. Enable StreamBeam in System Settings > Privacy & Security > Accessibility for remote input.',
            error: true,
          );
        }
      }
    } catch (e) {
      _setStatus(e.toString(), error: true);
    }
  }

  Future<void> _copyLink() async {
    final link = _snap.pairingLink;
    if (link == null || link.isEmpty) return;
    await Clipboard.setData(ClipboardData(text: link));
    if (mounted) {
      ScaffoldMessenger.of(
        context,
      ).showSnackBar(const SnackBar(content: Text('Pairing link copied')));
    }
  }

  @override
  Widget build(BuildContext context) {
    if (!hostPlatformSupported()) {
      return const Center(
        child: Padding(
          padding: EdgeInsets.all(24),
          child: Text(
            'Beaming this screen isn\'t supported on this platform yet.',
            textAlign: TextAlign.center,
          ),
        ),
      );
    }

    final theme = Theme.of(context);

    // Android needs a fresh MediaProjection grant before its host can listen.
    if (Platform.isAndroid && !_beaming) {
      return Scaffold(
        appBar: streamBeamAppBar(),
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(32),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 420),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(Icons.cast, size: 48, color: theme.colorScheme.primary),
                  const SizedBox(height: 16),
                  Text(
                    'Beam this screen',
                    style: theme.textTheme.titleMedium,
                    textAlign: TextAlign.center,
                  ),
                  const SizedBox(height: 8),
                  Text(
                    'Share this phone\'s screen to another device on your Wi‑Fi. '
                    'Android will ask permission to capture your screen.',
                    textAlign: TextAlign.center,
                    style: theme.textTheme.bodyMedium?.copyWith(
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                  ),
                  const SizedBox(height: 24),
                  FilledButton.icon(
                    onPressed: _starting
                        ? null
                        : () => unawaited(_startBeaming()),
                    icon: _starting
                        ? const SizedBox(
                            width: 18,
                            height: 18,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : const Icon(Icons.play_arrow),
                    label: Text(_starting ? 'Starting…' : 'Start beaming'),
                  ),
                  if (_status != null) ...[
                    const SizedBox(height: 16),
                    Text(
                      _status!,
                      textAlign: TextAlign.center,
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: _statusError ? theme.colorScheme.error : null,
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

    final link = _snap.pairingLink;
    final busy = _starting && !_snap.running;

    final connectedDevicesSection = [
      const SectionHeader('Connected devices'),
      ..._snap.clients.map(
        (c) => DeviceTile(
          icon: Icons.smartphone,
          title: c.name,
          subtitle: c.addr,
          active: true,
          trailing: Wrap(
            spacing: 4,
            children: [
              TextButton(
                onPressed: () {
                  try {
                    hostDisconnectClient(addr: c.addr);
                    _refresh();
                  } catch (e) {
                    _setStatus(e.toString(), error: true);
                  }
                },
                child: const Text('Stop'),
              ),
              TextButton(
                onPressed: () {
                  try {
                    hostForgetClient(addr: c.addr);
                    _refresh();
                  } catch (e) {
                    _setStatus(e.toString(), error: true);
                  }
                },
                child: const Text('Remove'),
              ),
            ],
          ),
        ),
      ),
    ];

    return Scaffold(
      body: SafeArea(
        child: busy
            ? const Center(child: CircularProgressIndicator())
            : RefreshIndicator(
                onRefresh: () async => _refresh(),
                child: CenteredContent(
                  child: ListView(
                    padding: const EdgeInsets.all(16),
                    children: [
                      if (_snap.clients.isNotEmpty) ...[
                        ...connectedDevicesSection,
                        const SizedBox(height: 12),
                      ],
                      Card(
                        child: Padding(
                          padding: const EdgeInsets.all(16),
                          child: Column(
                            crossAxisAlignment: CrossAxisAlignment.stretch,
                            children: [
                              Text(
                                _snap.hostName.isNotEmpty
                                    ? _snap.hostName
                                    : 'StreamBeam host',
                                style: theme.textTheme.titleMedium?.copyWith(
                                  fontWeight: FontWeight.w600,
                                ),
                              ),
                              const SizedBox(height: 4),
                              Text(
                                !_snap.running
                                    ? 'Starting host…'
                                    : _beaming
                                    ? 'Beaming now'
                                    : 'Ready — start from a paired device',
                                style: theme.textTheme.bodySmall?.copyWith(
                                  color: theme.colorScheme.onSurfaceVariant,
                                ),
                              ),
                              if (_snap.running) ...[
                                const SizedBox(height: 2),
                                Text(
                                  'Listening on ${_snap.listenAddr}',
                                  style: theme.textTheme.bodySmall?.copyWith(
                                    color: theme.colorScheme.onSurfaceVariant,
                                  ),
                                ),
                              ],
                              const SizedBox(height: 16),
                              Center(
                                child: link != null && link.isNotEmpty
                                    ? Container(
                                        padding: const EdgeInsets.all(12),
                                        decoration: BoxDecoration(
                                          color: Colors.white,
                                          borderRadius: BorderRadius.circular(
                                            12,
                                          ),
                                        ),
                                        child: QrImageView(
                                          data: link,
                                          size: 220,
                                          backgroundColor: Colors.white,
                                        ),
                                      )
                                    : const SizedBox(
                                        height: 220,
                                        child: Center(
                                          child: CircularProgressIndicator(),
                                        ),
                                      ),
                              ),
                              const SizedBox(height: 12),
                              Text(
                                'Scan once to pair this device. After approval, it can start future beams directly.',
                                textAlign: TextAlign.center,
                                style: theme.textTheme.bodySmall?.copyWith(
                                  color: theme.colorScheme.onSurfaceVariant,
                                ),
                              ),
                              const SizedBox(height: 12),
                              FilledButton.icon(
                                onPressed: link == null
                                    ? null
                                    : () => _copyLink(),
                                icon: const Icon(Icons.link),
                                label: const Text('Copy link'),
                              ),
                              if (Platform.isAndroid) ...[
                                const SizedBox(height: 8),
                                OutlinedButton.icon(
                                  onPressed: () => unawaited(_stopBeaming()),
                                  icon: const Icon(Icons.stop),
                                  label: const Text('Stop beaming'),
                                ),
                              ],
                            ],
                          ),
                        ),
                      ),
                      const SizedBox(height: 12),
                      const SectionHeader('Settings'),
                      Card(
                        child: Column(
                          children: [
                            SwitchListTile(
                              title: const Text('Discoverable on LAN'),
                              subtitle: const Text(
                                'Other devices can find this one on the local network',
                              ),
                              value: _snap.discoverable,
                              onChanged: _snap.running
                                  ? (on) {
                                      try {
                                        hostSetDiscoverable(on_: on);
                                        _refresh();
                                      } catch (e) {
                                        _setStatus(e.toString(), error: true);
                                      }
                                    }
                                  : null,
                            ),
                            const Divider(height: 1),
                            SwitchListTile(
                              title: const Text('Accept new connections'),
                              subtitle: const Text(
                                'Turn off to pause pairing and streaming',
                              ),
                              value: _snap.acceptsConnections,
                              onChanged: _snap.running
                                  ? (on) {
                                      try {
                                        hostSetAcceptsConnections(on_: on);
                                        _refresh();
                                      } catch (e) {
                                        _setStatus(e.toString(), error: true);
                                      }
                                    }
                                  : null,
                            ),
                          ],
                        ),
                      ),
                      if (_status != null) ...[
                        const SizedBox(height: 12),
                        StatusNotice(
                          message: _status!,
                          isError: _statusError,
                          onDismiss: () => setState(() => _status = null),
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

/// App shell shared by every platform: two tabs — **Watch** (stream *from*
/// another device) and **Beam** (stream *to* another device). Each platform
/// leads with the tab it actually supports (phones watch, desktops beam) and
/// shows a "coming soon" placeholder for the other.
class AppShell extends StatefulWidget {
  const AppShell({super.key});

  @override
  State<AppShell> createState() => _AppShellState();
}

class _AppShellState extends State<AppShell> {
  int _index = 0;

  @override
  Widget build(BuildContext context) {
    final caps = appCapabilities();

    if (!caps.anySupported) {
      return _DesktopUnsupportedPage(platformLabel: caps.platformLabel);
    }

    // Both tabs use the same pages on every platform ("Watch" = stream FROM
    // another device, "Beam" = stream TO another device). Each page gates its
    // own not-yet-wired functionality per platform (e.g. WatchPage shows a
    // "coming soon" on desktop playback; BeamPage on a host it can't run).
    final Widget watchTab = const WatchPage();
    final Widget beamTab = const BeamPage();

    const watchDest = NavigationDestination(
      icon: Icon(Icons.download_for_offline_outlined),
      selectedIcon: Icon(Icons.download_for_offline),
      label: 'Watch',
    );
    const beamDest = NavigationDestination(
      icon: Icon(Icons.cast),
      selectedIcon: Icon(Icons.cast_connected),
      label: 'Beam',
    );

    // Lead with the tab this platform supports.
    final List<Widget> pages;
    final List<NavigationDestination> destinations;
    if (caps.host && !caps.watch) {
      pages = [beamTab, watchTab];
      destinations = [beamDest, watchDest];
    } else {
      pages = [watchTab, beamTab];
      destinations = [watchDest, beamDest];
    }

    // While a tab is playing fullscreen video, drop the tab bar so the stream
    // fills the screen (and the immersive player isn't framed by chrome).
    return ValueListenableBuilder<bool>(
      valueListenable: appImmersive,
      builder: (context, immersive, _) {
        // `content` (and the WatchPage/BeamPage State it keeps alive via
        // IndexedStack) must stay at the *same position* in the tree across
        // every branch below — Flutter can't reconcile a widget that moves
        // to a different parent/depth between rebuilds (no matching
        // ancestor to diff against), so it tears down and rebuilds the
        // whole subtree's State instead of preserving it. That previously
        // happened here: tapping a device to connect flips `appImmersive`
        // true, which used to make this builder return a *differently
        // shaped* tree in landscape (dropping the Row/NavigationRail
        // wrapper) — destroying WatchPage's State (and the connection
        // attempt it had just started) the instant the tap set it in
        // motion. Second tap "worked" only because by then `appImmersive`
        // was already true, so this builder didn't rebuild at all. Always
        // building the same Row/Expanded/NavigationRail shape and only
        // toggling which parts are *visible* keeps one stable position for
        // `content` no matter what changes.
        final content = IndexedStack(
          index: _index.clamp(0, pages.length - 1),
          children: pages,
        );

        final media = MediaQuery.of(context);
        final isPhoneLandscape =
            media.orientation == Orientation.landscape &&
            media.size.shortestSide < 600;
        final showChrome = !immersive;
        final showRail = showChrome && isPhoneLandscape;
        final showBottomBar = showChrome && !isPhoneLandscape;

        final body = Row(
          children: [
            Expanded(child: content),
            if (showRail) ...[
              const VerticalDivider(width: 1),
              NavigationRail(
                selectedIndex: _index.clamp(0, destinations.length - 1),
                onDestinationSelected: (i) => setState(() => _index = i),
                labelType: NavigationRailLabelType.all,
                destinations: [
                  for (final d in destinations)
                    NavigationRailDestination(
                      icon: d.icon,
                      selectedIcon: d.selectedIcon,
                      label: Text(d.label),
                    ),
                ],
              ),
            ],
          ],
        );

        return Scaffold(
          // The on-screen system nav bar (back/home/recents) sits along
          // whichever edge is physically "down" in landscape — often the
          // same right edge the rail lives on — so without a SafeArea the
          // rail's destinations render right underneath/behind it. Applied
          // unconditionally (not just when the rail shows) so this doesn't
          // change `body`'s ancestry depth between states either. `top:
          // false` — WatchPage's own SliverAppBar already reserves the
          // status-bar inset itself (`primary: true`); doubling it here
          // would push its header down an extra status-bar's worth.
          body: SafeArea(top: false, child: body),
          bottomNavigationBar: showBottomBar
              ? NavigationBar(
                  selectedIndex: _index.clamp(0, destinations.length - 1),
                  onDestinationSelected: (i) => setState(() => _index = i),
                  destinations: destinations,
                )
              : null,
        );
      },
    );
  }
}

class _DesktopUnsupportedPage extends StatelessWidget {
  const _DesktopUnsupportedPage({required this.platformLabel});

  final String platformLabel;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
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
      ),
      body: Center(
        child: Padding(
          padding: const EdgeInsets.all(32),
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 420),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  Icons.desktop_mac_outlined,
                  size: 48,
                  color: theme.colorScheme.primary,
                ),
                const SizedBox(height: 16),
                Text(
                  'Not available on $platformLabel yet',
                  style: theme.textTheme.titleMedium,
                  textAlign: TextAlign.center,
                ),
                const SizedBox(height: 12),
                Text(
                  'This build does not support hosting or watching streams on '
                  '$platformLabel.',
                  textAlign: TextAlign.center,
                  style: theme.textTheme.bodyMedium?.copyWith(
                    color: theme.colorScheme.onSurfaceVariant,
                  ),
                ),
                const SizedBox(height: 24),
                _UnsupportedFeatureRow(
                  icon: Icons.cast,
                  title: 'Beam my screen',
                  subtitle: 'Host your screen for phones and other devices',
                  supported: false,
                ),
                const SizedBox(height: 12),
                _UnsupportedFeatureRow(
                  icon: Icons.play_circle_outline,
                  title: 'Watch a stream',
                  subtitle: 'Connect to another StreamBeam host on this device',
                  supported: false,
                ),
                const SizedBox(height: 24),
                Text(
                  'Use StreamBeam on Android to watch, or on Windows to host '
                  'and pair with your phone.',
                  textAlign: TextAlign.center,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.onSurfaceVariant,
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

class _UnsupportedFeatureRow extends StatelessWidget {
  const _UnsupportedFeatureRow({
    required this.icon,
    required this.title,
    required this.subtitle,
    required this.supported,
  });

  final IconData icon;
  final String title;
  final String subtitle;
  final bool supported;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Icon(
          icon,
          size: 22,
          color: supported
              ? theme.colorScheme.primary
              : theme.colorScheme.onSurfaceVariant,
        ),
        const SizedBox(width: 12),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(title, style: theme.textTheme.titleSmall),
              Text(
                supported ? subtitle : 'Not supported on this platform yet',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: theme.colorScheme.onSurfaceVariant,
                ),
              ),
            ],
          ),
        ),
        Icon(
          supported ? Icons.check_circle_outline : Icons.block,
          size: 20,
          color: supported
              ? theme.colorScheme.primary
              : theme.colorScheme.onSurfaceVariant,
        ),
      ],
    );
  }
}

Future<String> streambeamDataDir() async {
  if (Platform.isWindows) {
    final appdata = Platform.environment['APPDATA'];
    if (appdata != null) {
      final dir = Directory('$appdata/streambeam');
      await dir.create(recursive: true);
      return dir.path;
    }
  }
  if (Platform.isLinux || Platform.isMacOS) {
    final home = Platform.environment['HOME'];
    if (home != null) {
      final dir = Directory('$home/.streambeam');
      await dir.create(recursive: true);
      return dir.path;
    }
  }
  final dir = await getApplicationDocumentsDirectory();
  return dir.path;
}
