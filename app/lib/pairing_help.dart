import 'package:flutter/material.dart';

enum PairingAction { requestAccess, scan, paste }

/// First contact and pairing recovery share one actionable entry point.
class PairingHelp extends StatelessWidget {
  const PairingHelp({
    super.key,
    required this.hostName,
    required this.canScan,
    this.needsCode = false,
  });

  final String hostName;
  final bool canScan;
  final bool needsCode;

  @override
  Widget build(BuildContext context) => AlertDialog(
    scrollable: true,
    title: Text(needsCode ? 'Pair with $hostName' : 'Connect to $hostName'),
    content: SizedBox(
      width: 380,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            needsCode
                ? '$hostName is reachable, but this device needs to be paired. '
                      'Open StreamBeam on $hostName and choose Show QR. '
                      'Scan it here, or copy its pairing link to this device.'
                : 'Send a connection request to $hostName. On that device, '
                      'open StreamBeam and tap Allow. If it runs only as a '
                      'background service, use its pairing QR or link instead.',
          ),
          const SizedBox(height: 20),
          if (!needsCode) ...[
            FilledButton.icon(
              onPressed: () =>
                  Navigator.pop(context, PairingAction.requestAccess),
              icon: const Icon(Icons.devices),
              label: const Text('Request connection'),
            ),
            const SizedBox(height: 16),
            const Text(
              'Or pair using the QR code or link shown on that device.',
            ),
            const SizedBox(height: 12),
          ],
          if (canScan)
            FilledButton.tonalIcon(
              onPressed: () => Navigator.pop(context, PairingAction.scan),
              icon: const Icon(Icons.qr_code_scanner),
              label: const Text('Scan pairing QR'),
            ),
          TextButton.icon(
            onPressed: () => Navigator.pop(context, PairingAction.paste),
            icon: const Icon(Icons.content_paste),
            label: const Text('Paste pairing link'),
          ),
        ],
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
    ],
  );
}
