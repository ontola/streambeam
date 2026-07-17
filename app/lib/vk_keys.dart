import 'package:flutter/services.dart';

/// Flutter [LogicalKeyboardKey] → Windows virtual-key code, the wire's
/// canonical key encoding (PROTOCOL.md `Key { vk, down }`). Viewers on any
/// platform map to VK before sending; hosts translate VK to their OS.
///
/// macOS notes: ⌘ maps to the Windows key, ⌥ to Alt.
int? vkForLogicalKey(LogicalKeyboardKey key) {
  final id = key.keyId;

  // Letters a–z → VK 0x41–0x5A.
  if (id >= LogicalKeyboardKey.keyA.keyId &&
      id <= LogicalKeyboardKey.keyZ.keyId) {
    return 0x41 + (id - LogicalKeyboardKey.keyA.keyId);
  }
  // Top-row digits 0–9 → VK 0x30–0x39.
  if (id >= LogicalKeyboardKey.digit1.keyId &&
      id <= LogicalKeyboardKey.digit9.keyId) {
    return 0x31 + (id - LogicalKeyboardKey.digit1.keyId);
  }
  if (id == LogicalKeyboardKey.digit0.keyId) return 0x30;
  // Numpad 0–9 → VK 0x60–0x69.
  if (id >= LogicalKeyboardKey.numpad1.keyId &&
      id <= LogicalKeyboardKey.numpad9.keyId) {
    return 0x61 + (id - LogicalKeyboardKey.numpad1.keyId);
  }
  if (id == LogicalKeyboardKey.numpad0.keyId) return 0x60;
  // F1–F12 → VK 0x70–0x7B.
  if (id >= LogicalKeyboardKey.f1.keyId && id <= LogicalKeyboardKey.f12.keyId) {
    return 0x70 + (id - LogicalKeyboardKey.f1.keyId);
  }

  return _vkTable[id];
}

/// A printable character expressed as a wire VK plus whether Shift must be
/// held. Used by touch platforms, where the software keyboard reports edited
/// text rather than physical [KeyEvent]s.
({int vk, bool shift})? vkStrokeForCharacter(String character) {
  if (character.length != 1) return null;
  final code = character.codeUnitAt(0);
  if (code >= 0x61 && code <= 0x7A) return (vk: code - 0x20, shift: false);
  if (code >= 0x41 && code <= 0x5A) return (vk: code, shift: true);
  if (code >= 0x30 && code <= 0x39) return (vk: code, shift: false);

  const plain = <String, int>{
    ' ': 0x20,
    ';': 0xBA,
    '=': 0xBB,
    ',': 0xBC,
    '-': 0xBD,
    '.': 0xBE,
    '/': 0xBF,
    '`': 0xC0,
    '[': 0xDB,
    r'\': 0xDC,
    ']': 0xDD,
    "'": 0xDE,
  };
  final plainVk = plain[character];
  if (plainVk != null) return (vk: plainVk, shift: false);

  const shifted = <String, int>{
    ')': 0x30,
    '!': 0x31,
    '@': 0x32,
    '#': 0x33,
    r'$': 0x34,
    '%': 0x35,
    '^': 0x36,
    '&': 0x37,
    '*': 0x38,
    '(': 0x39,
    ':': 0xBA,
    '+': 0xBB,
    '<': 0xBC,
    '_': 0xBD,
    '>': 0xBE,
    '?': 0xBF,
    '~': 0xC0,
    '{': 0xDB,
    '|': 0xDC,
    '}': 0xDD,
    '"': 0xDE,
  };
  final shiftedVk = shifted[character];
  return shiftedVk == null ? null : (vk: shiftedVk, shift: true);
}

final Map<int, int> _vkTable = {
  LogicalKeyboardKey.space.keyId: 0x20,
  LogicalKeyboardKey.enter.keyId: 0x0D,
  LogicalKeyboardKey.numpadEnter.keyId: 0x0D,
  LogicalKeyboardKey.tab.keyId: 0x09,
  LogicalKeyboardKey.backspace.keyId: 0x08,
  LogicalKeyboardKey.escape.keyId: 0x1B,
  LogicalKeyboardKey.capsLock.keyId: 0x14,
  LogicalKeyboardKey.shiftLeft.keyId: 0xA0,
  LogicalKeyboardKey.shiftRight.keyId: 0xA1,
  LogicalKeyboardKey.controlLeft.keyId: 0xA2,
  LogicalKeyboardKey.controlRight.keyId: 0xA3,
  LogicalKeyboardKey.altLeft.keyId: 0xA4,
  LogicalKeyboardKey.altRight.keyId: 0xA5,
  LogicalKeyboardKey.metaLeft.keyId: 0x5B, // ⌘ / Win
  LogicalKeyboardKey.metaRight.keyId: 0x5C,
  LogicalKeyboardKey.contextMenu.keyId: 0x5D,
  LogicalKeyboardKey.arrowLeft.keyId: 0x25,
  LogicalKeyboardKey.arrowUp.keyId: 0x26,
  LogicalKeyboardKey.arrowRight.keyId: 0x27,
  LogicalKeyboardKey.arrowDown.keyId: 0x28,
  LogicalKeyboardKey.insert.keyId: 0x2D,
  LogicalKeyboardKey.delete.keyId: 0x2E,
  LogicalKeyboardKey.home.keyId: 0x24,
  LogicalKeyboardKey.end.keyId: 0x23,
  LogicalKeyboardKey.pageUp.keyId: 0x21,
  LogicalKeyboardKey.pageDown.keyId: 0x22,
  LogicalKeyboardKey.printScreen.keyId: 0x2C,
  LogicalKeyboardKey.scrollLock.keyId: 0x91,
  LogicalKeyboardKey.pause.keyId: 0x13,
  LogicalKeyboardKey.numLock.keyId: 0x90,
  LogicalKeyboardKey.numpadMultiply.keyId: 0x6A,
  LogicalKeyboardKey.numpadAdd.keyId: 0x6B,
  LogicalKeyboardKey.numpadSubtract.keyId: 0x6D,
  LogicalKeyboardKey.numpadDecimal.keyId: 0x6E,
  LogicalKeyboardKey.numpadDivide.keyId: 0x6F,
  // US-layout OEM punctuation.
  LogicalKeyboardKey.semicolon.keyId: 0xBA,
  LogicalKeyboardKey.equal.keyId: 0xBB,
  LogicalKeyboardKey.comma.keyId: 0xBC,
  LogicalKeyboardKey.minus.keyId: 0xBD,
  LogicalKeyboardKey.period.keyId: 0xBE,
  LogicalKeyboardKey.slash.keyId: 0xBF,
  LogicalKeyboardKey.backquote.keyId: 0xC0,
  LogicalKeyboardKey.bracketLeft.keyId: 0xDB,
  LogicalKeyboardKey.backslash.keyId: 0xDC,
  LogicalKeyboardKey.bracketRight.keyId: 0xDD,
  LogicalKeyboardKey.quote.keyId: 0xDE,
};
