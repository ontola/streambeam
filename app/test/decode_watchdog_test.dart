import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/decode_watchdog.dart';

DecodeHealth health({
  int calls = 0,
  int decoded = 0,
  int failures = 0,
  int dropped = 0,
  int status = 0,
  String setupFailure = '',
}) => DecodeHealth(
  decodeCalls: calls,
  decodedFrames: decoded,
  decodeFailures: failures,
  dropped: dropped,
  lastDecodeStatus: status,
  lastSetupFailure: setupFailure,
);

void main() {
  const grace = Duration(seconds: 3);
  final past = grace + const Duration(seconds: 1);

  group('stays quiet when it should', () {
    test('a working stream never warns', () {
      final dog = DecodeWatchdog();
      for (var i = 1; i < 100; i++) {
        final v = dog.sample(
          health(calls: i, decoded: i),
          Duration(milliseconds: i * 100),
        );
        expect(v, DecodeVerdict.healthy);
      }
      expect(dog.message, isNull);
    });

    // A stalled host, a paused stream or a dead network is not this bug, and a
    // watchdog that cries wolf is a watchdog people learn to ignore.
    test('no frames offered is never blamed on the decoder', () {
      final dog = DecodeWatchdog();
      expect(dog.sample(health(), past), DecodeVerdict.healthy);
      expect(dog.message, isNull);
    });

    test('slow first frame inside the grace period is tolerated', () {
      final dog = DecodeWatchdog();
      expect(
        dog.sample(health(calls: 4), const Duration(milliseconds: 500)),
        DecodeVerdict.healthy,
      );
      expect(
        dog.sample(health(calls: 12), const Duration(milliseconds: 2500)),
        DecodeVerdict.healthy,
      );
      // ...and once a frame lands, it stays quiet forever after.
      expect(
        dog.sample(health(calls: 20, decoded: 1), past),
        DecodeVerdict.healthy,
      );
    });

    test('recovery clears the countdown, so a later stall gets a full grace', () {
      final dog = DecodeWatchdog();
      dog.sample(health(calls: 5), const Duration(seconds: 1));
      dog.sample(health(calls: 9, decoded: 4), const Duration(seconds: 2));
      // Counters reset by a reconfigure; the clock has moved well past `grace`,
      // but the wait only starts now.
      expect(
        dog.sample(health(calls: 3), const Duration(seconds: 30)),
        DecodeVerdict.healthy,
      );
      expect(
        dog.sample(health(calls: 40), const Duration(seconds: 34)),
        DecodeVerdict.noFramesOut,
      );
    });
  });

  group('catches the historical black screens', () {
    // 2026-08-17: `makeSession` lost `session = sess`, so every frame hit
    // `guard let session` and was discarded — 720 dropped / 720 calls.
    test('frames dropped before the decoder', () {
      final dog = DecodeWatchdog();
      dog.sample(health(calls: 120, dropped: 120), const Duration(seconds: 1));
      expect(
        dog.sample(health(calls: 720, dropped: 720), past),
        DecodeVerdict.droppedBeforeDecode,
      );
      expect(dog.message, contains('never reached the'));
      expect(dog.message, contains('720'));
    });

    // 2026-08-17: the MediaCodec-shaped AV1 made VideoToolbox reject every
    // frame with -12909.
    test('decoder rejecting every frame reports its status', () {
      final dog = DecodeWatchdog();
      dog.sample(health(calls: 30, failures: 30), const Duration(seconds: 1));
      expect(
        dog.sample(
          health(calls: 400, failures: 400, status: -12909),
          past,
        ),
        DecodeVerdict.decoderRejecting,
      );
      expect(dog.message, contains('-12909'));
    });

    test('setup failure is surfaced verbatim', () {
      final dog = DecodeWatchdog();
      const reason = 'VTDecompressionSessionCreate status -12910';
      dog.sample(
        health(calls: 10, dropped: 10, setupFailure: reason),
        const Duration(seconds: 1),
      );
      dog.sample(
        health(calls: 90, dropped: 90, setupFailure: reason),
        past,
      );
      expect(dog.message, contains(reason));
    });

    // Frames accepted and acknowledged, nothing ever coming back: the residual
    // case, and the one with no other symptom at all.
    test('silent decoder with no failures still warns', () {
      final dog = DecodeWatchdog();
      dog.sample(health(calls: 50), const Duration(seconds: 1));
      expect(dog.sample(health(calls: 300), past), DecodeVerdict.noFramesOut);
      expect(dog.message, contains('none came out'));
    });
  });

  group('no-picture reason', () {
    test('a healthy stream shows no overlay at all', () {
      expect(
        noPictureReason(
          decodeWarning: null,
          videoStalled: false,
          videoTimedOut: false,
          hasVideo: true,
        ),
        isNull,
      );
    });

    test('waiting for the first frame is the only patient state', () {
      final r = noPictureReason(
        decodeWarning: null,
        videoStalled: false,
        videoTimedOut: false,
        hasVideo: false,
      );
      expect(r!.title, 'Waiting for video…');
      expect(r.waiting, isTrue);
    });

    // The decode warning must win even though frames are arriving (hasVideo
    // true, nothing stalled) — that combination is the black screen this whole
    // watchdog exists for, and it previously showed no overlay whatsoever.
    test('a dead decoder wins over an otherwise healthy stream', () {
      final r = noPictureReason(
        decodeWarning: 'No picture: the decoder rejected all 400 frames.',
        videoStalled: false,
        videoTimedOut: false,
        hasVideo: true,
      );
      expect(r, isNotNull);
      expect(r!.detail, contains('rejected all 400 frames'));
      expect(r.detail, contains('decoding problem'));
      expect(r.waiting, isFalse);
    });

    test('a decode warning outranks a stall', () {
      final r = noPictureReason(
        decodeWarning: 'No picture: 90 frames never reached the decoder.',
        videoStalled: true,
        videoTimedOut: true,
        hasVideo: true,
      );
      expect(r!.title, 'No picture');
    });

    test('stall outranks the connect timeout', () {
      final r = noPictureReason(
        decodeWarning: null,
        videoStalled: true,
        videoTimedOut: true,
        hasVideo: true,
      );
      expect(r!.title, 'Stream stopped responding');
      expect(r.waiting, isFalse);
    });

    test('no video after the connect timeout', () {
      final r = noPictureReason(
        decodeWarning: null,
        videoStalled: false,
        videoTimedOut: true,
        hasVideo: false,
      );
      expect(r!.title, 'No video received');
      expect(r.waiting, isFalse);
    });

    // The player renders a stop/cancel button unconditionally in this overlay,
    // so every reason it can return is escapable. A spinner with no way out
    // leaves an impatient user force-quitting the app.
    test('every no-picture state carries a title and detail to explain itself', () {
      final states = <NoPictureReason?>[
        noPictureReason(
          decodeWarning: 'x',
          videoStalled: false,
          videoTimedOut: false,
          hasVideo: true,
        ),
        noPictureReason(
          decodeWarning: null,
          videoStalled: true,
          videoTimedOut: false,
          hasVideo: true,
        ),
        noPictureReason(
          decodeWarning: null,
          videoStalled: false,
          videoTimedOut: true,
          hasVideo: false,
        ),
        noPictureReason(
          decodeWarning: null,
          videoStalled: false,
          videoTimedOut: false,
          hasVideo: false,
        ),
      ];
      for (final s in states) {
        expect(s, isNotNull);
        expect(s!.title, isNotEmpty);
        expect(s.detail, isNotEmpty);
      }
      // Exactly one of them is a "be patient" state.
      expect(states.where((s) => s!.waiting).length, 1);
    });
  });

  group('channel parsing', () {
    test('reads the native reply', () {
      final parsed = DecodeHealth.fromChannel({
        'decodeCalls': 7,
        'decodedFrames': 3,
        'decodeFailures': 1,
        'dropped': 2,
        'lastDecodeStatus': -12909,
        'lastSetupFailure': 'nope',
      });
      expect(parsed, isNotNull);
      expect(parsed!.decodeCalls, 7);
      expect(parsed.decodedFrames, 3);
      expect(parsed.lastDecodeStatus, -12909);
      expect(parsed.lastSetupFailure, 'nope');
    });

    // Windows and Android don't implement `health` yet; playback must not care.
    test('a non-map reply yields null rather than throwing', () {
      expect(DecodeHealth.fromChannel(null), isNull);
      expect(DecodeHealth.fromChannel(42), isNull);
    });

    test('missing or wrongly-typed keys default instead of throwing', () {
      final parsed = DecodeHealth.fromChannel({'decodeCalls': 'lots'});
      expect(parsed, isNotNull);
      expect(parsed!.decodeCalls, 0);
      expect(parsed.lastSetupFailure, '');
    });
  });
}
