import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/remote_keyboard.dart';

void main() {
  test('chords press modifiers first and release in reverse order', () {
    final events = <(int, bool)>[];
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

  testWidgets('custom modifiers stay local until Send and reset afterward', (
    tester,
  ) async {
    final events = <(int, bool)>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: RemoteKeyboard(
            sendKey: ({required vk, required down}) => events.add((vk, down)),
            onClose: () {},
          ),
        ),
      ),
    );
    await tester.tap(find.text('Ctrl'));
    await tester.tap(find.text('Alt'));
    await tester.pump();
    expect(events, isEmpty);
    expect(
      find.text('Ctrl+Alt+Tab · modifiers release after sending'),
      findsOneWidget,
    );
    await tester.tap(find.text('Send'));
    await tester.pump();
    expect(events, [
      (0xA2, true),
      (0xA4, true),
      (9, true),
      (9, false),
      (0xA4, false),
      (0xA2, false),
    ]);
    expect(
      tester
          .widgetList<FilterChip>(find.byType(FilterChip))
          .every((chip) => !chip.selected),
      isTrue,
    );
    events.clear();
    await tester.tap(find.text('Alt+Tab'));
    await tester.pump();
    expect(events, [(0xA4, true), (9, true), (9, false), (0xA4, false)]);
    events.clear();
    await tester.tap(find.text('Ctrl'));
    await tester.pumpWidget(const SizedBox());
    expect(events, isEmpty);
  });

  testWidgets('secure attention uses its RPC', (tester) async {
    var sas = 0;
    final events = <(int, bool)>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: RemoteKeyboard(
            sendKey: ({required vk, required down}) => events.add((vk, down)),
            onClose: () {},
            onSecureAttention: () => sas++,
          ),
        ),
      ),
    );
    await tester.tap(find.text('Ctrl'));
    await tester.tap(find.text('Alt'));
    await tester.tap(find.byType(DropdownButtonFormField<String>));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Delete').last);
    await tester.pumpAndSettle();
    await tester.tap(find.text('Send'));
    await tester.pump();
    expect(sas, 1);
    expect(events, isEmpty);
  });

  testWidgets('text editing sends shifted characters and backspace', (
    tester,
  ) async {
    final events = <(int, bool)>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: RemoteKeyboard(
            sendKey: ({required vk, required down}) => events.add((vk, down)),
            onClose: () {},
          ),
        ),
      ),
    );
    await tester.tap(find.text('Type text'));
    await tester.pump();
    await tester.enterText(find.byType(TextField), 'A');
    expect(events, [(0xA0, true), (0x41, true), (0x41, false), (0xA0, false)]);
    events.clear();
    await tester.enterText(find.byType(TextField), '');
    expect(events, [(8, true), (8, false)]);
  });

  for (final size in [const Size(360, 640), const Size(640, 280)]) {
    testWidgets('controls fit $size', (tester) async {
      tester.view.physicalSize = size;
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.resetPhysicalSize);
      addTearDown(tester.view.resetDevicePixelRatio);
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: Align(
              alignment: Alignment.bottomCenter,
              child: RemoteKeyboard(
                sendKey: ({required vk, required down}) {},
                onClose: () {},
              ),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();

      await tester.ensureVisible(find.text('Send'));
      await tester.tap(find.text('Send'));
      await tester.pump();
      expect(find.text('Sent Tab'), findsOneWidget);
    });
  }
}
