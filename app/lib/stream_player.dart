import 'dart:async';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'gamepad_forwarder.dart';
import 'src/rust/api.dart';

/// How single-finger drag maps to the remote mouse.
enum PointerMode {
  /// Finger position maps to a point on the PC screen (absolute).
  direct,
  /// Drag moves the cursor by delta, like a laptop trackpad (relative).
  trackpad,
}

/// Corner the floating menu button snaps to on mouse-first platforms.
enum _MenuCorner { topLeft, topRight, bottomLeft, bottomRight }

/// Fullscreen stream viewer: pinch-zoom, two-finger pan, edge-drag menu.
///
/// Shared by every platform — the caller injects the platform's [video]
/// widget (AndroidView on Android, a Flutter [Texture] on desktop) and
/// declares its capabilities; everything else (menu, diagnostics, gestures,
/// overlays) is one implementation. Don't fork this per platform.
class StreamPlayerScreen extends StatefulWidget {
  const StreamPlayerScreen({
    required this.status,
    required this.statusIsError,
    required this.isStreaming,
    required this.aspectRatio,
    required this.streamWidth,
    required this.streamHeight,
    required this.video,
    required this.onStop,
    this.inputSupported = true,
    this.touchControls = true,
    this.upscaleSupported = false,
    this.upscaleActive = false,
    this.onToggleUpscale,
    super.key,
  });

  final String? status;
  final bool statusIsError;
  final bool isStreaming;
  final double aspectRatio;
  final int streamWidth;
  final int streamHeight;

  /// The platform's video surface, sized/positioned by this screen.
  final Widget video;
  final VoidCallback onStop;

  /// Whether this build can capture & send input to the host at all.
  /// False skips the input layers entirely.
  final bool inputSupported;

  /// Whether the primary pointer is touch (phone). False (desktop mouse)
  /// hides the touch-only UI (pointer-mode picker, keyboard, edge-drag
  /// zone) and shows a menu button instead; mouse events always map
  /// directly (hover moves, real button press/release, wheel scroll).
  final bool touchControls;

  /// GPU upscaling (MetalFX on macOS): shown in the menu when supported.
  final bool upscaleSupported;
  final bool upscaleActive;
  final VoidCallback? onToggleUpscale;

  @override
  State<StreamPlayerScreen> createState() => _StreamPlayerScreenState();
}

class _StreamPlayerScreenState extends State<StreamPlayerScreen>
    with SingleTickerProviderStateMixin {
  static const _openThreshold = 48.0;
  static const _hintPeek = 72.0;
  /// Extra slide-off so shadows / sub-pixel rounding don't peek at the edge.
  static const _hideSlack = 32.0;

  double _menuWidthFor(double screenW) =>
      (screenW * 0.78).clamp(220.0, 300.0);

  double get _menuWidth => _menuWidthFor(MediaQuery.sizeOf(context).width);

  bool get _menuOnLeft =>
      _menuCorner == _MenuCorner.topLeft || _menuCorner == _MenuCorner.bottomLeft;

  /// Width of the right-edge zone that opens the menu (~14% of screen, 64–96 logical px).
  double _edgeGrabFor(double screenW) =>
      (screenW * 0.14).clamp(64.0, 96.0);

  final _transformController = TransformationController();
  late final AnimationController _hintController;
  late final Animation<double> _hintPeekAnim;

  double _menuDrag = 0;
  bool _menuOpen = false;
  /// Where the floating menu button lives (mouse-first platforms). Left
  /// corners also anchor the slide-in panel to the left edge.
  _MenuCorner _menuCorner = _MenuCorner.topRight;
  /// Button position while it's being dragged between corners.
  Offset? _buttonDragPos;
  bool _edgeDragActive = false;
  bool _hintPlayed = false;
  bool _keyboardOpen = false;
  bool _hasVideo = false;
  bool _videoTimedOut = false;
  DateTime? _streamStartedAt;
  Timer? _videoPoll;
  Timer? _diagPoll;
  bool _panEnabled = false;
  bool _blockViewerGestures = false;
  bool _multiTouchSession = false;
  PointerMode _pointerMode = PointerMode.trackpad;
  final _liveDiagListenable = ValueNotifier(const _LiveDiagnostics());
  int _diagLastBytes = 0;
  int _diagLastFrames = 0;
  DateTime? _diagLastSampleAt;
  final _keyboardFocus = FocusNode();
  final _keyboardCtrl = TextEditingController();

  bool get _showStatusOverlay =>
      !widget.isStreaming && (widget.statusIsError || widget.status != null);

  @override
  void initState() {
    super.initState();
    if (Platform.isAndroid) {
      SystemChrome.setEnabledSystemUIMode(SystemUiMode.immersiveSticky);
    }
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
    // The screen can mount with the stream already live (desktop connects
    // before showing the player) — didUpdateWidget won't fire for that.
    if (widget.isStreaming) {
      _hintPlayed = true;
      _hintController.forward(from: 0);
      _beginVideoWatch();
      _beginDiagnostics();
    }
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
    if (widget.isStreaming && !oldWidget.isStreaming) {
      _beginVideoWatch();
      _beginDiagnostics();
    } else if (!widget.isStreaming && oldWidget.isStreaming) {
      _endVideoWatch();
      _endDiagnostics();
    }
  }

  void _beginVideoWatch() {
    _hasVideo = false;
    _videoTimedOut = false;
    _streamStartedAt = DateTime.now();
    _videoPoll?.cancel();
    _videoPoll = Timer.periodic(const Duration(milliseconds: 400), (_) {
      if (!mounted || !widget.isStreaming) return;
      final frames = playbackFramesRendered();
      if (frames > BigInt.zero && !_hasVideo) {
        setState(() {
          _hasVideo = true;
          _videoTimedOut = false;
        });
        _videoPoll?.cancel();
        _videoPoll = null;
        return;
      }
      final started = _streamStartedAt;
      if (!_videoTimedOut &&
          started != null &&
          DateTime.now().difference(started) > const Duration(seconds: 12)) {
        setState(() => _videoTimedOut = true);
      }
    });
  }

  void _endVideoWatch() {
    _videoPoll?.cancel();
    _videoPoll = null;
    _streamStartedAt = null;
    _hasVideo = false;
    _videoTimedOut = false;
  }

  void _beginDiagnostics() {
    _diagPoll?.cancel();
    _diagPoll = null;
    _liveDiagListenable.value = const _LiveDiagnostics();
    _diagLastBytes = 0;
    _diagLastFrames = 0;
    _diagLastSampleAt = null;
    _syncDiagnosticsPoll();
  }

  void _endDiagnostics() {
    _diagPoll?.cancel();
    _diagPoll = null;
    _liveDiagListenable.value = const _LiveDiagnostics();
  }

  void _syncDiagnosticsPoll() {
    if (!widget.isStreaming || !_menuOpen) {
      _diagPoll?.cancel();
      _diagPoll = null;
      return;
    }
    if (_diagPoll != null) return;
    _updateDiagnostics();
    _diagPoll = Timer.periodic(const Duration(milliseconds: 500), (_) {
      _updateDiagnostics();
    });
  }

  void _updateDiagnostics() {
    if (!mounted || !widget.isStreaming) return;
    final snap = playbackDiagnostics();
    final now = DateTime.now();
    var bytesPerSec = _liveDiagListenable.value.bytesPerSec;
    var renderFps = _liveDiagListenable.value.renderFps;
    final bytes = snap.bytesReceived.toInt();
    final frames = snap.framesRendered.toInt();
    final sampleAt = _diagLastSampleAt;
    if (sampleAt != null) {
      final elapsed = now.difference(sampleAt).inMilliseconds / 1000.0;
      if (elapsed >= 0.25) {
        bytesPerSec = (bytes - _diagLastBytes) / elapsed;
        renderFps = (frames - _diagLastFrames) / elapsed;
        _diagLastBytes = bytes;
        _diagLastFrames = frames;
        _diagLastSampleAt = now;
      }
    } else {
      _diagLastBytes = bytes;
      _diagLastFrames = frames;
      _diagLastSampleAt = now;
    }

    final width = snap.width > 0 ? snap.width : widget.streamWidth;
    final height = snap.height > 0 ? snap.height : widget.streamHeight;
    final next = _LiveDiagnostics(
      width: width,
      height: height,
      targetFps: snap.fps,
      renderFps: renderFps,
      codec: snap.codec,
      bytesPerSec: bytesPerSec,
    );
    if (next != _liveDiagListenable.value) {
      _liveDiagListenable.value = next;
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
    _videoPoll?.cancel();
    _diagPoll?.cancel();
    _hintController.removeListener(_onHintTick);
    _hintController.removeStatusListener(_onHintStatus);
    _hintController.dispose();
    _transformController.removeListener(_onTransformChanged);
    // Restore the system bars (back/home) when leaving the fullscreen player.
    if (Platform.isAndroid) {
      SystemChrome.setEnabledSystemUIMode(
        SystemUiMode.manual,
        overlays: SystemUiOverlay.values,
      );
    }
    _transformController.dispose();
    _liveDiagListenable.dispose();
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
    _syncDiagnosticsPoll();
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
    final grab = _edgeGrabFor(screenW);
    final fromMenuEdge = _menuOnLeft
        ? details.globalPosition.dx <= grab
        : details.globalPosition.dx >= screenW - grab;
    if (fromMenuEdge || _menuOpen || _menuDrag > 0) {
      _hintController.stop();
      _edgeDragActive = true;
    }
  }

  void _onEdgeDragUpdate(DragUpdateDetails details) {
    if (!_edgeDragActive) return;
    setState(() {
      final base = _menuOpen ? _menuWidth : _menuDrag;
      // Dragging away from the anchored edge reveals the panel.
      final towardOpen = _menuOnLeft ? details.delta.dx : -details.delta.dx;
      _menuDrag = (base + towardOpen).clamp(0.0, _menuWidth);
      if (_menuOpen && _menuDrag < _menuWidth) {
        _menuOpen = false;
      }
    });
  }

  void _onEdgeDragEnd(DragEndDetails details) {
    if (!_edgeDragActive) return;
    _edgeDragActive = false;
    final velocity = details.primaryVelocity ?? 0;
    final towardOpen = _menuOnLeft ? velocity : -velocity;
    final flingOpen = towardOpen > 250;
    final flingClose = towardOpen < -250;
    final shouldOpen =
        !flingClose && (_menuDrag > _openThreshold || flingOpen);
    setState(() {
      _menuOpen = shouldOpen;
      _menuDrag = 0;
    });
    _syncDiagnosticsPoll();
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

  /// Floating menu button (mouse-first platforms): tap opens the panel,
  /// drag snaps it to any corner — a left corner also moves the panel to
  /// the left edge. One stable Positioned so the drag gesture survives
  /// position updates.
  Widget _buildMenuButton(Size viewport) {
    const buttonSize = 48.0;
    const inset = 10.0;
    final pad = MediaQuery.paddingOf(context);
    final drag = _buttonDragPos;

    final double left;
    final double top;
    if (drag != null) {
      left = (drag.dx - buttonSize / 2)
          .clamp(0.0, viewport.width - buttonSize);
      top = (drag.dy - buttonSize / 2)
          .clamp(0.0, viewport.height - buttonSize);
    } else {
      left = _menuOnLeft
          ? inset + pad.left
          : viewport.width - buttonSize - inset - pad.right;
      top = (_menuCorner == _MenuCorner.topLeft ||
              _menuCorner == _MenuCorner.topRight)
          ? inset + pad.top
          : viewport.height - buttonSize - inset - pad.bottom;
    }

    return Positioned(
      left: left,
      top: top,
      width: buttonSize,
      height: buttonSize,
      child: GestureDetector(
        onPanStart: (d) => setState(() => _buttonDragPos = d.globalPosition),
        onPanUpdate: (d) => setState(() => _buttonDragPos = d.globalPosition),
        onPanEnd: (_) => _snapMenuButton(viewport),
        onPanCancel: () => _snapMenuButton(viewport),
        child: Opacity(
          opacity: drag != null ? 1.0 : 0.75,
          child: IconButton.filledTonal(
            tooltip: 'Menu (drag to move)',
            icon: const Icon(Icons.menu),
            onPressed: () {
              setState(() {
                _menuOpen = true;
                _menuDrag = 0;
              });
              _syncDiagnosticsPoll();
            },
          ),
        ),
      ),
    );
  }

  void _snapMenuButton(Size viewport) {
    final pos = _buttonDragPos;
    if (pos == null) return;
    final onLeft = pos.dx < viewport.width / 2;
    final onTop = pos.dy < viewport.height / 2;
    setState(() {
      _menuCorner = onTop
          ? (onLeft ? _MenuCorner.topLeft : _MenuCorner.topRight)
          : (onLeft ? _MenuCorner.bottomLeft : _MenuCorner.bottomRight);
      _buttonDragPos = null;
    });
  }

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
            final viewport = Size(constraints.maxWidth, constraints.maxHeight);
            final videoSize = _fitVideoSize(viewport);
            final videoRect = Rect.fromLTWH(
              (viewport.width - videoSize.width) / 2,
              (viewport.height - videoSize.height) / 2,
              videoSize.width,
              videoSize.height,
            );
            final menuSlide = _menuSlideOff;
            // Mouse-first platforms get a menu button instead of the
            // right-edge drag zone (which would swallow clicks near it).
            final edgeGrab =
                widget.touchControls ? _edgeGrabFor(constraints.maxWidth) : 0.0;
            final showStreamInput = widget.inputSupported &&
                widget.isStreaming &&
                _hasVideo &&
                !_menuBlocksPointer &&
                !_keyboardOpen;
            final mouseControlEnabled = showStreamInput;

            return Stack(
              clipBehavior: Clip.hardEdge,
              fit: StackFit.expand,
              children: [
                Center(
                  child: SizedBox(
                    width: videoSize.width,
                    height: videoSize.height,
                    child: IgnorePointer(
                      ignoring: _blockViewerGestures && !_multiTouchSession,
                      child: InteractiveViewer(
                        transformationController: _transformController,
                        minScale: 1,
                        maxScale: 4,
                        panEnabled: _panEnabled && _multiTouchSession,
                        clipBehavior: Clip.none,
                        child: SizedBox(
                          width: videoSize.width,
                          height: videoSize.height,
                          child: widget.video,
                        ),
                      ),
                    ),
                  ),
                ),
                if (showStreamInput && widget.touchControls)
                  // Android-only EventChannel; no native side elsewhere yet.
                  GamepadForwarder(enabled: showStreamInput),
                if (showStreamInput)
                  _StreamInputLayer(
                    key: const ValueKey('stream-input'),
                    videoRect: videoRect,
                    videoSize: videoSize,
                    refWidth: widget.streamWidth,
                    refHeight: widget.streamHeight,
                    mode: _pointerMode,
                    screenEdgeGrab: edgeGrab,
                    mouseControlEnabled: mouseControlEnabled,
                    multiTouchSession: _multiTouchSession,
                    onCaptureChanged: (capturing) {
                      if (_blockViewerGestures != capturing) {
                        setState(() => _blockViewerGestures = capturing);
                      }
                    },
                    onMultiTouchSessionChanged: (active) {
                      if (_multiTouchSession != active) {
                        setState(() {
                          _multiTouchSession = active;
                          if (active) {
                            _blockViewerGestures = false;
                          }
                        });
                      }
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
          if (widget.isStreaming && !_hasVideo)
            ColoredBox(
              color: Colors.black,
              child: Center(
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 32),
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Icon(
                        _videoTimedOut
                            ? Icons.videocam_off_outlined
                            : Icons.hourglass_top,
                        color: Colors.white70,
                        size: 48,
                      ),
                      const SizedBox(height: 16),
                      Text(
                        _videoTimedOut
                            ? 'No video from your PC'
                            : 'Waiting for video…',
                        textAlign: TextAlign.center,
                        style: theme.textTheme.titleMedium?.copyWith(
                          color: Colors.white,
                        ),
                      ),
                      const SizedBox(height: 10),
                      Text(
                        _videoTimedOut
                            ? 'Check that streambeam-host is running on your PC and both devices are on the same Wi‑Fi. The host may still be using a test clip — if this persists, restart the host app.'
                            : 'Connected to your PC. The first video frame has not arrived yet.',
                        textAlign: TextAlign.center,
                        style: theme.textTheme.bodyMedium?.copyWith(
                          color: Colors.white60,
                        ),
                      ),
                      if (!_videoTimedOut) ...[
                        const SizedBox(height: 20),
                        const SizedBox(
                          width: 28,
                          height: 28,
                          child: CircularProgressIndicator(
                            strokeWidth: 2,
                            color: Colors.white54,
                          ),
                        ),
                      ],
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
            left: _menuOnLeft ? 0 : null,
            right: _menuOnLeft ? null : 0,
            width: _menuWidth,
            child: Transform.translate(
              offset: Offset(_menuOnLeft ? -menuSlide : menuSlide, 0),
              child: Builder(builder: (context) {
                // Drag handle only — don't wrap the whole menu or taps won't reach it.
                final handle = GestureDetector(
                  behavior: HitTestBehavior.opaque,
                  onHorizontalDragStart: _onEdgeDragStart,
                  onHorizontalDragUpdate: _onEdgeDragUpdate,
                  onHorizontalDragEnd: _onEdgeDragEnd,
                  child: const SizedBox(width: 20),
                );
                final menu = Expanded(
                  child: ValueListenableBuilder<_LiveDiagnostics>(
                    valueListenable: _liveDiagListenable,
                    builder: (context, liveDiag, _) => _StreamMenu(
                      diagnostics: liveDiag,
                      showDiagnostics: widget.isStreaming,
                      touchControls: widget.inputSupported && widget.touchControls,
                      anchorLeft: _menuOnLeft,
                      pointerMode: _pointerMode,
                      onPointerModeChanged: (mode) {
                        setState(() {
                          _pointerMode = mode;
                          _blockViewerGestures = false;
                          _multiTouchSession = false;
                        });
                      },
                      upscaleSupported: widget.upscaleSupported,
                      upscaleActive: widget.upscaleActive,
                      onToggleUpscale: widget.onToggleUpscale,
                      onStop: _handleStop,
                      onKeyboard: _openKeyboard,
                    ),
                  ),
                );
                return Row(
                  children:
                      _menuOnLeft ? [menu, handle] : [handle, menu],
                );
              }),
            ),
          ),
          if (widget.touchControls)
            Positioned(
              right: 0,
              top: 0,
              bottom: 0,
              width: edgeGrab,
              child: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onHorizontalDragStart: _onEdgeDragStart,
                onHorizontalDragUpdate: _onEdgeDragUpdate,
                onHorizontalDragEnd: _onEdgeDragEnd,
                child: const SizedBox.expand(),
              ),
            )
          else if (!_menuOpen)
            _buildMenuButton(viewport),
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

/// Tracks pointers and maps single-finger touch to PC mouse when [mouseControlEnabled].
/// Two or more fingers pass through to [InteractiveViewer] for pinch/pan (no mouse).
class _StreamInputLayer extends StatefulWidget {
  const _StreamInputLayer({
    super.key,
    required this.videoRect,
    required this.videoSize,
    required this.refWidth,
    required this.refHeight,
    required this.mode,
    required this.screenEdgeGrab,
    required this.mouseControlEnabled,
    required this.multiTouchSession,
    required this.onCaptureChanged,
    required this.onMultiTouchSessionChanged,
  });

  final Rect videoRect;
  final Size videoSize;
  final int refWidth;
  final int refHeight;
  final PointerMode mode;
  final double screenEdgeGrab;
  final bool mouseControlEnabled;
  final bool multiTouchSession;
  final ValueChanged<bool> onCaptureChanged;
  final ValueChanged<bool> onMultiTouchSessionChanged;

  @override
  State<_StreamInputLayer> createState() => _StreamInputLayerState();
}

class _StreamInputLayerState extends State<_StreamInputLayer> {
  static const _tapSlop = 12.0;
  static const _pinchSlop = 16.0;
  static const _tapTimeout = Duration(milliseconds: 280);
  static const _relFlushInterval = Duration(milliseconds: 8);

  final _activePointers = <int>{};
  final _pointers = <int, Offset>{};
  /// Currently-pressed mouse buttons (Flutter bitmask), for real
  /// press/release semantics (drag & select) on mouse-driven platforms.
  int _mouseButtons = 0;
  double _scrollAccumX = 0;
  double _scrollAccumY = 0;
  Offset? _singleOrigin;
  DateTime? _singleDownAt;
  double _singleTravel = 0;
  Offset? _lastMovePos;
  double _relAccumX = 0;
  double _relAccumY = 0;
  Timer? _relFlushTimer;

  bool _pendingTwoFinger = false;
  double? _twoFingerStartSpan;
  double _twoFingerTravel = 0;
  DateTime? _twoFingerDownAt;

  int get _refW => widget.refWidth.clamp(1, 65535);
  int get _refH => widget.refHeight.clamp(1, 65535);

  bool get _multiFingerContact =>
      widget.multiTouchSession ||
      _pendingTwoFinger ||
      _pointers.length >= 2;

  @override
  void didUpdateWidget(_StreamInputLayer oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.mode != widget.mode) {
      _resetMouseState();
    }
    if (!widget.mouseControlEnabled || widget.multiTouchSession) {
      _setCapturing(false);
    }
    if (!widget.mouseControlEnabled && _mouseButtons != 0) {
      _syncMouseButtons(0);
    }
  }

  @override
  void dispose() {
    _cancelRelFlush();
    _flushRel();
    if (_mouseButtons != 0) _syncMouseButtons(0);
    _setCapturing(false);
    super.dispose();
  }

  void _cancelRelFlush() {
    _relFlushTimer?.cancel();
    _relFlushTimer = null;
  }

  void _resetMouseState() {
    _cancelRelFlush();
    _flushRel();
    _activePointers.clear();
    _pointers.clear();
    _singleOrigin = null;
    _singleDownAt = null;
    _singleTravel = 0;
    _lastMovePos = null;
    _relAccumX = 0;
    _relAccumY = 0;
    _pendingTwoFinger = false;
    _twoFingerStartSpan = null;
    _twoFingerTravel = 0;
    _twoFingerDownAt = null;
    _setCapturing(false);
  }

  Offset? _videoLocal(Offset stackLocal, {bool clamp = false}) {
    if (widget.videoRect.contains(stackLocal)) {
      return stackLocal - widget.videoRect.topLeft;
    }
    if (!clamp) return null;
    final x = (stackLocal.dx - widget.videoRect.left)
        .clamp(0.0, widget.videoSize.width);
    final y = (stackLocal.dy - widget.videoRect.top)
        .clamp(0.0, widget.videoSize.height);
    return Offset(x, y);
  }

  void _sendAbs(Offset videoLocal) {
    final x = (videoLocal.dx / widget.videoSize.width * _refW)
        .round()
        .clamp(0, _refW);
    final y = (videoLocal.dy / widget.videoSize.height * _refH)
        .round()
        .clamp(0, _refH);
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
    _relAccumX += delta.dx * scaleX;
    _relAccumY += delta.dy * scaleY;
    _scheduleRelFlush();
  }

  void _scheduleRelFlush() {
    if (_relFlushTimer != null) return;
    _relFlushTimer = Timer(_relFlushInterval, () {
      _relFlushTimer = null;
      _flushRel();
    });
  }

  void _flushRel() {
    final dx = _relAccumX.round().clamp(-32768, 32767);
    final dy = _relAccumY.round().clamp(-32768, 32767);
    if (dx == 0 && dy == 0) return;
    _relAccumX -= dx;
    _relAccumY -= dy;
    sendMouseMove(dx: dx, dy: dy);
    if (_relAccumX.round() != 0 || _relAccumY.round() != 0) {
      _scheduleRelFlush();
    }
  }

  void _setCapturing(bool on) {
    widget.onCaptureChanged(on);
  }

  bool _isInMenuEdgeZone(Offset global) {
    if (widget.screenEdgeGrab <= 0) return false;
    final screenW = MediaQuery.sizeOf(context).width;
    return global.dx >= screenW - widget.screenEdgeGrab;
  }

  // ---- Mouse path (desktop, or a mouse attached to a phone) ----
  // Touch gestures synthesize taps/drags; a real mouse maps 1:1 instead:
  // hover/move → absolute position, buttons → press/release, wheel → scroll.

  static bool _isMouse(PointerEvent e) =>
      e.kind == PointerDeviceKind.mouse || e.kind == PointerDeviceKind.trackpad;

  /// Diff [next] against the held bitmask and send press/release events.
  void _syncMouseButtons(int next) {
    const mapping = [
      [kPrimaryMouseButton, 0],
      [kSecondaryMouseButton, 1],
      [kMiddleMouseButton, 2],
    ];
    for (final m in mapping) {
      final wasDown = _mouseButtons & m[0] != 0;
      final isDown = next & m[0] != 0;
      if (wasDown != isDown) {
        sendMouseButton(button: m[1], down: isDown);
      }
    }
    _mouseButtons = next & (kPrimaryMouseButton | kSecondaryMouseButton | kMiddleMouseButton);
  }

  void _onMouseDown(PointerDownEvent event) {
    if (!widget.mouseControlEnabled) return;
    final local = _videoLocal(event.localPosition);
    if (local == null) return; // clicks on the letterbox stay local
    _sendAbs(local);
    _syncMouseButtons(event.buttons);
  }

  void _onMouseMove(PointerMoveEvent event) {
    if (!widget.mouseControlEnabled) {
      _syncMouseButtons(0);
      return;
    }
    // Mid-drag the pointer may leave the video; clamp so drags keep tracking.
    final local = _videoLocal(event.localPosition, clamp: _mouseButtons != 0);
    if (local != null) _sendAbs(local);
    _syncMouseButtons(event.buttons);
  }

  void _onMouseUp(PointerEvent event) {
    _syncMouseButtons(event.buttons);
  }

  void _onPointerHover(PointerHoverEvent event) {
    if (!_isMouse(event) || !widget.mouseControlEnabled) return;
    final local = _videoLocal(event.localPosition);
    if (local != null) _sendAbs(local);
  }

  /// Accumulate pixel deltas into whole scroll lines (host expects lines).
  void _sendScrollPixels(double dxPixels, double dyPixels) {
    const pixelsPerLine = 40.0;
    _scrollAccumX += dxPixels / pixelsPerLine;
    _scrollAccumY += dyPixels / pixelsPerLine;
    final sx = _scrollAccumX.truncate();
    final sy = _scrollAccumY.truncate();
    if (sx == 0 && sy == 0) return;
    _scrollAccumX -= sx;
    _scrollAccumY -= sy;
    sendMouseScroll(dx: sx.clamp(-32768, 32767), dy: sy.clamp(-32768, 32767));
  }

  void _onPointerSignal(PointerSignalEvent event) {
    if (event is! PointerScrollEvent || !widget.mouseControlEnabled) return;
    // Wheel-down (positive dy) = scroll down = negative wheel lines.
    _sendScrollPixels(-event.scrollDelta.dx, -event.scrollDelta.dy);
  }

  void _onPointerPanZoomUpdate(PointerPanZoomUpdateEvent event) {
    if (!widget.mouseControlEnabled) return;
    // Trackpad two-finger pan (natural scrolling: sign is already inverted
    // relative to wheel deltas).
    _sendScrollPixels(event.panDelta.dx, event.panDelta.dy);
  }

  bool get _canSendMouse =>
      widget.mouseControlEnabled &&
      !_multiFingerContact &&
      _pointers.length <= 1;

  double? _twoFingerSpan() {
    if (_pointers.length < 2) return null;
    final pts = _pointers.values.toList(growable: false);
    return (pts[0] - pts[1]).distance;
  }

  void _trackTwoFingerPinch() {
    final span = _twoFingerSpan();
    final startSpan = _twoFingerStartSpan;
    if (span == null || startSpan == null) return;
    _twoFingerTravel = math.max(_twoFingerTravel, (span - startSpan).abs());
    if (_twoFingerTravel >= _pinchSlop) {
      _beginMultiTouchSession();
    }
  }

  void _beginMultiTouchSession() {
    if (widget.multiTouchSession) {
      _pendingTwoFinger = _pointers.length >= 2;
      return;
    }
    _cancelRelFlush();
    _flushRel();
    _pendingTwoFinger = _pointers.length >= 2;
    _singleOrigin = null;
    _singleDownAt = null;
    _singleTravel = 0;
    _lastMovePos = null;
    _relAccumX = 0;
    _relAccumY = 0;
    _setCapturing(false);
    widget.onMultiTouchSessionChanged(true);
  }

  void _endMultiTouchSession() {
    if (!widget.multiTouchSession) return;
    widget.onMultiTouchSessionChanged(false);
  }

  void _removePointer(int pointer) {
    _activePointers.remove(pointer);
    _pointers.remove(pointer);
  }

  void _onPointerDown(PointerDownEvent event) {
    if (_isMouse(event)) {
      _onMouseDown(event);
      return;
    }
    if (_isInMenuEdgeZone(event.position)) return;

    final local = _videoLocal(event.localPosition);
    if (local == null) return;

    _activePointers.add(event.pointer);
    _pointers[event.pointer] = local;

    if (_pointers.length >= 2) {
      _pendingTwoFinger = true;
      _twoFingerDownAt ??= DateTime.now();
      _twoFingerStartSpan ??= _twoFingerSpan();
      _twoFingerTravel = 0;
      _beginMultiTouchSession();
      return;
    }

    if (!_canSendMouse) return;

    _singleOrigin = local;
    _singleDownAt = DateTime.now();
    _singleTravel = 0;
    _lastMovePos = local;
    _relAccumX = 0;
    _relAccumY = 0;
    _setCapturing(true);
    if (widget.mode == PointerMode.direct) {
      _sendAbs(local);
    }
  }

  void _onPointerMove(PointerMoveEvent event) {
    if (_isMouse(event)) {
      _onMouseMove(event);
      return;
    }
    if (!_activePointers.contains(event.pointer)) return;

    final local = _videoLocal(event.localPosition, clamp: true);
    if (local != null) {
      _pointers[event.pointer] = local;
    }

    if (_multiFingerContact) {
      if (_pointers.length >= 2) {
        _trackTwoFingerPinch();
      }
      return;
    }

    if (!_canSendMouse || _pointers.length != 1 || _singleOrigin == null) {
      return;
    }

    final tracked = _pointers.values.firstOrNull;
    if (tracked == null) return;

    _singleTravel = math.max(
      _singleTravel,
      (tracked - _singleOrigin!).distance,
    );
    if (widget.mode == PointerMode.direct) {
      _sendAbs(tracked);
    } else {
      final last = _lastMovePos;
      if (last != null) {
        _sendRel(tracked - last);
      }
      _lastMovePos = tracked;
    }
  }

  void _onPointerUp(PointerUpEvent event) {
    if (_isMouse(event)) {
      _onMouseUp(event);
      return;
    }
    final countBefore = _pointers.length;
    final activeBefore = _activePointers.length;
    final pendingTwo = _pendingTwoFinger;
    final twoStart = _twoFingerDownAt;
    final twoTravel = _twoFingerTravel;
    final origin = _singleOrigin;
    final downAt = _singleDownAt;
    final travel = _singleTravel;

    _removePointer(event.pointer);
    _flushRel();

    if (_activePointers.isEmpty) {
      if (pendingTwo &&
          activeBefore >= 2 &&
          twoStart != null &&
          DateTime.now().difference(twoStart) < _tapTimeout &&
          twoTravel < _pinchSlop) {
        sendMouseClick(button: 1);
      } else if (activeBefore == 1 &&
          countBefore == 1 &&
          !pendingTwo &&
          !widget.multiTouchSession &&
          origin != null &&
          downAt != null &&
          travel < _tapSlop &&
          DateTime.now().difference(downAt) < _tapTimeout) {
        if (widget.mode == PointerMode.direct) {
          _sendAbs(origin);
        }
        sendMouseClick(button: 0);
      }

      _resetMouseState();
      _endMultiTouchSession();
    } else if (_activePointers.length == 1) {
      // One finger left after a multi-touch gesture — wait for a fresh touch.
      _pendingTwoFinger = false;
      _twoFingerStartSpan = null;
      _twoFingerTravel = 0;
      _twoFingerDownAt = null;
      _singleOrigin = null;
      _singleDownAt = null;
      _singleTravel = 0;
      _lastMovePos = null;
      _relAccumX = 0;
      _relAccumY = 0;
      _setCapturing(false);
    }
  }

  void _onPointerCancel(PointerCancelEvent event) {
    if (_isMouse(event)) {
      _syncMouseButtons(0);
      return;
    }
    _removePointer(event.pointer);
    if (_activePointers.isEmpty) {
      _resetMouseState();
      _endMultiTouchSession();
    }
  }

  @override
  Widget build(BuildContext context) {
    return Positioned.fill(
      child: Listener(
        behavior: HitTestBehavior.translucent,
        onPointerDown: _onPointerDown,
        onPointerMove: _onPointerMove,
        onPointerUp: _onPointerUp,
        onPointerCancel: _onPointerCancel,
        onPointerHover: _onPointerHover,
        onPointerSignal: _onPointerSignal,
        onPointerPanZoomUpdate: _onPointerPanZoomUpdate,
        child: const SizedBox.expand(),
      ),
    );
  }
}

class _LiveDiagnostics {
  const _LiveDiagnostics({
    this.width = 0,
    this.height = 0,
    this.targetFps = 0,
    this.renderFps = 0,
    this.codec = '',
    this.bytesPerSec = 0,
  });

  final int width;
  final int height;
  final int targetFps;
  final double renderFps;
  final String codec;
  final double bytesPerSec;

  bool get hasResolution => width > 0 && height > 0;

  @override
  int get hashCode => Object.hash(
        width,
        height,
        targetFps,
        renderFps.round(),
        bytesPerSec.round(),
        codec,
      );

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is _LiveDiagnostics &&
          width == other.width &&
          height == other.height &&
          targetFps == other.targetFps &&
          (renderFps - other.renderFps).abs() < 0.05 &&
          (bytesPerSec - other.bytesPerSec).abs() < 100 &&
          codec == other.codec;
}

class _StreamMenu extends StatelessWidget {
  const _StreamMenu({
    required this.diagnostics,
    required this.showDiagnostics,
    required this.touchControls,
    this.anchorLeft = false,
    required this.pointerMode,
    required this.onPointerModeChanged,
    required this.upscaleSupported,
    required this.upscaleActive,
    required this.onToggleUpscale,
    required this.onStop,
    required this.onKeyboard,
  });

  final _LiveDiagnostics diagnostics;
  final bool showDiagnostics;
  final bool touchControls;
  /// True when the panel is anchored to the left screen edge.
  final bool anchorLeft;
  final PointerMode pointerMode;
  final ValueChanged<PointerMode> onPointerModeChanged;
  final bool upscaleSupported;
  final bool upscaleActive;
  final VoidCallback? onToggleUpscale;
  final VoidCallback onStop;
  final VoidCallback onKeyboard;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final size = MediaQuery.sizeOf(context);
    final insets = MediaQuery.paddingOf(context);
    final compact = size.height < 640 || size.width < 360;
    final metrics = _MenuMetrics(compact: compact, screenWidth: size.width, insets: insets);
    final trackpad = pointerMode == PointerMode.trackpad;
    final tileDensity =
        compact ? VisualDensity.compact : VisualDensity.standard;
    final tilePadding = EdgeInsets.symmetric(
      horizontal: metrics.horizontal,
      vertical: compact ? 0 : 2,
    );

    const edgeSide = BorderSide(color: Color(0x33000000), width: 1);
    return Material(
      color: theme.colorScheme.surface.withValues(alpha: 0.96),
      elevation: 0,
      shape: anchorLeft
          ? const Border(right: edgeSide)
          : const Border(left: edgeSide),
      child: SafeArea(
        left: anchorLeft,
        right: !anchorLeft,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: EdgeInsets.fromLTRB(
                metrics.horizontal + 4,
                metrics.top,
                metrics.horizontal + 4,
                compact ? 4 : 8,
              ),
              child: Text(
                'StreamBeam',
                style: (compact
                        ? theme.textTheme.titleSmall
                        : theme.textTheme.titleMedium)
                    ?.copyWith(fontWeight: FontWeight.w600),
              ),
            ),
            Expanded(
              child: ListView(
                padding: EdgeInsets.fromLTRB(
                  metrics.horizontal,
                  0,
                  metrics.horizontal,
                  metrics.bottom,
                ),
                children: [
                  if (showDiagnostics) ...[
                    Padding(
                      padding: EdgeInsets.fromLTRB(
                        metrics.horizontal + 4,
                        compact ? 4 : 8,
                        metrics.horizontal + 4,
                        compact ? 2 : 4,
                      ),
                      child: Text(
                        'Connection',
                        style: theme.textTheme.titleSmall,
                      ),
                    ),
                    Padding(
                      padding: EdgeInsets.symmetric(horizontal: metrics.horizontal),
                      child: _DiagnosticsPanel(
                        diagnostics: diagnostics,
                        compact: compact,
                      ),
                    ),
                    const Divider(height: 1),
                  ],
                  if (upscaleSupported) ...[
                    Padding(
                      padding: EdgeInsets.fromLTRB(
                        metrics.horizontal + 4,
                        compact ? 4 : 8,
                        metrics.horizontal + 4,
                        compact ? 2 : 4,
                      ),
                      child: Text(
                        'Video',
                        style: theme.textTheme.titleSmall,
                      ),
                    ),
                    SwitchListTile(
                      value: upscaleActive,
                      onChanged: onToggleUpscale == null
                          ? null
                          : (_) => onToggleUpscale!(),
                      title: const Text('Upscaling'),
                      subtitle: Text(
                        'Sharper image via GPU (MetalFX)',
                        style: theme.textTheme.bodySmall,
                      ),
                      dense: compact,
                      contentPadding: tilePadding,
                    ),
                    const Divider(height: 1),
                  ],
                  if (touchControls) ...[
                    Padding(
                      padding: EdgeInsets.fromLTRB(
                        metrics.horizontal + 4,
                        compact ? 4 : 8,
                        metrics.horizontal + 4,
                        compact ? 2 : 4,
                      ),
                      child: Text(
                        'Pointer mode',
                        style: theme.textTheme.titleSmall,
                      ),
                    ),
                    Padding(
                      padding: EdgeInsets.symmetric(horizontal: metrics.horizontal),
                      child: SegmentedButton<PointerMode>(
                        segments: [
                          ButtonSegment(
                            value: PointerMode.direct,
                            icon: Icon(Icons.touch_app, size: compact ? 18 : 20),
                            label: compact ? null : const Text('Direct'),
                            tooltip: 'Direct touch — finger maps to screen',
                          ),
                          ButtonSegment(
                            value: PointerMode.trackpad,
                            icon: Icon(Icons.swipe, size: compact ? 18 : 20),
                            label: compact ? null : const Text('Trackpad'),
                            tooltip: 'Trackpad — drag moves cursor',
                          ),
                        ],
                        selected: {pointerMode},
                        emptySelectionAllowed: false,
                        showSelectedIcon: false,
                        onSelectionChanged: (selected) {
                          if (selected.isNotEmpty) {
                            onPointerModeChanged(selected.first);
                          }
                        },
                      ),
                    ),
                    Padding(
                      padding: EdgeInsets.fromLTRB(
                        metrics.horizontal + 4,
                        compact ? 2 : 6,
                        metrics.horizontal + 4,
                        compact ? 4 : 8,
                      ),
                      child: Text(
                        trackpad
                            ? 'Drag to move the cursor · tap to left-click'
                            : 'Your finger position maps to the PC screen · tap to click',
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: theme.colorScheme.onSurfaceVariant,
                        ),
                      ),
                    ),
                  ],
                  const Divider(height: 1),
                  ListTile(
                    leading: Icon(
                      Icons.stop_circle_outlined,
                      size: compact ? 22 : 24,
                    ),
                    title: const Text('Stop'),
                    visualDensity: tileDensity,
                    contentPadding: tilePadding,
                    onTap: onStop,
                  ),
                  if (touchControls)
                    ListTile(
                      leading: Icon(
                        Icons.keyboard_outlined,
                        size: compact ? 22 : 24,
                      ),
                      title: const Text('Keyboard'),
                      subtitle: Text(
                        'Send text to your PC',
                        style: theme.textTheme.bodySmall,
                      ),
                      visualDensity: tileDensity,
                      contentPadding: tilePadding,
                      onTap: onKeyboard,
                    ),
                  Padding(
                    padding: EdgeInsets.fromLTRB(
                      metrics.horizontal + 4,
                      compact ? 4 : 8,
                      metrics.horizontal + 4,
                      compact ? 4 : 8,
                    ),
                    child: Text(
                      touchControls
                          ? 'Drag from the right edge for menu\n'
                              '1 finger: move cursor · tap: left click\n'
                              '2 fingers: pinch zoom / pan (local only)\n'
                              'Quick 2-finger tap: right click'
                          : 'Mouse controls the remote device directly:\n'
                              'click · drag · right-click · scroll',
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: theme.colorScheme.onSurfaceVariant,
                        height: compact ? 1.25 : 1.35,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _DiagnosticsPanel extends StatelessWidget {
  const _DiagnosticsPanel({
    required this.diagnostics,
    required this.compact,
  });

  final _LiveDiagnostics diagnostics;
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final labelStyle = theme.textTheme.bodySmall?.copyWith(
      color: theme.colorScheme.onSurfaceVariant,
    );
    final valueStyle = (compact
            ? theme.textTheme.bodySmall
            : theme.textTheme.bodyMedium)
        ?.copyWith(fontFeatures: const [FontFeature.tabularFigures()]);

    final resolution = diagnostics.hasResolution
        ? '${diagnostics.width}×${diagnostics.height}'
        : '—';
    final fps = diagnostics.renderFps > 0.05
        ? diagnostics.targetFps > 0
            ? '${diagnostics.renderFps.toStringAsFixed(0)} fps (${diagnostics.targetFps} target)'
            : '${diagnostics.renderFps.toStringAsFixed(0)} fps'
        : diagnostics.targetFps > 0
            ? '0 fps (${diagnostics.targetFps} target)'
            : '—';
    final rate = _formatDataRate(diagnostics.bytesPerSec);
    final codec = diagnostics.codec.isNotEmpty ? diagnostics.codec : '—';

    return Column(
      children: [
        _DiagRow(label: 'Resolution', value: resolution, labelStyle: labelStyle, valueStyle: valueStyle),
        _DiagRow(label: 'Frame rate', value: fps, labelStyle: labelStyle, valueStyle: valueStyle),
        _DiagRow(label: 'Data rate', value: rate, labelStyle: labelStyle, valueStyle: valueStyle),
        _DiagRow(label: 'Codec', value: codec, labelStyle: labelStyle, valueStyle: valueStyle),
      ],
    );
  }

  static String _formatDataRate(double bytesPerSec) {
    if (bytesPerSec <= 0) return '—';
    if (bytesPerSec < 1024) {
      return '${bytesPerSec.round()} B/s';
    }
    if (bytesPerSec < 1024 * 1024) {
      return '${(bytesPerSec / 1024).toStringAsFixed(1)} KB/s';
    }
    return '${(bytesPerSec / (1024 * 1024)).toStringAsFixed(1)} MB/s';
  }
}

class _DiagRow extends StatelessWidget {
  const _DiagRow({
    required this.label,
    required this.value,
    required this.labelStyle,
    required this.valueStyle,
  });

  final String label;
  final String value;
  final TextStyle? labelStyle;
  final TextStyle? valueStyle;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 3),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Expanded(flex: 2, child: Text(label, style: labelStyle)),
          Expanded(
            flex: 3,
            child: Text(
              value,
              style: valueStyle,
              textAlign: TextAlign.end,
            ),
          ),
        ],
      ),
    );
  }
}

class _MenuMetrics {
  const _MenuMetrics({
    required this.compact,
    required this.screenWidth,
    required this.insets,
  });

  final bool compact;
  final double screenWidth;
  final EdgeInsets insets;

  double get horizontal => compact
      ? 4
      : (screenWidth * 0.025).clamp(6.0, 14.0);

  double get top => math.max(insets.top, compact ? 6 : 12);

  double get bottom => math.max(insets.bottom, compact ? 8 : 16);
}
