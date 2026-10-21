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
  const BeamPage({super.key, this.embedded = false});

  /// Renders only the compact "This Device" controls for the unified home
  /// screen. The full page remains available for platform-specific fallback
  /// flows, but is no longer exposed as a separate navigation tab.
  final bool embedded;

  @override
  State<BeamPage> createState() => _BeamPageState();
}

class _BeamPageState extends State<BeamPage> with WidgetsBindingObserver {
  Timer? _poll;
  HostSnapshot _snap = HostSnapshot(
    running: false,
    hostName: '',
    listenAddr: '',
    lanAddrs: const [],
    discoverable: false,
    acceptsConnections: false,
    clients: [],
    viewers: [],
  );
  bool _starting = false;
  String? _status;
  // Secure-desktop service state ('unsupported' hides the toggle). Polled in
  // _refresh alongside the host snapshot.
  String _secureDesktop = 'unsupported';
  bool _secureDesktopBusy = false;
  bool _statusError = false;
  String? _shownHostError;
  BigInt? _shownApprovalId;
  String _notifiedViewers = '';

  static const _screencast = MethodChannel('streambeam/screencast');
  static const _remoteInput = MethodChannel('streambeam/remote_input');
  bool _remoteInputEnabled = false;
  bool? _macScreenRecordingGranted;
  bool? _macAccessibilityGranted;
  DateTime? _lastPermissionRefresh;

  /// Android starts capture explicitly. On macOS the host stays ready and the
  /// native runner toggles capture on demand when a client starts/stops.
  bool _beaming = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    // Every host stays ready: a paired client initiates the actual session.
    // Android raises its mandatory per-session MediaProjection picker only
    // after that authenticated request arrives.
    if (hostPlatformSupported()) {
      unawaited(_ensureHostRunning());
    }
    if (Platform.isMacOS) {
      _screencast.setMethodCallHandler(_handleNativeCaptureEvent);
    }
    _poll = Timer.periodic(
      const Duration(milliseconds: 250),
      (_) => _refresh(),
    );
    unawaited(_refreshPermissions(force: true));
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      unawaited(_refreshPermissions(force: true));
    }
  }

  Future<void> _refreshPermissions({bool force = false}) async {
    final now = DateTime.now();
    if (!force &&
        _lastPermissionRefresh != null &&
        now.difference(_lastPermissionRefresh!) < const Duration(seconds: 1)) {
      return;
    }
    _lastPermissionRefresh = now;
    if (Platform.isAndroid) {
      await _refreshRemoteInput();
    } else if (Platform.isMacOS) {
      try {
        final raw = await _screencast.invokeMethod<dynamic>('permissionStatus');
        final status = Map<Object?, Object?>.from(raw as Map);
        final screenRecording = status['screenRecording'] == true;
        final accessibility = status['accessibility'] == true;
        if (mounted &&
            (screenRecording != _macScreenRecordingGranted ||
                accessibility != _macAccessibilityGranted)) {
          setState(() {
            _macScreenRecordingGranted = screenRecording;
            _macAccessibilityGranted = accessibility;
          });
        }
      } catch (_) {}
    }
  }

  Future<void> _refreshRemoteInput() async {
    if (!Platform.isAndroid) return;
    try {
      final enabled =
          await _remoteInput.invokeMethod<bool>('isEnabled') ?? false;
      if (mounted && enabled != _remoteInputEnabled) {
        setState(() => _remoteInputEnabled = enabled);
      }
    } catch (_) {}
  }

  Future<void> _openRemoteInputSettings() async {
    await _remoteInput.invokeMethod<void>('openSettings');
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
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
      // Normally the host has been listening since app startup. Preserve the
      // manual button as a fallback without trying to bind the port twice.
      if (!hostSnapshot().running) {
        startHost(
          hostName: null,
          listen: null,
          fps: 30,
          maxEncodeDim: Platform.isMacOS ? 1920 : 1280,
        );
      }
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
    // When the secure-desktop service is running, IT owns the host (and the
    // listen port) from a SYSTEM worker sharing this user's identity — so the
    // in-app host must stand down or the two collide on 47989.
    if (Platform.isWindows && secureDesktopStatus() == 'running') return;
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

  Future<void> _toggleSecureDesktop(bool on) async {
    if (_secureDesktopBusy) return;
    setState(() => _secureDesktopBusy = true);
    setState(
      () => _status = on
          ? 'Approve the Windows prompt to let StreamBeam run behind the login screen…'
          : 'Approve the Windows prompt to turn this off…',
    );
    try {
      // Hand the listen port over cleanly: stop the in-app host before the
      // service's worker tries to bind it (and vice-versa on disable).
      if (on) {
        try {
          stopHost();
        } catch (_) {}
      }
      // Blocks on the UAC dialog; runs off the UI isolate so the app stays
      // responsive while the elevated helper works.
      await Future(() => secureDesktopSetEnabled(enable: on));
      if (!on) {
        // Service gone — bring the in-app host back so beaming still works.
        await _ensureHostRunning();
      }
      setState(() => _status = null);
    } catch (e) {
      _setStatus(e.toString(), error: true);
    } finally {
      if (mounted) {
        setState(() {
          _secureDesktopBusy = false;
          _secureDesktop = Platform.isWindows
              ? secureDesktopStatus()
              : 'unsupported';
        });
      }
    }
  }

  void _refresh() {
    if (!mounted || !hostPlatformSupported()) return;
    if (Platform.isAndroid) {
      unawaited(_refreshPermissions());
      unawaited(_refreshAndroidCapture());
    } else if (Platform.isMacOS) {
      unawaited(_refreshPermissions());
    }
    final next = hostSnapshot();
    final secure = Platform.isWindows ? secureDesktopStatus() : 'unsupported';
    setState(() {
      _snap = next;
      _secureDesktop = secure;
    });
    _syncViewerNotification(next.viewers);
    if (next.error != null && next.error!.isNotEmpty) {
      if (_shownHostError != next.error) {
        _shownHostError = next.error;
        _setStatus(next.error!, error: true);
      }
    } else {
      _shownHostError = null;
    }
    final pending = next.pendingApproval;
    // Android handles approval at Activity level, including while this page
    // is absent because the tablet is watching another device.
    if (!Platform.isAndroid &&
        pending != null &&
        pending.id != _shownApprovalId) {
      _shownApprovalId = pending.id;
      unawaited(_showApprovalDialog(pending));
    }
  }

  /// Where peers reach this device. Prefers IPv4 — it's the one a human can
  /// recognise and retype — and falls back to the bind address only when
  /// nothing better is known.
  String _reachableLabel() {
    final addrs = _snap.lanAddrs;
    if (addrs.isEmpty) {
      return _snap.listenAddr.isEmpty ? 'Starting…' : _snap.listenAddr;
    }
    final ipv4 = addrs.firstWhere(
      (a) => !a.startsWith('[') && a.split(':').length == 2,
      orElse: () => addrs.first,
    );
    final extra = addrs.length - 1;
    return extra > 0 ? '$ipv4  ·  +$extra more' : ipv4;
  }

  Future<void> _promptRename() async {
    final controller = TextEditingController(text: _snap.hostName);
    final name = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Rename this device'),
        content: TextField(
          controller: controller,
          autofocus: true,
          decoration: const InputDecoration(
            labelText: 'Device name',
            helperText: 'How this device appears to your other devices',
          ),
          onSubmitted: (v) => Navigator.of(context).pop(v),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(controller.text),
            child: const Text('Rename'),
          ),
        ],
      ),
    );
    if (name == null || name.trim().isEmpty || name.trim() == _snap.hostName) {
      return;
    }
    try {
      setCustomDeviceName(name: name.trim());
    } catch (e) {
      if (mounted) _setStatus('Could not rename: $e', error: true);
      return;
    }
    // The name is baked into the mDNS advertisement and the pairing payload
    // when the host starts, so a restart is what makes peers see it.
    stopHost();
    startHost(
      hostName: null,
      listen: null,
      fps: 30,
      maxEncodeDim: Platform.isMacOS ? 1920 : 1280,
    );
    if (mounted) {
      _refresh();
      _setStatus('Renamed to ${name.trim()}');
    }
  }

  Future<void> _refreshAndroidCapture() async {
    try {
      final active =
          await _screencast.invokeMethod<bool>('isCaptureActive') ?? false;
      if (mounted && active != _beaming) setState(() => _beaming = active);
    } catch (_) {}
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
    if (error) {
      showErrorToast(message);
      setState(() {
        _status = null;
        _statusError = false;
      });
      return;
    }
    setState(() {
      _status = message;
      _statusError = false;
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

  Future<void> _showPairingQr() async {
    final link = _snap.pairingLink;
    if (link == null || link.isEmpty || !mounted) return;
    await showDialog<void>(
      context: context,
      builder: (dialogContext) => Dialog(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 360),
          child: Padding(
            padding: const EdgeInsets.all(20),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(
                  'Pair this device',
                  style: Theme.of(dialogContext).textTheme.headlineSmall,
                ),
                const SizedBox(height: 16),
                Center(
                  child: Container(
                    width: 264,
                    height: 264,
                    padding: const EdgeInsets.all(12),
                    decoration: BoxDecoration(
                      color: Colors.white,
                      borderRadius: BorderRadius.circular(12),
                    ),
                    child: QrImageView(
                      data: link,
                      size: 240,
                      backgroundColor: Colors.white,
                    ),
                  ),
                ),
                const SizedBox(height: 12),
                const Text(
                  'Scan once to pair. After approval, this device can start future beams directly.',
                  textAlign: TextAlign.center,
                ),
                const SizedBox(height: 16),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    TextButton.icon(
                      onPressed: _copyLink,
                      icon: const Icon(Icons.link),
                      label: const Text('Copy link'),
                    ),
                    const SizedBox(width: 8),
                    FilledButton(
                      onPressed: () => Navigator.pop(dialogContext),
                      child: const Text('Done'),
                    ),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  Future<void> _showPermissionHelp({required String permission}) async {
    final androidAccessibility =
        Platform.isAndroid && permission == 'accessibility';
    final screenRecording = permission == 'screenRecording';
    final title = androidAccessibility
        ? 'Allow remote control'
        : screenRecording
        ? 'Allow Screen Recording'
        : 'Allow Accessibility';
    final instructions = androidAccessibility
        ? 'StreamBeam uses Android Accessibility to let a paired device tap, swipe and type on this device. To edit text, it reads and changes the focused text field.\n\n'
              'This happens locally when you use remote control. Accessibility text is not sent to StreamBeam servers. Your paired viewer can see text that appears in the screen you share.\n\n'
              '1. Open Accessibility settings.\n'
              '2. Find StreamBeam under installed or downloaded apps.\n'
              '3. Enable “Use StreamBeam” and confirm the warning.\n\n'
              'StreamBeam only uses this while a paired device controls this screen.'
        : screenRecording
        ? 'Open System Settings → Privacy & Security → Screen & System Audio Recording, then enable StreamBeam.\n\n'
              'If access still fails while enabled, remove the old StreamBeam entry and add the currently installed app again. Then restart StreamBeam.'
        : 'Open System Settings → Privacy & Security → Accessibility, then enable StreamBeam.\n\n'
              'This lets paired devices send mouse and keyboard input to this Mac.';
    if (!mounted) return;
    final open = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => AlertDialog(
        icon: const Icon(Icons.admin_panel_settings_outlined),
        title: Text(title),
        content: Text(instructions),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(dialogContext, false),
            child: const Text('Not now'),
          ),
          FilledButton.icon(
            onPressed: () => Navigator.pop(dialogContext, true),
            icon: const Icon(Icons.settings),
            label: Text(
              androidAccessibility
                  ? 'Agree and open settings'
                  : 'Open settings',
            ),
          ),
        ],
      ),
    );
    if (open != true) return;
    try {
      if (androidAccessibility) {
        await _openRemoteInputSettings();
      } else {
        await _screencast.invokeMethod<void>('openPermissionSettings', {
          'permission': permission,
        });
      }
    } catch (e) {
      _setStatus(e.toString(), error: true);
    }
  }

  void _stopViewer(HostClientInfo viewer) {
    try {
      final stopped = hostDisconnectClient(addr: viewer.addr);
      if (!stopped) {
        throw StateError('${viewer.name} is no longer connected.');
      }
      setState(() {
        _snap = HostSnapshot(
          running: _snap.running,
          hostName: _snap.hostName,
          listenAddr: _snap.listenAddr,
          lanAddrs: _snap.lanAddrs,
          discoverable: _snap.discoverable,
          acceptsConnections: _snap.acceptsConnections,
          pairingLink: _snap.pairingLink,
          clients: _snap.clients,
          viewers: _snap.viewers
              .where((item) => item.addr != viewer.addr)
              .toList(),
          error: _snap.error,
          pendingApproval: _snap.pendingApproval,
        );
      });
    } catch (e) {
      _setStatus(e.toString(), error: true);
    }
  }

  Widget _buildThisDevice(BuildContext context) {
    final theme = Theme.of(context);
    final viewers = _snap.viewers;
    final running = _snap.running;
    final link = _snap.pairingLink;
    final missingAndroidAccessibility =
        Platform.isAndroid && !_remoteInputEnabled;
    final missingMacScreenRecording =
        Platform.isMacOS && _macScreenRecordingGranted == false;
    final missingMacAccessibility =
        Platform.isMacOS && _macAccessibilityGranted == false;
    final hasMissingPermissions =
        missingAndroidAccessibility ||
        missingMacScreenRecording ||
        missingMacAccessibility;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SectionHeader(
          'This Device',
          trailing: Text(
            !running
                ? 'Starting…'
                : viewers.isEmpty
                ? 'Ready'
                : '${viewers.length} watching',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onSurfaceVariant,
            ),
          ),
        ),
        Card(
          clipBehavior: Clip.antiAlias,
          child: Column(
            children: [
              // Identity row, matching how every other device is presented in
              // the list above: name, and the address peers reach it on. This
              // device previously showed neither, so there was no way to tell
              // what others see you as, or which address to type.
              ListTile(
                leading: Icon(
                  Platform.isAndroid
                      ? Icons.smartphone
                      : Platform.isMacOS
                      ? Icons.laptop_mac
                      : Icons.desktop_windows,
                  color: theme.colorScheme.primary,
                ),
                title: Text(
                  _snap.hostName.isEmpty ? 'This device' : _snap.hostName,
                  style: theme.textTheme.titleSmall,
                ),
                subtitle: Text(
                  _reachableLabel(),
                  style: theme.textTheme.bodySmall,
                ),
                trailing: IconButton(
                  icon: const Icon(Icons.edit_outlined, size: 20),
                  tooltip: 'Rename this device',
                  onPressed: running ? _promptRename : null,
                ),
              ),
              const Divider(height: 1),
              if (viewers.isNotEmpty) ...[
                ColoredBox(
                  color: theme.colorScheme.primaryContainer,
                  child: Column(
                    children: [
                      for (final viewer in viewers)
                        ListTile(
                          leading: Icon(
                            Icons.visibility,
                            color: theme.colorScheme.onPrimaryContainer,
                          ),
                          title: Text('${viewer.name} is watching'),
                          subtitle: Text(
                            'Watching this device · ${viewer.addr}',
                          ),
                          trailing: TextButton(
                            onPressed: () => _stopViewer(viewer),
                            child: const Text('Stop'),
                          ),
                        ),
                    ],
                  ),
                ),
                const Divider(height: 1),
              ],
              if (hasMissingPermissions) ...[
                Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    Padding(
                      padding: const EdgeInsets.fromLTRB(16, 14, 16, 4),
                      child: Align(
                        alignment: Alignment.centerLeft,
                        child: DecoratedBox(
                          decoration: BoxDecoration(
                            color: theme.colorScheme.errorContainer.withValues(
                              alpha: 0.55,
                            ),
                            borderRadius: BorderRadius.circular(999),
                          ),
                          child: Padding(
                            padding: const EdgeInsets.symmetric(
                              horizontal: 10,
                              vertical: 5,
                            ),
                            child: Row(
                              mainAxisSize: MainAxisSize.min,
                              children: [
                                Icon(
                                  Icons.info_outline,
                                  size: 16,
                                  color: theme.colorScheme.onErrorContainer
                                      .withValues(alpha: 0.8),
                                ),
                                const SizedBox(width: 6),
                                Text(
                                  'Permissions needed',
                                  style: theme.textTheme.labelMedium?.copyWith(
                                    color: theme.colorScheme.onErrorContainer,
                                    fontWeight: FontWeight.w600,
                                  ),
                                ),
                              ],
                            ),
                          ),
                        ),
                      ),
                    ),
                    if (missingAndroidAccessibility)
                      ListTile(
                        leading: const Icon(Icons.touch_app_outlined),
                        title: const Text('Remote control is disabled'),
                        subtitle: const Text(
                          'Allow Accessibility access for touch and keyboard control',
                        ),
                        trailing: const Icon(Icons.chevron_right),
                        onTap: () => unawaited(
                          _showPermissionHelp(permission: 'accessibility'),
                        ),
                      ),
                    if (missingMacScreenRecording)
                      ListTile(
                        leading: const Icon(Icons.screen_share_outlined),
                        title: const Text('Screen Recording is disabled'),
                        subtitle: const Text(
                          'Allow access so this Mac can share its screen',
                        ),
                        trailing: const Icon(Icons.chevron_right),
                        onTap: () => unawaited(
                          _showPermissionHelp(permission: 'screenRecording'),
                        ),
                      ),
                    if (missingMacAccessibility)
                      ListTile(
                        leading: const Icon(Icons.keyboard_alt_outlined),
                        title: const Text('Remote input is disabled'),
                        subtitle: const Text(
                          'Allow Accessibility access for mouse and keyboard control',
                        ),
                        trailing: const Icon(Icons.chevron_right),
                        onTap: () => unawaited(
                          _showPermissionHelp(permission: 'accessibility'),
                        ),
                      ),
                  ],
                ),
                const Divider(height: 1),
              ],
              ListTile(
                leading: const Icon(Icons.qr_code_2),
                title: const Text('Pair another device'),
                subtitle: const Text('Show this device’s QR code'),
                trailing: FilledButton.tonal(
                  onPressed: link == null || link.isEmpty
                      ? null
                      : () => unawaited(_showPairingQr()),
                  child: const Text('Show QR'),
                ),
              ),
              const Divider(height: 1),
              SwitchListTile(
                secondary: const Icon(Icons.wifi_find),
                title: const Text('Discoverable'),
                subtitle: const Text(
                  'Let paired devices find this one on the local network',
                ),
                value: _snap.discoverable,
                onChanged: running
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
              if (_secureDesktop != 'unsupported') ...[
                const Divider(height: 1),
                SwitchListTile(
                  secondary: const Icon(Icons.lock_open),
                  title: const Text('Allow access when locked'),
                  subtitle: Text(
                    _secureDesktop == 'running'
                        ? 'On — streaming survives lock, logout and the login screen'
                        : 'Keep streaming (and let you sign in remotely) while '
                              'this PC is locked or at the login screen',
                  ),
                  value: _secureDesktop == 'running',
                  onChanged: _secureDesktopBusy
                      ? null
                      : (on) => unawaited(_toggleSecureDesktop(on)),
                ),
              ],
            ],
          ),
        ),
      ],
    );
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

    if (widget.embedded) {
      return _buildThisDevice(context);
    }

    final theme = Theme.of(context);

    // Keep the Android setup page approachable before the idle host snapshot
    // arrives. Once listening, show the same ready/pairing UI as desktop.
    if (Platform.isAndroid && !_beaming && !_snap.running) {
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
                  const SizedBox(height: 16),
                  Card(
                    child: ListTile(
                      leading: Icon(
                        _remoteInputEnabled
                            ? Icons.touch_app
                            : Icons.touch_app_outlined,
                      ),
                      title: const Text('Remote touch control'),
                      subtitle: Text(
                        _remoteInputEnabled
                            ? 'Enabled — paired viewers can send native multi-touch'
                            : 'Enable Android Accessibility gesture access',
                      ),
                      trailing: _remoteInputEnabled
                          ? const Icon(Icons.check_circle, color: Colors.green)
                          : TextButton(
                              onPressed: () =>
                                  unawaited(_openRemoteInputSettings()),
                              child: const Text('Set up'),
                            ),
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
                        StatusNotice(message: _status!, isError: false),
                      ],
                    ],
                  ),
                ),
              ),
      ),
    );
  }
}

/// One shared home surface: remote devices first, followed by controls and
/// status for this device. Streaming itself still becomes immersive inside
/// [WatchPage], without swapping the surrounding widget tree.
class AppShell extends StatelessWidget {
  const AppShell({super.key});

  @override
  Widget build(BuildContext context) {
    final caps = appCapabilities();

    if (!caps.anySupported) {
      return _DesktopUnsupportedPage(platformLabel: caps.platformLabel);
    }

    return const WatchPage();
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
