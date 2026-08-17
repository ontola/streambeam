import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// Debug-only screen answering one question: can this app put HDR on screen?
///
/// See `docs/HDR.md`. Passthrough is worthless if the picture can only be
/// composited as SDR, and Flutter renders through an engine-owned SDR layer —
/// so the plan assumes a native platform view owning an EDR `CAMetalLayer`.
/// This proves that assumption before any encoder or protocol work leans on it.
///
/// The native side draws a ramp from 0 to 8 in EDR units with a notch at 1.0
/// (SDR white). Two independent readings:
///
///  - **By eye**: everything right of the notch should be visibly brighter than
///    any white the surrounding Flutter UI can produce. If it flattens into a
///    solid block at the notch, EDR is not reaching the screen.
///  - **By number**: `edrHeadroomLive` is the headroom macOS is *granting* while
///    the ramp is displayed. macOS only raises it above 1.0 when EDR content is
///    actually on screen, so > 1.0 is objective proof, independent of eyeballs
///    and immune to screenshots (which flatten to SDR).
class HdrProbeScreen extends StatefulWidget {
  const HdrProbeScreen({super.key});

  @override
  State<HdrProbeScreen> createState() => _HdrProbeScreenState();
}

class _HdrProbeScreenState extends State<HdrProbeScreen> {
  static const _channel = MethodChannel('streambeam/hdr_probe');
  Map<Object?, Object?>? _report;
  String? _error;

  Future<void> _refresh() async {
    try {
      final report = await _channel.invokeMethod<dynamic>('report');
      if (!mounted) return;
      setState(() {
        _report = report is Map ? report : null;
        _error = report == null ? 'no probe view has been created yet' : null;
      });
    } catch (e) {
      if (!mounted) return;
      setState(() => _error = e.toString());
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final headroom = _report?['edrHeadroomLive'];
    final engaged = headroom is num && headroom > 1.0;

    return Scaffold(
      appBar: AppBar(title: const Text('HDR (EDR) probe')),
      body: Column(
        children: [
          const Expanded(
            child: AppKitView(
              viewType: 'streambeam/hdr_probe',
              layoutDirection: TextDirection.ltr,
            ),
          ),
          Padding(
            padding: const EdgeInsets.all(16),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  'Four patches: 1×, 2×, 4×, 8× SDR white, over a fixed SDR '
                  'white strip along the bottom. If EDR is live they step '
                  'visibly brighter left to right and the upper patches glow '
                  'against the strip. If all four look like the same white, '
                  'the values are being clipped and EDR is not reaching the '
                  'screen.',
                  style: theme.textTheme.bodySmall,
                ),
                const SizedBox(height: 8),
                Row(
                  children: [
                    FilledButton(
                      onPressed: _refresh,
                      child: const Text('Read EDR headroom'),
                    ),
                    const SizedBox(width: 12),
                    if (_report != null)
                      Text(
                        engaged
                            ? 'EDR engaged — headroom $headroom×'
                            : 'EDR not engaged (headroom ${headroom ?? "?"})',
                        style: theme.textTheme.bodyMedium?.copyWith(
                          color: engaged ? Colors.green : theme.colorScheme.error,
                          fontWeight: FontWeight.bold,
                        ),
                      ),
                    if (_error != null)
                      Expanded(
                        child: Text(
                          _error!,
                          style: theme.textTheme.bodySmall
                              ?.copyWith(color: theme.colorScheme.error),
                        ),
                      ),
                  ],
                ),
                if (_report != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 8),
                    child: SelectableText(
                      _report.toString(),
                      style: theme.textTheme.bodySmall,
                    ),
                  ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
