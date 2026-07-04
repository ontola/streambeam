import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:path_provider/path_provider.dart';
import 'package:qr_flutter/qr_flutter.dart';

import 'src/rust/api.dart';

const _logoAsset = 'assets/streambeam-logo.svg';

/// Windows desktop host UI — pair via QR, manage connected phones.
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
  );
  bool _starting = false;
  String? _status;
  bool _statusError = false;
  BigInt? _shownApprovalId;

  @override
  void initState() {
    super.initState();
    if (hostPlatformSupported()) {
      unawaited(_ensureHostRunning());
    }
    _poll = Timer.periodic(const Duration(seconds: 1), (_) => _refresh());
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }

  Future<void> _ensureHostRunning() async {
    if (_starting || _snap.running) return;
    setState(() => _starting = true);
    try {
      startHost(
        hostName: null,
        listen: null,
        fps: 30,
        maxEncodeDim: 1920,
      );
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
    if (next.error != null && next.error!.isNotEmpty) {
      _setStatus(next.error!, error: true);
    }
    final pending = next.pendingApproval;
    if (pending != null && pending.id != _shownApprovalId) {
      _shownApprovalId = pending.id;
      unawaited(_showApprovalDialog(pending));
    }
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
        title: const Text('Allow this device?'),
        content: Text(
          '${req.clientName} at ${req.clientAddr} wants to connect.\n\n'
          'Allow only if you recognize this phone.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Deny'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Allow'),
          ),
        ],
      ),
    );
    if (allow == null) return;
    try {
      hostRespondPairing(id: req.id, allow: allow);
    } catch (e) {
      _setStatus(e.toString(), error: true);
    }
  }

  Future<void> _copyLink() async {
    final link = _snap.pairingLink;
    if (link == null || link.isEmpty) return;
    await Clipboard.setData(ClipboardData(text: link));
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('Pairing link copied')),
      );
    }
  }

  Future<void> _copyPayload() async {
    final payload = _snap.pairingPayload;
    if (payload == null || payload.isEmpty) return;
    await Clipboard.setData(ClipboardData(text: payload));
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('sbm1: payload copied')),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    if (!hostPlatformSupported()) {
      return const Center(
        child: Padding(
          padding: EdgeInsets.all(24),
          child: Text(
            'Hosting is available on Windows only in this build.',
            textAlign: TextAlign.center,
          ),
        ),
      );
    }

    final theme = Theme.of(context);
    final link = _snap.pairingLink;
    final busy = _starting && !_snap.running;

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
            const Text('Beam my screen'),
          ],
        ),
      ),
      body: busy
          ? const Center(child: CircularProgressIndicator())
          : RefreshIndicator(
              onRefresh: () async => _refresh(),
              child: ListView(
                padding: const EdgeInsets.all(16),
                children: [
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
                            _snap.running
                                ? 'Listening on ${_snap.listenAddr}'
                                : 'Starting host…',
                            style: theme.textTheme.bodySmall?.copyWith(
                              color: theme.colorScheme.onSurfaceVariant,
                            ),
                          ),
                          const SizedBox(height: 16),
                          Center(
                            child: link != null && link.isNotEmpty
                                ? Container(
                                    padding: const EdgeInsets.all(12),
                                    decoration: BoxDecoration(
                                      color: Colors.white,
                                      borderRadius: BorderRadius.circular(12),
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
                            'Scan this QR with StreamBeam on your phone, '
                            'or copy the link below.',
                            textAlign: TextAlign.center,
                            style: theme.textTheme.bodySmall?.copyWith(
                              color: theme.colorScheme.onSurfaceVariant,
                            ),
                          ),
                          const SizedBox(height: 12),
                          Row(
                            children: [
                              Expanded(
                                child: FilledButton.icon(
                                  onPressed:
                                      link == null ? null : () => _copyLink(),
                                  icon: const Icon(Icons.link),
                                  label: const Text('Copy link'),
                                ),
                              ),
                              const SizedBox(width: 8),
                              OutlinedButton.icon(
                                onPressed: _snap.pairingPayload == null
                                    ? null
                                    : () => _copyPayload(),
                                icon: const Icon(Icons.content_paste),
                                label: const Text('sbm1:'),
                              ),
                            ],
                          ),
                        ],
                      ),
                    ),
                  ),
                  const SizedBox(height: 12),
                  Card(
                    child: Column(
                      children: [
                        SwitchListTile(
                          title: const Text('Discoverable on LAN'),
                          subtitle: const Text(
                            'Phones can find this PC on the local network',
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
                  const SizedBox(height: 12),
                  Text(
                    'Connected devices',
                    style: theme.textTheme.titleSmall,
                  ),
                  const SizedBox(height: 8),
                  if (_snap.clients.isEmpty)
                    Card(
                      child: Padding(
                        padding: const EdgeInsets.all(16),
                        child: Text(
                          'No phone connected yet.',
                          style: theme.textTheme.bodyMedium?.copyWith(
                            color: theme.colorScheme.onSurfaceVariant,
                          ),
                        ),
                      ),
                    )
                  else
                    ..._snap.clients.map((c) => Card(
                          child: ListTile(
                            leading: const Icon(Icons.smartphone),
                            title: Text(c.name),
                            subtitle: Text(c.addr),
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
                                  child: const Text('Forget'),
                                ),
                              ],
                            ),
                          ),
                        )),
                  if (_status != null) ...[
                    const SizedBox(height: 12),
                    MaterialBanner(
                      content: Text(_status!),
                      backgroundColor: _statusError
                          ? theme.colorScheme.errorContainer
                          : theme.colorScheme.surfaceContainerHighest,
                      actions: [
                        TextButton(
                          onPressed: () => setState(() => _status = null),
                          child: const Text('Dismiss'),
                        ),
                      ],
                    ),
                  ],
                ],
              ),
            ),
    );
  }
}

/// Desktop shell: host + (future) watch tabs.
class AppShell extends StatefulWidget {
  const AppShell({super.key});

  @override
  State<AppShell> createState() => _AppShellState();
}

class _AppShellState extends State<AppShell> {
  int _index = 0;

  @override
  Widget build(BuildContext context) {
    final hostSupported = hostPlatformSupported();
    final pages = <Widget>[
      if (hostSupported) const BeamPage(),
      const _DesktopWatchStub(),
    ];
    final destinations = <NavigationDestination>[
      if (hostSupported)
        const NavigationDestination(
          icon: Icon(Icons.cast),
          label: 'Beam',
        ),
      const NavigationDestination(
        icon: Icon(Icons.play_circle_outline),
        label: 'Watch',
      ),
    ];

    return Scaffold(
      body: IndexedStack(
        index: _index.clamp(0, pages.length - 1),
        children: pages,
      ),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _index.clamp(0, destinations.length - 1),
        onDestinationSelected: (i) => setState(() => _index = i),
        destinations: destinations,
      ),
    );
  }
}

class _DesktopWatchStub extends StatelessWidget {
  const _DesktopWatchStub();

  @override
  Widget build(BuildContext context) {
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
            const Text('Watch'),
          ],
        ),
      ),
      body: Center(
        child: Padding(
          padding: const EdgeInsets.all(32),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(
                Icons.phone_android,
                size: 48,
                color: Theme.of(context).colorScheme.primary,
              ),
              const SizedBox(height: 16),
              Text(
                'Watch on this device',
                style: Theme.of(context).textTheme.titleMedium,
              ),
              const SizedBox(height: 8),
              Text(
                'Desktop playback is coming soon. Use your phone to watch for now.',
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.bodyMedium?.copyWith(
                      color: Theme.of(context).colorScheme.onSurfaceVariant,
                    ),
              ),
            ],
          ),
        ),
      ),
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
