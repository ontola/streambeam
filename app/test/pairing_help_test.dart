import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/pairing_help.dart';

void main() {
  testWidgets('rejected pairing offers working recovery actions, not retry', (
    tester,
  ) async {
    PairingAction? chosen;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) {
            return TextButton(
              onPressed: () async {
                chosen = await showDialog<PairingAction>(
                  context: context,
                  builder: (_) => const PairingHelp(
                    hostName: 'MANCAVE',
                    canScan: true,
                    needsCode: true,
                  ),
                );
              },
              child: const Text('Open'),
            );
          },
        ),
      ),
    );
    await tester.tap(find.text('Open'));
    await tester.pumpAndSettle();
    expect(find.text('Pair with MANCAVE'), findsOneWidget);
    expect(find.text('Connect'), findsNothing);
    await tester.tap(find.text('Scan pairing QR'));
    await tester.pumpAndSettle();
    expect(chosen, PairingAction.scan);
  });

  testWidgets(
    'desktop first contact explains approval without offering a camera',
    (tester) async {
      await tester.pumpWidget(
        const MaterialApp(
          home: Scaffold(
            body: PairingHelp(hostName: 'MANCAVE', canScan: false),
          ),
        ),
      );
      expect(find.textContaining('tap Allow'), findsOneWidget);
      expect(find.text('Scan pairing QR'), findsNothing);
      expect(find.text('Paste pairing link'), findsOneWidget);
      expect(find.text('Request connection'), findsOneWidget);
      expect(tester.takeException(), isNull);
    },
  );
}
