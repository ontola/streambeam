import 'package:flutter/material.dart';

import 'vk_keys.dart';

typedef SendRemoteKey = void Function({required int vk, required bool down});

/// Send a complete chord synchronously through the ordered input stream.
/// Modifier selection is local UI state: no remote key is held between taps.
void sendRemoteChord(SendRemoteKey send, Iterable<int> modifiers, int key) {
  final keys = <int>{...modifiers, key}.toList();
  final pressed = <int>[];
  try {
    for (final vk in keys) {
      pressed.add(vk);
      send(vk: vk, down: true);
    }
  } finally {
    for (final vk in pressed.reversed) {
      send(vk: vk, down: false);
    }
  }
}

class RemoteKeyboard extends StatefulWidget {
  const RemoteKeyboard({
    super.key,
    required this.sendKey,
    required this.onClose,
    this.onSecureAttention,
  });

  final SendRemoteKey sendKey;
  final VoidCallback onClose;
  final VoidCallback? onSecureAttention;

  @override
  State<RemoteKeyboard> createState() => _RemoteKeyboardState();
}

class _RemoteKeyboardState extends State<RemoteKeyboard> {
  static const _modifiers = {
    'Ctrl': 0xA2,
    'Alt': 0xA4,
    'Shift': 0xA0,
    'Win': 0x5B,
  };
  static const _specialKeys = {
    'Tab': 0x09,
    'Esc': 0x1B,
    '←': 0x25,
    '↑': 0x26,
    '↓': 0x28,
    '→': 0x27,
    '⌫': 0x08,
    'Del': 0x2E,
    'Enter': 0x0D,
  };
  final _selected = <int>{};
  final _textController = TextEditingController();
  final _focus = FocusNode();
  String _text = '';

  @override
  void dispose() {
    _textController.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _clearLocalText() {
    _text = '';
    _textController.clear();
  }

  void _send(int key, {Iterable<int>? modifiers, bool shift = false}) {
    final held = <int>{
      ...modifiers ?? _modifiers.values.where(_selected.contains),
      if (shift) 0xA0,
    };
    if (key == 0x2E &&
        held.contains(0xA2) &&
        held.contains(0xA4) &&
        widget.onSecureAttention != null) {
      widget.onSecureAttention!();
    } else {
      sendRemoteChord(widget.sendKey, held, key);
    }
    setState(_selected.clear);
  }

  void _tapKey(int key, {Iterable<int>? modifiers}) {
    _send(key, modifiers: modifiers);
    // Navigation/shortcuts can change the remote selection or focused app.
    // Old local text is no longer an editable mirror of that remote field.
    _clearLocalText();
    _focus.requestFocus();
  }

  void _onText(String value) {
    var prefix = 0;
    while (prefix < _text.length &&
        prefix < value.length &&
        _text.codeUnitAt(prefix) == value.codeUnitAt(prefix)) {
      prefix++;
    }
    final chord = _selected.isNotEmpty;
    for (var i = prefix; i < _text.length; i++) {
      _send(0x08);
    }
    for (final rune in value.substring(prefix).runes) {
      final stroke = vkStrokeForCharacter(String.fromCharCode(rune));
      if (stroke == null) continue;
      // IME capitalization must not turn Ctrl+C into Ctrl+Shift+C. Explicit
      // Shift remains available as a modifier; ordinary typing keeps its case.
      final shortcut = _selected.any((vk) => vk != 0xA0);
      _send(stroke.vk, shift: stroke.shift && !shortcut);
    }
    if (chord) {
      _clearLocalText();
    } else {
      _text = value;
    }
  }

  Widget _key(
    String label,
    VoidCallback onTap, {
    bool selected = false,
    String? tooltip,
  }) {
    final colors = Theme.of(context).colorScheme;
    final button = TextButton(
      style: TextButton.styleFrom(
        minimumSize: const Size(44, 44),
        padding: const EdgeInsets.symmetric(horizontal: 10),
        foregroundColor: selected ? colors.onPrimary : colors.onSurface,
        backgroundColor: selected ? colors.primary : Colors.transparent,
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(8)),
      ),
      onPressed: onTap,
      child: Text(label),
    );
    return Semantics(
      selected: selected,
      child: tooltip == null
          ? button
          : Tooltip(message: tooltip, child: button),
    );
  }

  @override
  Widget build(BuildContext context) {
    final selected = _modifiers.entries
        .where((m) => _selected.contains(m.value))
        .map((m) => m.key)
        .join('+');
    return TextFieldTapRegion(
      child: Material(
        color: Theme.of(context).colorScheme.surface,
        elevation: 8,
        borderRadius: const BorderRadius.vertical(top: Radius.circular(12)),
        child: SafeArea(
          top: false,
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 720),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                Row(
                  children: [
                    const SizedBox(width: 12),
                    Expanded(
                      child: TextField(
                        controller: _textController,
                        focusNode: _focus,
                        autofocus: true,
                        autocorrect: false,
                        enableSuggestions: false,
                        textCapitalization: TextCapitalization.none,
                        smartDashesType: SmartDashesType.disabled,
                        smartQuotesType: SmartQuotesType.disabled,
                        textInputAction: TextInputAction.done,
                        decoration: InputDecoration(
                          hintText: selected.isEmpty
                              ? 'Type on PC'
                              : '$selected + next key',
                          border: InputBorder.none,
                          isDense: true,
                        ),
                        onChanged: _onText,
                        onEditingComplete: () {},
                        onSubmitted: (_) => _tapKey(0x0D),
                      ),
                    ),
                    PopupMenuButton<(List<int>, int)>(
                      tooltip: 'Quick shortcuts',
                      requestFocus: false,
                      icon: const Icon(Icons.bolt),
                      onSelected: (value) =>
                          _tapKey(value.$2, modifiers: value.$1),
                      itemBuilder: (_) => [
                        for (final item in const [
                          ('Copy', 'Ctrl+C', [0xA2], 0x43),
                          ('Paste', 'Ctrl+V', [0xA2], 0x56),
                          ('Select all', 'Ctrl+A', [0xA2], 0x41),
                          ('Undo', 'Ctrl+Z', [0xA2], 0x5A),
                          (
                            'Task manager',
                            'Ctrl+Shift+Esc',
                            [0xA2, 0xA0],
                            0x1B,
                          ),
                          ('Show desktop', 'Win+D', [0x5B], 0x44),
                        ])
                          PopupMenuItem(
                            value: (item.$3, item.$4),
                            child: Text('${item.$1}  ·  ${item.$2}'),
                          ),
                      ],
                    ),
                    PopupMenuButton<int>(
                      tooltip: 'More keys',
                      requestFocus: false,
                      icon: const Icon(Icons.more_horiz),
                      onSelected: _tapKey,
                      itemBuilder: (_) => [
                        for (final key in const {
                          'Home': 0x24,
                          'End': 0x23,
                          'Page up': 0x21,
                          'Page down': 0x22,
                        }.entries)
                          PopupMenuItem(value: key.value, child: Text(key.key)),
                        for (var i = 1; i <= 12; i++)
                          PopupMenuItem(value: 0x6F + i, child: Text('F$i')),
                      ],
                    ),
                    IconButton(
                      tooltip: 'Close keyboard',
                      onPressed: widget.onClose,
                      icon: const Icon(Icons.keyboard_hide_outlined),
                    ),
                  ],
                ),
                SingleChildScrollView(
                  scrollDirection: Axis.horizontal,
                  padding: const EdgeInsets.fromLTRB(4, 0, 4, 4),
                  child: Row(
                    children: [
                      for (final modifier in _modifiers.entries)
                        _key(modifier.key, () {
                          _clearLocalText();
                          setState(() {
                            if (!_selected.remove(modifier.value)) {
                              _selected.add(modifier.value);
                            }
                          });
                          _focus.requestFocus();
                        }, selected: _selected.contains(modifier.value)),
                      const SizedBox(width: 6),
                      _key('Alt+Tab', () => _tapKey(0x09, modifiers: [0xA4])),
                      for (final key in _specialKeys.entries)
                        _key(
                          key.key,
                          () => _tapKey(key.value),
                          tooltip: key.key == '⌫' ? 'Backspace' : null,
                        ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
