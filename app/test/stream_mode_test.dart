import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/stream_player.dart';

void main() {
  // The viewer's self-reported size is not trustworthy: this Mac double-counts
  // devicePixelRatio on a 5K display and reports 10240x5760. Every request is
  // normalized to the 3840 ceiling first, so the numbers below hold whatever
  // the platform claims.
  const bogus5k = (10240, 5760);
  const phone = (1080, 2400);

  test('quality normalizes an over-reported viewer size down to 4K', () {
    final (w, h, fps) = StreamMode.quality.apply(bogus5k.$1, bogus5k.$2, 120);
    expect(w, 3840);
    expect(h, 2160);
    expect(fps, 30);
  });

  test('quality raises a smaller viewer up to the 4K ceiling', () {
    final (w, h, _) = StreamMode.quality.apply(1920, 1080, 60);
    expect(w, 3840);
    expect(h, 2160);
  });

  test('game keeps full refresh rate at the 4K ceiling', () {
    final (w, h, fps) = StreamMode.game.apply(bogus5k.$1, bogus5k.$2, 120);
    expect(w, 3840);
    expect(h, 2160);
    expect(fps, 120);
  });

  test('Android game starts at a lighter, orientation-independent 60 fps', () {
    for (final size in [(1200, 2670), (2670, 1200), (1600, 2560)]) {
      final (w, h, fps) = StreamMode.game.apply(
        size.$1,
        size.$2,
        120,
        isAndroid: true,
      );
      expect(w > h ? w : h, 1920);
      expect(fps, 60);
    }
    expect(StreamMode.game.apply(1280, 720, 30, isAndroid: true), (
      1280,
      720,
      30,
    ));
    expect(
      StreamMode.custom.apply(
        1920,
        1080,
        120,
        isAndroid: true,
        customSettings: const CustomStreamSettings(maxFps: 120),
      ),
      (3840, 2160, 120),
    );
  });

  test('custom slider changes the request instead of pinning to the clamp', () {
    // The regression: with a 10240-wide base, every scale above ~37% exceeded
    // the host's own 3840 clamp and produced an identical stream.
    final full = StreamMode.custom.apply(
      bogus5k.$1,
      bogus5k.$2,
      120,
      customSettings: const CustomStreamSettings(resolutionScale: 1.0),
    );
    final half = StreamMode.custom.apply(
      bogus5k.$1,
      bogus5k.$2,
      120,
      customSettings: const CustomStreamSettings(resolutionScale: 0.5),
    );
    final threeQuarter = StreamMode.custom.apply(
      bogus5k.$1,
      bogus5k.$2,
      120,
      customSettings: const CustomStreamSettings(resolutionScale: 0.75),
    );

    expect(full.$1, 3840);
    expect(half.$1, 1920);
    expect(threeQuarter.$1, 2880);
    // Each step must be distinct — that is the whole point of the slider.
    expect({full.$1, half.$1, threeQuarter.$1}.length, 3);
  });

  test('data saver still only tightens, from any reported size', () {
    final (bogusW, _, bogusFps) = StreamMode.dataSaver.apply(
      bogus5k.$1,
      bogus5k.$2,
      120,
    );
    expect(bogusW, 1280);
    expect(bogusFps, 24);

    // A phone below the cap keeps its own (portrait) size untouched.
    final (phoneW, phoneH, _) = StreamMode.dataSaver.apply(
      phone.$1,
      phone.$2,
      60,
    );
    expect(phoneH, 1280);
    expect(phoneW, 576);
  });

  test('aspect ratio survives normalization', () {
    final (w, h, _) = StreamMode.quality.apply(2560, 1080, 60); // ultrawide
    expect(w, 3840);
    expect(h, 1620);
    expect(w / h, closeTo(2560 / 1080, 0.001));
  });
}
