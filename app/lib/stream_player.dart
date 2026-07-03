import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// Fullscreen stream viewer: pinch-zoom, two-finger pan, edge-drag menu.
class StreamPlayerScreen extends StatefulWidget {
  const StreamPlayerScreen({
    required this.status,
    required this.statusIsError,
    required this.isStreaming,
    required this.aspectRatio,
    required this.onStop,
    super.key,
  });

  final String? status;
  final bool statusIsError;
  final bool isStreaming;
  final double aspectRatio;
  final VoidCallback onStop;

  @override
  State<StreamPlayerScreen> createState() => _StreamPlayerScreenState();
}

class _StreamPlayerScreenState extends State<StreamPlayerScreen> {
  static const _menuWidth = 260.0;
  static const _edgeGrabWidth = 28.0;
  static const _openThreshold = 72.0;

  final _transformController = TransformationController();

  double _menuDrag = 0;
  bool _menuOpen = false;
  bool _edgeDragActive = false;
  bool _keyboardOpen = false;
  bool _panEnabled = false;
  final _keyboardFocus = FocusNode();
  final _keyboardCtrl = TextEditingController();

  bool get _showStatusOverlay =>
      !widget.isStreaming && (widget.statusIsError || widget.status != null);

  @override
  void initState() {
    super.initState();
    SystemChrome.setEnabledSystemUIMode(SystemUiMode.immersiveSticky);
    _transformController.addListener(_onTransformChanged);
  }

  void _onTransformChanged() {
    final scale = _transformController.value.getMaxScaleOnAxis();
    final pan = scale > 1.01;
    if (pan != _panEnabled && mounted) {
      setState(() => _panEnabled = pan);
    }
  }

  @override
  void dispose() {
    _transformController.removeListener(_onTransformChanged);
    SystemChrome.setEnabledSystemUIMode(SystemUiMode.edgeToEdge);
    _transformController.dispose();
    _keyboardFocus.dispose();
    _keyboardCtrl.dispose();
    super.dispose();
  }

  void _closeMenu() {
    setState(() {
      _menuOpen = false;
      _menuDrag = 0;
    });
  }

  void _toggleMenu() {
    setState(() {
      _menuOpen = !_menuOpen;
      _menuDrag = 0;
    });
  }

  void _openKeyboard() {
    _closeMenu();
    setState(() => _keyboardOpen = true);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (mounted) _keyboardFocus.requestFocus();
    });
  }

  void _closeKeyboard() {
    _keyboardFocus.unfocus();
    setState(() => _keyboardOpen = false);
  }

  void _handleStop() {
    _closeMenu();
    _closeKeyboard();
    widget.onStop();
  }

  void _onEdgeDragStart(DragStartDetails details) {
    final fromEdge = details.globalPosition.dx <= _edgeGrabWidth;
    if (fromEdge || _menuOpen || _menuDrag > 0) {
      _edgeDragActive = true;
    }
  }

  void _onEdgeDragUpdate(DragUpdateDetails details) {
    if (!_edgeDragActive) return;
    setState(() {
      final base = _menuOpen ? _menuWidth : 0.0;
      _menuDrag = (base + details.delta.dx).clamp(0, _menuWidth);
    });
  }

  void _onEdgeDragEnd(DragEndDetails details) {
    if (!_edgeDragActive) return;
    _edgeDragActive = false;
    final open = _menuDrag > _openThreshold ||
        details.primaryVelocity != null && details.primaryVelocity! > 200;
    setState(() {
      _menuOpen = open;
      _menuDrag = 0;
    });
  }

  double get _menuVisibleWidth => _menuOpen ? _menuWidth : _menuDrag;

  Size _fitVideoSize(Size viewport) {
    final ratio = widget.aspectRatio;
    if (ratio <= 0 || !ratio.isFinite) {
      return viewport;
    }
    final viewportRatio = viewport.width / viewport.height;
    if (viewportRatio > ratio) {
      final height = viewport.height;
      return Size(height * ratio, height);
    }
    final width = viewport.width;
    return Size(width, width / ratio);
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);

    return PopScope(
      canPop: false,
      onPopInvokedWithResult: (didPop, result) {
        if (didPop) return;
        if (_keyboardOpen) {
          _closeKeyboard();
          return;
        }
        if (_menuOpen || _menuDrag > 0) {
          _closeMenu();
          return;
        }
        _handleStop();
      },
      child: Material(
        color: Colors.black,
        child: Stack(
        fit: StackFit.expand,
        children: [
          LayoutBuilder(
            builder: (context, constraints) {
              final viewport = Size(constraints.maxWidth, constraints.maxHeight);
              final videoSize = _fitVideoSize(viewport);
              return Center(
                child: SizedBox(
                  width: videoSize.width,
                  height: videoSize.height,
                  child: InteractiveViewer(
                    transformationController: _transformController,
                    minScale: 1,
                    maxScale: 4,
                    panEnabled: _panEnabled,
                    clipBehavior: Clip.none,
                    child: SizedBox(
                      width: videoSize.width,
                      height: videoSize.height,
                      child: const AndroidView(
                        viewType: 'streambeam/video',
                        layoutDirection: TextDirection.ltr,
                      ),
                    ),
                  ),
                ),
              );
            },
          ),
          if (_showStatusOverlay)
            ColoredBox(
              color: Colors.black87,
              child: Center(
                child: Padding(
                  padding: const EdgeInsets.all(24),
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      if (!widget.statusIsError)
                        const CircularProgressIndicator(color: Colors.white),
                      if (!widget.statusIsError) const SizedBox(height: 16),
                      if (widget.statusIsError)
                        const Icon(
                          Icons.error_outline,
                          color: Colors.white,
                          size: 40,
                        ),
                      if (widget.statusIsError) const SizedBox(height: 16),
                      Text(
                        widget.status ?? 'Connecting…',
                        textAlign: TextAlign.center,
                        style: theme.textTheme.bodyLarge?.copyWith(
                          color: Colors.white,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          if (_menuVisibleWidth > 0 || _edgeDragActive)
            GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: _menuOpen ? _closeMenu : null,
              child: Container(color: Colors.black54.withValues(alpha: 0.35)),
            ),
          Positioned(
            left: _menuVisibleWidth - _menuWidth,
            top: 0,
            bottom: 0,
            width: _menuWidth,
            child: _StreamMenu(
              onStop: _handleStop,
              onKeyboard: _openKeyboard,
            ),
          ),
          Positioned(
            left: 0,
            top: 0,
            bottom: 0,
            width: _edgeGrabWidth,
            child: GestureDetector(
              behavior: HitTestBehavior.translucent,
              onHorizontalDragStart: _onEdgeDragStart,
              onHorizontalDragUpdate: _onEdgeDragUpdate,
              onHorizontalDragEnd: _onEdgeDragEnd,
              onTap: _toggleMenu,
              child: Align(
                alignment: Alignment.centerLeft,
                child: AnimatedOpacity(
                  opacity: widget.isStreaming && !_menuOpen ? 0.5 : 0,
                  duration: const Duration(milliseconds: 200),
                  child: Container(
                    margin: const EdgeInsets.only(left: 4),
                    width: 4,
                    height: 48,
                    decoration: BoxDecoration(
                      color: Colors.white54,
                      borderRadius: BorderRadius.circular(2),
                    ),
                  ),
                ),
              ),
            ),
          ),
          if (widget.isStreaming && _menuVisibleWidth == 0 && !_keyboardOpen)
            Positioned(
              right: 16,
              bottom: MediaQuery.paddingOf(context).bottom + 16,
              child: IconButton(
                style: IconButton.styleFrom(
                  backgroundColor: Colors.black45,
                  foregroundColor: Colors.white,
                ),
                onPressed: _toggleMenu,
                icon: const Icon(Icons.menu),
                tooltip: 'Menu',
              ),
            ),
          if (_keyboardOpen) ...[
            GestureDetector(
              onTap: _closeKeyboard,
              child: Container(color: Colors.black54),
            ),
            Align(
              alignment: Alignment.bottomCenter,
              child: Material(
                color: theme.colorScheme.surface,
                elevation: 8,
                child: SafeArea(
                  top: false,
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(16, 12, 16, 12),
                    child: Row(
                      children: [
                        Expanded(
                          child: TextField(
                            controller: _keyboardCtrl,
                            focusNode: _keyboardFocus,
                            autofocus: true,
                            decoration: InputDecoration(
                              hintText: 'Type to send keys to PC…',
                              border: const OutlineInputBorder(),
                              isDense: true,
                              suffixIcon: IconButton(
                                icon: const Icon(Icons.close),
                                onPressed: _closeKeyboard,
                              ),
                            ),
                            onSubmitted: (_) => _closeKeyboard(),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          ],
        ],
      ),
      ),
    );
  }
}

class _StreamMenu extends StatelessWidget {
  const _StreamMenu({
    required this.onStop,
    required this.onKeyboard,
  });

  final VoidCallback onStop;
  final VoidCallback onKeyboard;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final topPad = math.max(MediaQuery.paddingOf(context).top, 16.0);

    return Material(
      color: theme.colorScheme.surface.withValues(alpha: 0.96),
      elevation: 8,
      child: SafeArea(
        right: false,
        child: Padding(
          padding: EdgeInsets.fromLTRB(8, topPad, 8, 16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(12, 0, 12, 16),
                child: Text(
                  'StreamBeam',
                  style: theme.textTheme.titleMedium,
                ),
              ),
              ListTile(
                leading: const Icon(Icons.stop_circle_outlined),
                title: const Text('Stop'),
                onTap: onStop,
              ),
              ListTile(
                leading: const Icon(Icons.keyboard_outlined),
                title: const Text('Keyboard'),
                subtitle: Text(
                  'Send text to your PC',
                  style: theme.textTheme.bodySmall,
                ),
                onTap: onKeyboard,
              ),
            ],
          ),
        ),
      ),
    );
  }
}
