import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'src/rust/api.dart';

/// How single-finger drag maps to the remote mouse.
enum PointerMode {
  /// Finger position maps to a point on the PC screen (absolute).
  direct,
  /// Drag moves the cursor by delta, like a laptop trackpad (relative).
  trackpad,
}

/// Fullscreen stream viewer: pinch-zoom, two-finger pan, edge-drag menu.
class StreamPlayerScreen extends StatefulWidget {
  const StreamPlayerScreen({
    required this.status,
    required this.statusIsError,
    required this.isStreaming,
    required this.aspectRatio,
    required this.streamWidth,
    required this.streamHeight,
    required this.onStop,
    super.key,
  });

  final String? status;
  final bool statusIsError;
  final bool isStreaming;
  final double aspectRatio;
  final int streamWidth;
  final int streamHeight;
  final VoidCallback onStop;

  @override
  State<StreamPlayerScreen> createState() => _StreamPlayerScreenState();
}

class _StreamPlayerScreenState extends State<StreamPlayerScreen>
    with SingleTickerProviderStateMixin {
  static const _menuWidth = 260.0;
  static const _edgeGrabWidth = 28.0;
  static const _openThreshold = 72.0;
  static const _hintPeek = 72.0;
  /// Extra slide-off so shadows / sub-pixel rounding don't peek at the edge.
  static const _hideSlack = 32.0;

  final _transformController = TransformationController();
  late final AnimationController _hintController;
  late final Animation<double> _hintPeekAnim;

  double _menuDrag = 0;
  bool _menuOpen = false;
  bool _edgeDragActive = false;
  bool _hintPlayed = false;
  bool _keyboardOpen = false;
  bool _panEnabled = false;
  bool _blockViewerGestures = false;
  PointerMode _pointerMode = PointerMode.direct;
  final _keyboardFocus = FocusNode();
  final _keyboardCtrl = TextEditingController();

  bool get _showStatusOverlay =>
      !widget.isStreaming && (widget.statusIsError || widget.status != null);

  @override
  void initState() {
    super.initState();
    SystemChrome.setEnabledSystemUIMode(SystemUiMode.immersiveSticky);
    _transformController.addListener(_onTransformChanged);
    _hintController = AnimationController(
      vsync: this,
      duration: const Duration(milliseconds: 1300),
    );
    _hintPeekAnim = TweenSequence<double>([
      TweenSequenceItem(
        tween: Tween(begin: 0.0, end: _hintPeek)
            .chain(CurveTween(curve: Curves.easeOutCubic)),
        weight: 38,
      ),
      TweenSequenceItem(
        tween: Tween(begin: _hintPeek, end: 0.0)
            .chain(CurveTween(curve: Curves.easeInCubic)),
        weight: 62,
      ),
    ]).animate(_hintController);
    _hintController.addListener(_onHintTick);
    _hintController.addStatusListener(_onHintStatus);
  }

  void _onHintStatus(AnimationStatus status) {
    if (status == AnimationStatus.completed ||
        status == AnimationStatus.dismissed) {
      if (_menuDrag != 0 && !_menuOpen && !_edgeDragActive && mounted) {
        setState(() => _menuDrag = 0);
      }
    }
  }

  void _onHintTick() {
    if (!_hintController.isAnimating || _edgeDragActive || _menuOpen) return;
    final v = _hintPeekAnim.value < 0.5 ? 0.0 : _hintPeekAnim.value;
    if ((_menuDrag - v).abs() > 0.5) {
      setState(() => _menuDrag = v);
    }
  }

  @override
  void didUpdateWidget(StreamPlayerScreen oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (widget.isStreaming && !oldWidget.isStreaming && !_hintPlayed) {
      _hintPlayed = true;
      _hintController.forward(from: 0);
    }
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
    _hintController.removeListener(_onHintTick);
    _hintController.removeStatusListener(_onHintStatus);
    _hintController.dispose();
    _transformController.removeListener(_onTransformChanged);
    SystemChrome.setEnabledSystemUIMode(SystemUiMode.edgeToEdge);
    _transformController.dispose();
    _keyboardFocus.dispose();
    _keyboardCtrl.dispose();
    super.dispose();
  }

  void _closeMenu() {
    _hintController.stop();
    setState(() {
      _menuOpen = false;
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
    final screenW = MediaQuery.sizeOf(context).width;
    final fromRightEdge = details.globalPosition.dx >= screenW - _edgeGrabWidth;
    if (fromRightEdge || _menuOpen || _menuDrag > 0) {
      _hintController.stop();
      _edgeDragActive = true;
    }
  }

  void _onEdgeDragUpdate(DragUpdateDetails details) {
    if (!_edgeDragActive) return;
    setState(() {
      final base = _menuOpen ? _menuWidth : _menuDrag;
      _menuDrag = (base - details.delta.dx).clamp(0.0, _menuWidth);
      if (_menuOpen && _menuDrag < _menuWidth) {
        _menuOpen = false;
      }
    });
  }

  void _onEdgeDragEnd(DragEndDetails details) {
    if (!_edgeDragActive) return;
    _edgeDragActive = false;
    final velocity = details.primaryVelocity ?? 0;
    final flingOpen = velocity < -350;
    final flingClose = velocity > 350;
    final shouldOpen =
        !flingClose && (_menuDrag > _openThreshold || flingOpen);
    setState(() {
      _menuOpen = shouldOpen;
      _menuDrag = 0;
    });
  }

  double get _menuVisibleWidth => _menuOpen ? _menuWidth : _menuDrag;

  /// Positive X pushes the panel off the right edge of the screen.
  double get _menuSlideOff {
    if (_menuOpen) return 0;
    final visible = _menuDrag;
    if (visible <= 0) return _menuWidth + _hideSlack;
    return _menuWidth - visible;
  }

  bool get _menuBlocksPointer =>
      _menuOpen ||
      _edgeDragActive ||
      (_menuVisibleWidth > 12 && !_hintController.isAnimating);

  bool get _showMenuScrim =>
      (_menuVisibleWidth > 20 || _edgeDragActive) &&
      !_hintController.isAnimating;

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
        child: LayoutBuilder(
          builder: (context, constraints) {
            final menuSlide = _menuSlideOff;

            return Stack(
              clipBehavior: Clip.hardEdge,
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
                  child: Stack(
                    fit: StackFit.expand,
                    children: [
                      IgnorePointer(
                        ignoring: _blockViewerGestures,
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
                      if (widget.isStreaming &&
                          !_panEnabled &&
                          !_menuBlocksPointer &&
                          !_keyboardOpen)
                        _StreamTouchSurface(
                          videoSize: videoSize,
                          refWidth: widget.streamWidth,
                          refHeight: widget.streamHeight,
                          mode: _pointerMode,
                          onCaptureChanged: (capturing) {
                            if (_blockViewerGestures != capturing) {
                              setState(() => _blockViewerGestures = capturing);
                            }
                          },
                        ),
                    ],
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
          if (_showMenuScrim)
            GestureDetector(
              behavior: HitTestBehavior.opaque,
              onTap: _closeMenu,
              onHorizontalDragStart: _onEdgeDragStart,
              onHorizontalDragUpdate: _onEdgeDragUpdate,
              onHorizontalDragEnd: _onEdgeDragEnd,
              child: Container(color: Colors.black54.withValues(alpha: 0.35)),
            ),
          Positioned(
            top: 0,
            bottom: 0,
            right: 0,
            width: _menuWidth,
            child: Transform.translate(
              offset: Offset(menuSlide, 0),
              child: GestureDetector(
                onHorizontalDragStart: _onEdgeDragStart,
                onHorizontalDragUpdate: _onEdgeDragUpdate,
                onHorizontalDragEnd: _onEdgeDragEnd,
                child: _StreamMenu(
                  pointerMode: _pointerMode,
                  onPointerModeChanged: (mode) {
                    setState(() => _pointerMode = mode);
                    _closeMenu();
                  },
                  onStop: _handleStop,
                  onKeyboard: _openKeyboard,
                ),
              ),
            ),
          ),
          Positioned(
            right: 0,
            top: 0,
            bottom: 0,
            width: _edgeGrabWidth,
            child: GestureDetector(
              behavior: HitTestBehavior.translucent,
              onHorizontalDragStart: _onEdgeDragStart,
              onHorizontalDragUpdate: _onEdgeDragUpdate,
              onHorizontalDragEnd: _onEdgeDragEnd,
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
            );
          },
        ),
      ),
    );
  }
}

/// Maps touch on the video area to PC mouse events (when not zoomed).
class _StreamTouchSurface extends StatefulWidget {
  const _StreamTouchSurface({
    required this.videoSize,
    required this.refWidth,
    required this.refHeight,
    required this.mode,
    required this.onCaptureChanged,
  });

  final Size videoSize;
  final int refWidth;
  final int refHeight;
  final PointerMode mode;
  final ValueChanged<bool> onCaptureChanged;

  @override
  State<_StreamTouchSurface> createState() => _StreamTouchSurfaceState();
}

class _StreamTouchSurfaceState extends State<_StreamTouchSurface> {
  static const _tapSlop = 12.0;
  static const _tapTimeout = Duration(milliseconds: 280);

  final _pointers = <int, Offset>{};
  Offset? _singleOrigin;
  DateTime? _singleDownAt;
  double _singleTravel = 0;
  DateTime? _twoFingerDownAt;
  double _twoFingerTravel = 0;
  bool _twoFingerGesture = false;
  bool _twoFingerScrolled = false;
  Offset? _lastMovePos;
  Offset? _twoFingerLastCenter;

  int get _refW => widget.refWidth.clamp(1, 65535);
  int get _refH => widget.refHeight.clamp(1, 65535);

  void _sendAbs(Offset local) {
    final x = (local.dx / widget.videoSize.width * _refW).round().clamp(0, _refW);
    final y = (local.dy / widget.videoSize.height * _refH).round().clamp(0, _refH);
    sendMouseAbs(
      x: x,
      y: y,
      refW: _refW,
      refH: _refH,
    );
  }

  void _sendRel(Offset delta) {
    final scaleX = _refW / widget.videoSize.width;
    final scaleY = _refH / widget.videoSize.height;
    final dx = (delta.dx * scaleX).round().clamp(-32768, 32767);
    final dy = (delta.dy * scaleY).round().clamp(-32768, 32767);
    if (dx != 0 || dy != 0) {
      sendMouseMove(dx: dx, dy: dy);
    }
  }

  void _sendScroll(double dyPixels) {
    final lines = (dyPixels / 24).round().clamp(-32, 32);
    if (lines != 0) {
      sendMouseScroll(dx: 0, dy: -lines);
    }
  }

  void _setCapturing(bool on) {
    widget.onCaptureChanged(on);
  }

  void _onPointerDown(PointerDownEvent event) {
    _pointers[event.pointer] = event.localPosition;
    if (_pointers.length == 1) {
      _singleOrigin = event.localPosition;
      _singleDownAt = DateTime.now();
      _singleTravel = 0;
      _lastMovePos = event.localPosition;
      _setCapturing(true);
      if (widget.mode == PointerMode.direct) {
        _sendAbs(event.localPosition);
      }
    } else if (_pointers.length == 2) {
      _twoFingerGesture = true;
      _twoFingerScrolled = false;
      _twoFingerDownAt = DateTime.now();
      _twoFingerTravel = 0;
      _twoFingerLastCenter = _twoFingerCenter();
      _setCapturing(false);
    }
  }

  Offset? _twoFingerCenter() {
    if (_pointers.length < 2) return null;
    final pts = _pointers.values.toList(growable: false);
    return Offset((pts[0].dx + pts[1].dx) / 2, (pts[0].dy + pts[1].dy) / 2);
  }

  void _onPointerMove(PointerMoveEvent event) {
    _pointers[event.pointer] = event.localPosition;
    if (_pointers.length == 1 && _singleOrigin != null) {
      _singleTravel = math.max(
        _singleTravel,
        (event.localPosition - _singleOrigin!).distance,
      );
      if (widget.mode == PointerMode.direct) {
        _sendAbs(event.localPosition);
      } else {
        final last = _lastMovePos;
        if (last != null) {
          _sendRel(event.localPosition - last);
        }
        _lastMovePos = event.localPosition;
      }
    } else if (_pointers.length == 2 && _twoFingerDownAt != null) {
      final pts = _pointers.values.toList(growable: false);
      if (pts.length == 2) {
        _twoFingerTravel = math.max(
          _twoFingerTravel,
          (pts[0] - pts[1]).distance,
        );
      }
      final center = _twoFingerCenter();
      final prev = _twoFingerLastCenter;
      if (center != null && prev != null) {
        final dy = center.dy - prev.dy;
        if (dy.abs() > 1) {
          _twoFingerScrolled = true;
          _sendScroll(dy);
        }
      }
      _twoFingerLastCenter = center;
    }
  }

  void _onPointerUp(PointerUpEvent event) {
    final countBefore = _pointers.length;
    final twoStart = _twoFingerDownAt;
    final twoTravel = _twoFingerTravel;
    final twoGesture = _twoFingerGesture;
    final twoScroll = _twoFingerScrolled;
    final origin = _singleOrigin;
    final downAt = _singleDownAt;
    final travel = _singleTravel;

    _pointers.remove(event.pointer);

    if (_pointers.isEmpty) {
      if (twoGesture &&
          !twoScroll &&
          twoStart != null &&
          DateTime.now().difference(twoStart) < _tapTimeout &&
          twoTravel < _tapSlop) {
        sendMouseClick(button: 1);
      } else if (countBefore == 1 &&
          origin != null &&
          downAt != null &&
          travel < _tapSlop &&
          DateTime.now().difference(downAt) < _tapTimeout) {
        if (widget.mode == PointerMode.direct) {
          _sendAbs(origin);
        }
        sendMouseClick(button: 0);
      }
      _singleOrigin = null;
      _singleDownAt = null;
      _lastMovePos = null;
      _twoFingerDownAt = null;
      _twoFingerLastCenter = null;
      _twoFingerGesture = false;
      _twoFingerScrolled = false;
      _setCapturing(false);
    } else if (_pointers.length == 1) {
      _twoFingerDownAt = null;
      _twoFingerLastCenter = null;
      _twoFingerGesture = false;
      _twoFingerScrolled = false;
      _twoFingerTravel = 0;
      final remaining = _pointers.values.first;
      _singleOrigin = remaining;
      _singleDownAt = DateTime.now();
      _singleTravel = 0;
      _lastMovePos = remaining;
      _setCapturing(true);
      if (widget.mode == PointerMode.direct) {
        _sendAbs(remaining);
      }
    }
  }

  void _onPointerCancel(PointerCancelEvent event) {
    _pointers.remove(event.pointer);
    if (_pointers.isEmpty) {
      _singleOrigin = null;
      _singleDownAt = null;
      _lastMovePos = null;
      _twoFingerDownAt = null;
      _twoFingerLastCenter = null;
      _twoFingerGesture = false;
      _twoFingerScrolled = false;
      _setCapturing(false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Listener(
      behavior: HitTestBehavior.translucent,
      onPointerDown: _onPointerDown,
      onPointerMove: _onPointerMove,
      onPointerUp: _onPointerUp,
      onPointerCancel: _onPointerCancel,
      child: const SizedBox.expand(),
    );
  }
}

class _StreamMenu extends StatelessWidget {
  const _StreamMenu({
    required this.pointerMode,
    required this.onPointerModeChanged,
    required this.onStop,
    required this.onKeyboard,
  });

  final PointerMode pointerMode;
  final ValueChanged<PointerMode> onPointerModeChanged;
  final VoidCallback onStop;
  final VoidCallback onKeyboard;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final topPad = math.max(MediaQuery.paddingOf(context).top, 16.0);
    final trackpad = pointerMode == PointerMode.trackpad;

    return Material(
      color: theme.colorScheme.surface.withValues(alpha: 0.96),
      elevation: 0,
      shape: const Border(
        left: BorderSide(color: Color(0x33000000), width: 1),
      ),
      child: SafeArea(
        left: false,
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
                leading: Icon(trackpad ? Icons.swipe : Icons.touch_app),
                title: Text(trackpad ? 'Trackpad' : 'Direct touch'),
                subtitle: Text(
                  trackpad
                      ? 'Drag moves cursor · tap clicks'
                      : 'Finger maps to screen · tap clicks',
                  style: theme.textTheme.bodySmall,
                ),
                trailing: Switch(
                  value: trackpad,
                  onChanged: (on) => onPointerModeChanged(
                    on ? PointerMode.trackpad : PointerMode.direct,
                  ),
                ),
                onTap: () => onPointerModeChanged(
                  trackpad ? PointerMode.direct : PointerMode.trackpad,
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
              Padding(
                padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
                child: Text(
                  'Drag from the right edge for menu\n'
                  '1 finger: move · tap: left click\n'
                  '2 fingers: scroll · tap: right click',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.onSurfaceVariant,
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
