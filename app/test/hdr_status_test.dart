import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/hdr_status.dart';

HdrStatus status({
  bool present = true,
  int frames = 100,
  double headroom = 3.7,
  String setupFailure = '',
  int drawFailures = 0,
  String lastDrawFailure = '',
}) => HdrStatus(
  present: present,
  frames: frames,
  headroom: headroom,
  setupFailure: setupFailure,
  drawFailures: drawFailures,
  lastDrawFailure: lastDrawFailure,
);

void main() {
  group('names the reason HDR looks wrong', () {
    test('a working HDR picture reports its headroom', () {
      final line = hdrStatusLine(status());
      expect(line, contains('HDR active'));
      expect(line, contains('3.7x'));
    });

    test('no headroom is called out as the display, not the stream', () {
      // The whole point: PQ with headroom 1.0 looks dark but not obviously
      // broken, so without this the user debugs the host for an hour.
      final line = hdrStatusLine(status(headroom: 1.0));
      expect(line, contains('no extra range'));
      expect(line, contains('Low Power Mode'));
    });

    test('a missing EDR layer is distinguished from a dark one', () {
      expect(
        hdrStatusLine(status(present: false)),
        contains('nowhere to go'),
      );
    });

    test('a setup failure outranks everything after it', () {
      final line = hdrStatusLine(
        status(present: false, frames: 0, setupFailure: 'no Metal device'),
      );
      expect(line, contains('no Metal device'));
    });

    test('frames that never drew carry the draw failure', () {
      final line = hdrStatusLine(
        status(frames: 0, drawFailures: 12, lastDrawFailure: 'nextDrawable returned nil'),
      );
      expect(line, contains('nextDrawable returned nil'));
    });

    test('before the first frame it says so rather than blaming anything', () {
      expect(hdrStatusLine(status(frames: 0)), contains('Waiting'));
    });
  });

  group('reads the native report', () {
    test('missing fields fall back to a safe, honest default', () {
      final s = HdrStatus.fromMap({'present': true});
      expect(s!.frames, 0);
      expect(s.headroom, 1.0);
    });

    test('a null report is a null status, not a crash', () {
      expect(HdrStatus.fromMap(null), isNull);
    });
  });
}
