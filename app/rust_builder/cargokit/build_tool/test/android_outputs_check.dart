import 'dart:io';

import 'package:build_tool/src/build_gradle.dart';
import 'package:build_tool/src/target.dart';
import 'package:path/path.dart' as path;

void main() {
  final output = Directory.systemTemp.createTempSync('cargokit-abis-');
  try {
    for (final target in Target.androidTargets()) {
      final file = File(path.join(output.path, target.android!, 'library.so'));
      file.createSync(recursive: true);
      file.writeAsStringSync(target.android!);
    }
    final marker = File(path.join(output.path, 'keep.txt'))
      ..writeAsStringSync('keep');
    removeUnusedAndroidTargets(
      output.path,
      [Target.forFlutterName('android-arm64')!],
    );
    final remaining = output.listSync().whereType<Directory>().toList();
    if (remaining.length != 1 ||
        path.basename(remaining.single.path) != 'arm64-v8a') {
      throw StateError('A single-ABI build must remove stale ABI libraries');
    }
    if (File(path.join(remaining.single.path, 'library.so'))
                .readAsStringSync() !=
            'arm64-v8a' ||
        !marker.existsSync()) {
      throw StateError('Selected ABI and unrelated files must remain intact');
    }
    print(
        'PASS: Android builds remove stale ABIs and preserve selected output');
  } finally {
    output.deleteSync(recursive: true);
  }
}
