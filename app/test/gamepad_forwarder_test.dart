import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/gamepad_forwarder.dart';
import 'package:streambeam/src/rust/frb_generated.dart';

class _GamepadApi implements StreambeamFfiApi {
  final plugs = <(int, bool)>[];
  final buttons = <int>[];

  @override
  void crateApiSendGamepadPlug({required int pad, required bool connected}) {
    plugs.add((pad, connected));
  }

  @override
  void crateApiSendGamepadState({
    required int pad,
    required int buttons,
    required int lt,
    required int rt,
    required int lx,
    required int ly,
    required int rx,
    required int ry,
  }) => this.buttons.add(buttons);

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  testWidgets('opening and dismissing a menu keeps the controller attached', (
    tester,
  ) async {
    final api = _GamepadApi();
    StreambeamFfi.initMock(api: api);
    const channel = MethodChannel('streambeam/gamepad');
    final messenger = tester.binding.defaultBinaryMessenger;
    messenger.setMockMethodCallHandler(channel, (_) async => null);
    Future<void> emit(Map<String, Object> event) async {
      await messenger.handlePlatformMessage(
        channel.name,
        const StandardMethodCodec().encodeSuccessEnvelope(event),
        (_) {},
      );
      await tester.pump();
    }

    var activity = 0;
    Widget view(bool paused) => Directionality(
      textDirection: TextDirection.ltr,
      child: GamepadForwarder(
        enabled: true,
        paused: paused,
        onActivity: () => activity++,
      ),
    );
    await tester.pumpWidget(view(false));
    await emit({'kind': 'connected', 'gamepadId': '10'});
    await tester.pump(const Duration(milliseconds: 150));
    await emit({
      'kind': 'axis',
      'gamepadId': '10',
      'name': 'leftStickX',
      'value': 0.05,
    });
    expect(activity, 0); // connection, heartbeats and stick drift stay quiet
    await emit({
      'kind': 'axis',
      'gamepadId': '10',
      'name': 'leftStickX',
      'value': 0.6,
    });
    expect(activity, 1);
    await emit({'kind': 'button', 'gamepadId': '10', 'name': 'a', 'value': 1});
    expect(api.buttons.last, XInputButtons.a);
    expect(activity, 2);
    await tester.pumpWidget(view(true));
    expect(api.buttons.last, 0);
    // Releases during the menu must still update local state.
    await emit({'kind': 'button', 'gamepadId': '10', 'name': 'a', 'value': 0});
    expect(activity, 2);
    await tester.pumpWidget(view(false));
    expect(api.buttons.last, 0);
    await emit({'kind': 'button', 'gamepadId': '10', 'name': 'b', 'value': 1});
    expect(api.buttons.last, XInputButtons.b);
    expect(api.plugs, [(0, true)]);
    await tester.pumpWidget(const SizedBox());
    expect(api.plugs, [(0, true), (0, false)]);
    messenger.setMockMethodCallHandler(channel, null);
    StreambeamFfi.dispose();
  });
}
