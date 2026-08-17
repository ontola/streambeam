/// Detects "data is arriving but there is no picture" at runtime.
///
/// Every black-screen bug in this project's history looked identical to the
/// person watching: a black rectangle under a confident "Streaming…". The
/// wire-level counters were healthy in most of them, so only the *decoder's*
/// view distinguishes them — see `docs/DEBUGGING_CLIENT.md`.
///
/// This is the policy half, kept pure so it can be tested without a decoder, a
/// host or a network. `crates/core`'s frame gate broke three times while it was
/// tangled up with I/O; the same mistake isn't worth repeating here.
library;

import 'package:flutter/material.dart' show IconData, Icons;

/// What the player shows instead of a picture. `waiting` means "be patient",
/// everything else means "this is broken".
typedef NoPictureReason = ({
  IconData icon,
  String title,
  String detail,
  bool waiting,
});

/// Why there's no picture, most specific reason first, or null when there is
/// one.
///
/// Pure so the state machine is testable: the player widget itself can't be
/// pumped in a unit test (it reaches into `playbackFramesRendered`, which
/// needs an initialised Rust bridge), and this ordering has direct consequences
/// — the decode warning must win *even while frames arrive*, because a healthy
/// wire feeding a dead decoder is exactly the case that used to render a
/// confident black rectangle with no overlay at all.
NoPictureReason? noPictureReason({
  required String? decodeWarning,
  required bool videoStalled,
  required bool videoTimedOut,
  required bool hasVideo,
}) {
  if (decodeWarning != null) {
    return (
      icon: Icons.broken_image_outlined,
      title: 'No picture',
      detail:
          '$decodeWarning The stream is arriving, so this is a decoding '
          'problem on this device rather than a connection one.',
      waiting: false,
    );
  }
  if (videoStalled) {
    return (
      icon: Icons.sync_problem_outlined,
      title: 'Stream stopped responding',
      detail:
          'No new video frames arrived for 5 seconds. The other device may '
          'be asleep or the connection may have closed.',
      waiting: false,
    );
  }
  if (videoTimedOut) {
    return (
      icon: Icons.videocam_off_outlined,
      title: 'No video received',
      detail:
          'The connection opened, but no video arrived. Check that the other '
          'device is awake and still sharing its screen.',
      waiting: false,
    );
  }
  if (!hasVideo) {
    return (
      // Unused while `waiting`: the player draws a spinner in the icon's place
      // rather than an hourglass beside one. Kept so the record stays uniform.
      icon: Icons.hourglass_top,
      title: 'Waiting for video…',
      detail: 'Connected to your PC. The first video frame has not arrived yet.',
      waiting: true,
    );
  }
  return null;
}

/// One reading of the native decoder's counters (the `health` method on
/// `streambeam/video_desktop`).
class DecodeHealth {
  const DecodeHealth({
    required this.decodeCalls,
    required this.decodedFrames,
    required this.decodeFailures,
    required this.dropped,
    this.lastDecodeStatus = 0,
    this.lastSetupFailure = '',
  });

  /// Frames handed to the platform decoder.
  final int decodeCalls;

  /// Frames that came back out as pixels.
  final int decodedFrames;

  /// Frames the decoder rejected.
  final int decodeFailures;

  /// Frames that never reached the decoder at all.
  final int dropped;

  /// Platform status code of the last rejection (VideoToolbox `OSStatus` on
  /// macOS), 0 when there hasn't been one.
  final int lastDecodeStatus;

  /// Why decoder setup last failed; empty when it didn't.
  final String lastSetupFailure;

  /// Parse the platform channel's reply. Returns null if it isn't a map — an
  /// older or unimplemented native side, which must not break playback.
  static DecodeHealth? fromChannel(Object? reply) {
    if (reply is! Map) return null;
    int intAt(String key) {
      final v = reply[key];
      return v is int ? v : 0;
    }

    final setupFailure = reply['lastSetupFailure'];
    return DecodeHealth(
      decodeCalls: intAt('decodeCalls'),
      decodedFrames: intAt('decodedFrames'),
      decodeFailures: intAt('decodeFailures'),
      dropped: intAt('dropped'),
      lastDecodeStatus: intAt('lastDecodeStatus'),
      lastSetupFailure: setupFailure is String ? setupFailure : '',
    );
  }
}

/// What the watchdog concluded from the readings so far.
enum DecodeVerdict {
  /// Frames are decoding, or nothing has arrived yet to judge.
  healthy,

  /// Frames reached the decoder and it rejected all of them.
  decoderRejecting,

  /// Frames arrived but never reached the decoder (no session, repackaging
  /// failure) — the shape of the `session = sess` regression.
  droppedBeforeDecode,

  /// Frames were accepted and acknowledged but no pixels ever came back.
  noFramesOut,
}

/// Watches successive [DecodeHealth] samples and reports the first thing that
/// looks like a black screen.
///
/// Deliberately conservative: it only ever fires when the decoder has been
/// *given* frames, so a slow host, a stalled network or a paused stream can't
/// trigger it — those are not this bug, and a false alarm here trains people to
/// ignore the one true alarm.
class DecodeWatchdog {
  /// [grace] is how long frames may arrive with nothing decoded before that
  /// counts as broken. One second is too short: a 4K keyframe plus decoder
  /// warm-up legitimately takes a few hundred milliseconds, and reconfiguration
  /// resets the counters.
  DecodeWatchdog({this.grace = const Duration(seconds: 3)});

  final Duration grace;

  /// When frames first arrived at the decoder without any coming out. Null
  /// while healthy, so recovery clears it and the countdown restarts.
  Duration? _startedWaiting;
  DecodeVerdict _verdict = DecodeVerdict.healthy;
  DecodeHealth? _last;

  DecodeVerdict get verdict => _verdict;

  /// The most recent sample, for the diagnostic message.
  DecodeHealth? get last => _last;

  /// Feed one sample. [elapsed] is any monotonic clock (e.g. a `Stopwatch`),
  /// passed in rather than read here so tests don't have to wait in real time.
  DecodeVerdict sample(DecodeHealth health, Duration elapsed) {
    _last = health;

    // Any decoded frame means the whole path works; a later freeze is a
    // different problem (and would need its own rule) — don't claim this one.
    if (health.decodedFrames > 0) {
      _startedWaiting = null;
      _verdict = DecodeVerdict.healthy;
      return _verdict;
    }

    // Nothing has been offered to the decoder yet: could be connecting, could
    // be a stalled host. Not our call to make. `dropped` is a subset of
    // `decodeCalls` on macOS (counted inside `decode`) but need not be on every
    // platform, hence the max rather than a sum.
    final offered =
        health.decodeCalls > health.dropped ? health.decodeCalls : health.dropped;
    if (offered == 0) {
      _startedWaiting = null;
      _verdict = DecodeVerdict.healthy;
      return _verdict;
    }

    _startedWaiting ??= elapsed;
    if (elapsed - _startedWaiting! < grace) {
      _verdict = DecodeVerdict.healthy;
      return _verdict;
    }

    // Order matters: prefer the most specific explanation available. Every
    // offered frame having been dropped is the strongest signal, because it
    // means the decoder was never even asked.
    if (health.dropped > 0 && health.dropped >= health.decodeCalls) {
      _verdict = DecodeVerdict.droppedBeforeDecode;
    } else if (health.decodeFailures > 0) {
      _verdict = DecodeVerdict.decoderRejecting;
    } else {
      _verdict = DecodeVerdict.noFramesOut;
    }
    return _verdict;
  }

  /// Forget everything — call on reconfigure, since the native counters reset
  /// with the decoder session.
  void reset() {
    _startedWaiting = null;
    _verdict = DecodeVerdict.healthy;
    _last = null;
  }

  /// A short user-facing line, or null while healthy. Deliberately names the
  /// failing stage: "no picture" alone is what made this bug class so expensive.
  String? get message {
    final health = _last;
    if (health == null) return null;
    switch (_verdict) {
      case DecodeVerdict.healthy:
        return null;
      case DecodeVerdict.decoderRejecting:
        final status = health.lastDecodeStatus;
        return 'No picture: the decoder rejected all '
            '${health.decodeFailures} frames'
            '${status != 0 ? ' (status $status)' : ''}.';
      case DecodeVerdict.droppedBeforeDecode:
        return 'No picture: ${health.dropped} frames never reached the '
            'decoder'
            '${health.lastSetupFailure.isNotEmpty ? ' — ${health.lastSetupFailure}' : ''}.';
      case DecodeVerdict.noFramesOut:
        return 'No picture: ${health.decodeCalls} frames went into the decoder '
            'and none came out.';
    }
  }
}
