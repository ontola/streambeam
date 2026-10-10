import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/remote_keyboard.dart';

void main() {
  final events = <(int, bool)>[];
  Future<void> open(WidgetTester tester, {VoidCallback? sas}) async {
    events.clear();
    await tester.pumpWidget(
      MaterialApp(
        home: Material(
          child: Align(
            alignment: Alignment.bottomCenter,
            child: RemoteKeyboard(
              sendKey: ({required vk, required down}) => events.add((vk, down)),
              onClose: () {},
              onSecureAttention: sas,
            ),
          ),
        ),
      ),
    );
    await tester.pump();
  }

  test('chord presses modifiers first and releases in reverse order', () {
    events.clear();
    sendRemoteChord(({required vk, required down}) => events.add((vk, down)), [
      0xA2,
      0xA0,
    ], 0x1B);
    expect(events, [
      (0xA2, true),
      (0xA0, true),
      (0x1B, true),
      (0x1B, false),
      (0xA0, false),
      (0xA2, false),
    ]);
  });

  testWidgets('Ctrl then C sends immediately without accidental IME Shift', (
    tester,
  ) async {
    await open(tester);
    await tester.tap(find.text('Ctrl'));
    await tester.pump();
    expect(events, isEmpty);
    expect(find.text('Ctrl + next key'), findsOneWidget);
    await tester.enterText(find.byType(TextField), 'C');
    await tester.pump();
    expect(events, [(0xA2, true), (0x43, true), (0x43, false), (0xA2, false)]);
    expect(find.text('Type on PC'), findsOneWidget);
    expect(
      tester.widget<TextField>(find.byType(TextField)).controller!.text,
      isEmpty,
    );
    events.clear();
    await tester.enterText(find.byType(TextField), 'x');
    expect(events, [(0x58, true), (0x58, false)]);
  });

  testWidgets('Alt+Shift then Tab and one-tap Alt+Tab keep text input ready', (
    tester,
  ) async {
    await open(tester);
    await tester.tap(find.text('Alt'));
    await tester.tap(find.text('Shift'));
    await tester.pump();
    await tester.tap(find.text('Tab'));
    await tester.pump();
    expect(events, [
      (0xA4, true),
      (0xA0, true),
      (9, true),
      (9, false),
      (0xA0, false),
      (0xA4, false),
    ]);
    events.clear();
    await tester.tap(find.text('Alt+Tab'));
    await tester.pump();
    expect(events, [(0xA4, true), (9, true), (9, false), (0xA4, false)]);
    expect(
      tester.widget<TextField>(find.byType(TextField)).focusNode!.hasFocus,
      isTrue,
    );
  });

  testWidgets('selection and dismiss send no remote modifier press', (
    tester,
  ) async {
    await open(tester);
    await tester.tap(find.text('Ctrl'));
    await tester.pumpWidget(const SizedBox());
    expect(events, isEmpty);
  });

  testWidgets('shortcut clears old text without remote backspaces', (
    tester,
  ) async {
    await open(tester);
    await tester.enterText(find.byType(TextField), 'abc');
    events.clear();
    await tester.tap(find.text('Ctrl'));
    await tester.pump();
    await tester.enterText(find.byType(TextField), 'a');
    expect(events, [(0xA2, true), (0x41, true), (0x41, false), (0xA2, false)]);
    events.clear();
    await tester.enterText(find.byType(TextField), 'Z');
    expect(events, [(0xA0, true), (0x5A, true), (0x5A, false), (0xA0, false)]);
  });

  testWidgets('ordinary typing preserves uppercase and backspace', (
    tester,
  ) async {
    await open(tester);
    await tester.enterText(find.byType(TextField), 'A');
    expect(events, [(0xA0, true), (0x41, true), (0x41, false), (0xA0, false)]);
    events.clear();
    await tester.enterText(find.byType(TextField), '');
    expect(events, [(8, true), (8, false)]);
    events.clear();
    await tester.testTextInput.receiveAction(TextInputAction.done);
    await tester.pump();
    expect(events, [(13, true), (13, false)]);
    expect(
      tester.widget<TextField>(find.byType(TextField)).focusNode!.hasFocus,
      isTrue,
    );
  });

  testWidgets('secure attention retains its dedicated RPC', (tester) async {
    var sas = 0;
    await open(tester, sas: () => sas++);
    await tester.tap(find.text('Ctrl'));
    await tester.tap(find.text('Alt'));
    await tester.pump();
    await tester.ensureVisible(find.text('Del'));
    await tester.tap(find.text('Del'));
    await tester.pump();
    expect(sas, 1);
    expect(events, isEmpty);
  });

  for (final size in [const Size(360, 640), const Size(640, 180)]) {
    testWidgets('key bar stays below 104 pixels at $size', (tester) async {
      tester.view.physicalSize = size;
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      await open(tester);
      expect(
        tester.getSize(find.byType(RemoteKeyboard)).height,
        lessThanOrEqualTo(104),
      );
      expect(tester.takeException(), isNull);
      expect(find.byType(DropdownButtonFormField<String>), findsNothing);
      expect(find.text('Send'), findsNothing);
    });
  }
}
