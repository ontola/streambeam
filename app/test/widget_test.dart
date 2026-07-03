import 'package:flutter_test/flutter_test.dart';

import 'package:streambeam/main.dart';

void main() {
  testWidgets('app loads', (tester) async {
    await tester.pumpWidget(const StreamBeamApp());
    expect(find.text('StreamBeam'), findsOneWidget);
  });
}
