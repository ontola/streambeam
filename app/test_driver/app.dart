/// Dev-only entrypoint that exposes the Flutter Driver extension, so the app
/// can be driven programmatically (tap a device tile, open the sidebar, change
/// quality) instead of needing a human at the keyboard.
///
/// This exists because debugging the streaming path is a loop of "connect, look
/// at the picture, read the logs" — and on macOS the client's own Rust logs are
/// only visible via stderr, so a human clicking was the bottleneck in every
/// diagnosis (see PROGRESS, 2026-08-17).
///
/// Run it with:
///   cd app && flutter run -d macos -t test_driver/app.dart --print-dtd
///
/// then connect a Dart Tooling Daemon client to the printed URI. The normal
/// entrypoint (`lib/main.dart`) is untouched, so release builds never link
/// `flutter_driver`.
library;

import 'package:flutter_driver/driver_extension.dart';
import 'package:streambeam/main.dart' as app;

Future<void> main() async {
  enableFlutterDriverExtension();
  await app.main();
}
