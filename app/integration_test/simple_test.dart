import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:streambeam/main.dart';
import 'package:streambeam/src/rust/frb_generated.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => await StreambeamFfi.init());
  testWidgets('shows StreamBeam home', (WidgetTester tester) async {
    await tester.pumpWidget(const StreamBeamApp());
    expect(find.text('StreamBeam'), findsOneWidget);
  });
}
