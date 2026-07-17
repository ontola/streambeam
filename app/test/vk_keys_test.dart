import 'package:flutter_test/flutter_test.dart';
import 'package:streambeam/vk_keys.dart';

void main() {
  test('maps software-keyboard characters to VK strokes', () {
    expect(vkStrokeForCharacter('a'), (vk: 0x41, shift: false));
    expect(vkStrokeForCharacter('A'), (vk: 0x41, shift: true));
    expect(vkStrokeForCharacter('7'), (vk: 0x37, shift: false));
    expect(vkStrokeForCharacter('?'), (vk: 0xBF, shift: true));
    expect(vkStrokeForCharacter('é'), isNull);
  });
}
