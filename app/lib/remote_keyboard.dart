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
  static final _keys = <String, int>{
    'Tab': 0x09,
    'Esc': 0x1B,
    'Enter': 0x0D,
    'Space': 0x20,
    'Backspace': 0x08,
    'Delete': 0x2E,
    '←': 0x25,
    '↑': 0x26,
    '→': 0x27,
    '↓': 0x28,
    'Home': 0x24,
    'End': 0x23,
    'Page up': 0x21,
    'Page down': 0x22,
    for (var i = 0; i < 26; i++) String.fromCharCode(0x41 + i): 0x41 + i,
    for (var i = 0; i < 10; i++) '$i': 0x30 + i,
    for (var i = 1; i <= 12; i++) 'F$i': 0x6F + i,
  };
  final _selected = <int>{};
  final _textController = TextEditingController();
  final _focus = FocusNode();
  String _text = '';
  String _key = 'Tab';
  String? _sent;
  bool _typing = false;

  @override
  void dispose() {
    _textController.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _send(String label, Iterable<int> modifiers, int key) {
    if (key == 0x2E &&
        modifiers.contains(0xA2) &&
        modifiers.contains(0xA4) &&
        widget.onSecureAttention != null) {
      widget.onSecureAttention!();
    } else {
      sendRemoteChord(widget.sendKey, modifiers, key);
    }
    setState(() {
      _sent = 'Sent $label';
      _selected.clear();
    });
  }

  void _onText(String value) {
    var prefix = 0;
    while (prefix < _text.length &&
        prefix < value.length &&
        _text.codeUnitAt(prefix) == value.codeUnitAt(prefix)) {
      prefix++;
    }
    for (var i = prefix; i < _text.length; i++) {
      sendRemoteChord(widget.sendKey, const [], 0x08);
    }
    for (final rune in value.substring(prefix).runes) {
      final stroke = vkStrokeForCharacter(String.fromCharCode(rune));
      if (stroke != null) {
        sendRemoteChord(widget.sendKey, [if (stroke.shift) 0xA0], stroke.vk);
      }
    }
    _text = value;
  }

  @override
  Widget build(BuildContext context) {
    final chord = [
      for (final entry in _modifiers.entries)
        if (_selected.contains(entry.value)) entry.key,
      _key,
    ].join('+');
    return Material(
      color: Theme.of(context).colorScheme.surface,
      elevation: 8,
      borderRadius: const BorderRadius.vertical(top: Radius.circular(20)),
      child: SafeArea(
        top: false,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 720),
          child: SingleChildScrollView(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 12),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Expanded(
                      child: TextButton(
                        onPressed: () {
                          _focus.unfocus();
                          setState(() => _typing = false);
                        },
                        child: Text(
                          'Shortcuts',
                          style: TextStyle(
                            fontWeight: !_typing
                                ? FontWeight.bold
                                : FontWeight.normal,
                          ),
                        ),
                      ),
                    ),
                    Expanded(
                      child: TextButton(
                        onPressed: () {
                          setState(() => _typing = true);
                          WidgetsBinding.instance.addPostFrameCallback((_) {
                            if (mounted) _focus.requestFocus();
                          });
                        },
                        child: Text(
                          'Type text',
                          style: TextStyle(
                            fontWeight: _typing
                                ? FontWeight.bold
                                : FontWeight.normal,
                          ),
                        ),
                      ),
                    ),
                    IconButton(
                      tooltip: 'Close keyboard',
                      onPressed: widget.onClose,
                      icon: const Icon(Icons.close),
                    ),
                  ],
                ),
                if (_typing)
                  TextField(
                    controller: _textController,
                    focusNode: _focus,
                    autocorrect: false,
                    enableSuggestions: false,
                    decoration: const InputDecoration(
                      hintText: 'Type to send keys to PC…',
                      border: OutlineInputBorder(),
                      isDense: true,
                    ),
                    onChanged: _onText,
                    onSubmitted: (_) {
                      sendRemoteChord(widget.sendKey, const [], 0x0D);
                      widget.onClose();
                    },
                  )
                else ...[
                  SingleChildScrollView(
                    scrollDirection: Axis.horizontal,
                    child: Row(
                      spacing: 8,
                      children: [
                        for (final shortcut in const [
                          ('Alt+Tab', [0xA4], 0x09),
                          ('Ctrl+C', [0xA2], 0x43),
                          ('Ctrl+V', [0xA2], 0x56),
                          ('Ctrl+Z', [0xA2], 0x5A),
                          ('Ctrl+Shift+Esc', [0xA2, 0xA0], 0x1B),
                          ('Win+D', [0x5B], 0x44),
                        ])
                          ActionChip(
                            label: Text(shortcut.$1),
                            onPressed: () =>
                                _send(shortcut.$1, shortcut.$2, shortcut.$3),
                          ),
                      ],
                    ),
                  ),
                  const SizedBox(height: 8),
                  Wrap(
                    spacing: 8,
                    children: [
                      for (final modifier in _modifiers.entries)
                        FilterChip(
                          label: Text(modifier.key),
                          selected: _selected.contains(modifier.value),
                          onSelected: (on) => setState(() {
                            on
                                ? _selected.add(modifier.value)
                                : _selected.remove(modifier.value);
                            _sent = null;
                          }),
                        ),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Row(
                    children: [
                      Expanded(
                        child: DropdownButtonFormField<String>(
                          initialValue: _key,
                          isExpanded: true,
                          menuMaxHeight: 280,
                          decoration: const InputDecoration(
                            labelText: 'Key',
                            border: OutlineInputBorder(),
                            isDense: true,
                          ),
                          items: [
                            for (final key in _keys.keys)
                              DropdownMenuItem(value: key, child: Text(key)),
                          ],
                          onChanged: (key) => setState(() {
                            _key = key!;
                            _sent = null;
                          }),
                        ),
                      ),
                      const SizedBox(width: 12),
                      FilledButton(
                        onPressed: () => _send(
                          chord,
                          _modifiers.values.where(_selected.contains).toList(),
                          _keys[_key]!,
                        ),
                        child: const Text('Send'),
                      ),
                    ],
                  ),
                  const SizedBox(height: 8),
                  Semantics(
                    liveRegion: true,
                    child: Text(
                      _sent ??
                          (chord == 'Ctrl+Alt+Delete' &&
                                  widget.onSecureAttention != null
                              ? '$chord · requires host secure-desktop service'
                              : '$chord · modifiers release after sending'),
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ),
                ],
              ],
            ),
          ),
        ),
      ),
    );
  }
}
