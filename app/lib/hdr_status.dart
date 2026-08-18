/// What the native EDR layer reports about the HDR picture, and how to say it
/// in one line.
///
/// "Is HDR actually working?" has been unanswerable by looking: a PQ stream
/// with no EDR headroom looks dark-but-plausible, an SDR stream on a bright
/// display looks fine, and neither the decoder nor the host can tell the
/// difference — only the display layer knows. So it reports, and this turns
/// the report into something a human can act on.
///
/// Pure (no Flutter, no platform channel) so the wording is unit-testable;
/// see `test/hdr_status_test.dart`.
library;

class HdrStatus {
  const HdrStatus({
    required this.present,
    required this.frames,
    required this.headroom,
    this.setupFailure = '',
    this.drawFailures = 0,
    this.lastDrawFailure = '',
  });

  /// Whether the EDR platform view exists at all. False means HDR frames are
  /// being decoded and handed to nothing — the black-picture shape.
  final bool present;

  /// Frames the EDR layer has actually put on screen.
  final int frames;

  /// Headroom the display grants above SDR white, as a multiple. 1.0 means
  /// none, and PQ content will look very dark through no fault of the stream.
  final double headroom;

  final String setupFailure;
  final int drawFailures;
  final String lastDrawFailure;

  static HdrStatus? fromMap(Map<Object?, Object?>? map) {
    if (map == null) return null;
    return HdrStatus(
      present: map['present'] == true,
      frames: (map['frames'] as num?)?.toInt() ?? 0,
      headroom: (map['headroom'] as num?)?.toDouble() ?? 1.0,
      setupFailure: (map['setupFailure'] as String?) ?? '',
      drawFailures: (map['drawFailures'] as num?)?.toInt() ?? 0,
      lastDrawFailure: (map['lastDrawFailure'] as String?) ?? '',
    );
  }
}

/// One line describing the HDR picture, naming the culprit when there isn't
/// one. Ordered by how much it explains: a layer that failed to set up
/// explains everything after it, so it is reported first.
String hdrStatusLine(HdrStatus status) {
  if (status.setupFailure.isNotEmpty) {
    return 'HDR display unavailable — ${status.setupFailure}';
  }
  if (!status.present) {
    return 'HDR frames have nowhere to go — the EDR layer is missing';
  }
  if (status.frames == 0) {
    return status.lastDrawFailure.isEmpty
        ? 'Waiting for the first HDR frame'
        : 'No HDR frame drawn yet — ${status.lastDrawFailure}';
  }
  // Headroom is the one number that separates "HDR" from "a PQ stream being
  // crushed into SDR". Low Power Mode zeroes it on Apple displays, which is
  // the usual cause and not something any amount of debugging upstream fixes.
  if (status.headroom <= 1.0) {
    return 'HDR frames drawing, but this display grants no extra range — '
        'turn off Low Power Mode, or stream SDR';
  }
  final headroom = status.headroom.toStringAsFixed(1);
  return 'HDR active — ${status.frames} frames, ${headroom}x headroom '
      'above SDR white';
}
