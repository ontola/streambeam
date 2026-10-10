import 'package:build_tool/src/rustup.dart';
import 'package:build_tool/src/util.dart';

void check(bool condition, String message) {
  if (!condition) throw StateError(message);
}

// Standalone regression check: the vendored build tool pins older dependency
// versions, whose test runner predates the current Dart SDK frontend snapshot.
void main() {
  try {
    testRunCommandOverride = (args) {
      check(args.executable == 'rustup', 'Expected rustup');
      check(args.arguments.join(' ') == 'show active-toolchain',
          'Expected active toolchain lookup');
      check(args.workingDirectory == '/project/app/rust',
          'Lookup must use the crate directory');
      return TestRunCommandResult(
        stdout:
            '1.95.0-x86_64-unknown-linux-gnu (overridden by rust-toolchain.toml)\n',
      );
    };
    check(
        Rustup.activeToolchain('/project/app/rust') ==
            '1.95.0-x86_64-unknown-linux-gnu',
        'Must respect the project pin');

    testRunCommandOverride = (args) {
      if (args.arguments.join(' ') == 'toolchain list') {
        return TestRunCommandResult(
          stdout:
              '1.95.0-x86_64-unknown-linux-gnu (active)\ncustom-toolchain\n',
        );
      }
      check(
          args.arguments.join(' ') ==
              'target list --toolchain 1.95.0-x86_64-unknown-linux-gnu --installed',
          'Must recognize numbered toolchains and ignore custom ones');
      return TestRunCommandResult(stdout: 'aarch64-linux-android\n');
    };
    check(
        Rustup().installedTargets('1.95.0')?.single == 'aarch64-linux-android',
        'Installed Android target must be reused');
    print(
        'PASS: project toolchain pin and installed numbered toolchain lookup');
  } finally {
    testRunCommandOverride = null;
  }
}
