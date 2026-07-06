import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:path/path.dart' as p;
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show Int64List;
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:media_kit/media_kit.dart';
import 'package:media_kit_video/media_kit_video.dart';

import '../src/rust/api/library.dart' as rust;
import '../state/library_state.dart';
import '../widgets/photo_thumb.dart';

/// Full-screen burst review player.
///
/// Keyboard-first: Space play/pause · ←/→ frame step (Shift ±10) ·
/// Home/End · 1–4 speed · L loop · K/Enter keep frame · V keep video ·
/// X reject · D done · J/↓ next burst · U/↑ previous burst · Esc close.
///
/// Playback uses the pre-rendered cache MP4 (hardware decoded). When paused
/// the 2048px cached still of the current frame is overlaid — that still is
/// authoritative for keep-marking, so marking can never hit the wrong frame.
class BurstPlayerView extends ConsumerStatefulWidget {
  const BurstPlayerView({
    super.key,
    required this.bursts,
    required this.initialIndex,
  });

  final List<rust.BurstSummary> bursts;
  final int initialIndex;

  @override
  ConsumerState<BurstPlayerView> createState() => _BurstPlayerViewState();
}

class _BurstPlayerViewState extends ConsumerState<BurstPlayerView> {
  late final Player _player;
  late final VideoController _controller;
  late int _burstIndex;
  List<rust.PhotoSummary> _frames = [];
  final FocusNode _focus = FocusNode();

  bool _playing = false;
  bool _loop = true;
  double _rate = 0.25;
  int _frameIndex = 0;
  Duration _duration = Duration.zero;
  bool _zoom = false;
  bool _busy = false;
  final TransformationController _zoomController = TransformationController();

  // The summaries passed in are immutable; track decision changes locally.
  final Map<int, bool> _keepVideoOverride = {};
  final Map<int, String> _statusOverride = {};

  rust.BurstSummary get _burst => widget.bursts[_burstIndex];
  bool get _keepVideo => _keepVideoOverride[_burstIndex] ?? _burst.keepVideo;
  String get _status => _statusOverride[_burstIndex] ?? _burst.status;

  /// Must match the fps used at render time (preprocess/mod.rs).
  double get _fps => (_burst.fpsEstimate ?? 30.0).clamp(1.0, 240.0);

  @override
  void initState() {
    super.initState();
    _burstIndex = widget.initialIndex;
    _player = Player();
    _controller = VideoController(_player);
    _player.setPlaylistMode(PlaylistMode.single);
    _player.stream.position.listen((pos) {
      if (!mounted) return;
      // Only follow the decoder while actually playing. When paused,
      // _frameIndex is the source of truth — position events from
      // still-in-flight seeks would otherwise drag the display backwards
      // during rapid frame stepping.
      if (!_playing) return;
      final idx = (pos.inMilliseconds / 1000.0 * _fps).round();
      final clamped = _frames.isEmpty
          ? 0
          : idx.clamp(0, _frames.length - 1);
      if (clamped != _frameIndex) setState(() => _frameIndex = clamped);
    });
    _player.stream.playing.listen((playing) {
      if (mounted) setState(() => _playing = playing);
    });
    _player.stream.duration.listen((d) {
      if (mounted) setState(() => _duration = d);
    });
    _player.stream.completed.listen((completed) {
      if (completed && _loop && mounted) {
        _player.seek(Duration.zero);
        _player.play();
      }
    });
    _openBurst(_burstIndex, autoplay: true);
  }

  @override
  void dispose() {
    _player.dispose();
    _focus.dispose();
    _zoomController.dispose();
    super.dispose();
  }

  Future<void> _openBurst(int index, {bool autoplay = true}) async {
    final source = ref.read(selectedSourceProvider)!;
    _burstIndex = index;
    _frameIndex = 0;
    final frames = await rust.getBurstFrames(
        sourceId: source.id, burstId: _burst.id);
    if (!mounted) return;
    setState(() => _frames = frames);

    final videoPath = _burst.videoCachePath;
    if (videoPath != null && File(videoPath).existsSync()) {
      await _player.open(Media(videoPath), play: autoplay);
      await _player.setRate(_rate);
    } else {
      await _player.stop();
    }
  }

  // ---------- actions ----------

  Duration _frameDuration(int index) =>
      Duration(milliseconds: (index / _fps * 1000).round());

  Future<void> _togglePlay() async {
    if (_playing) {
      await _player.pause();
      // Land exactly on the displayed frame.
      await _player.seek(_frameDuration(_frameIndex));
    } else {
      // Resume from wherever stepping/scrubbing left the still.
      await _player.seek(_frameDuration(_frameIndex));
      await _player.play();
    }
  }

  /// Paused-mode navigation: update the authoritative index immediately
  /// (the still overlay repaints synchronously), pre-decode neighbours,
  /// and let the hidden video catch up in the background — never awaited,
  /// so held-down keys can't queue up a backlog of stale seeks.
  void _seekToFrame(int index) {
    if (_frames.isEmpty) return;
    if (_playing) _player.pause();
    final clamped = index.clamp(0, _frames.length - 1);
    setState(() => _frameIndex = clamped);
    _precacheAround(clamped);
    _player.seek(_frameDuration(clamped)); // fire-and-forget
  }

  void _step(int delta) => _seekToFrame(_frameIndex + delta);

  /// Decode the next few stills into Flutter's image cache so rapid
  /// stepping never flashes a half-loaded frame.
  void _precacheAround(int index) {
    for (final i in [index + 1, index + 2, index - 1, index + 3]) {
      if (i < 0 || i >= _frames.length) continue;
      final path = _frames[i].previewPath;
      if (path != null) {
        precacheImage(FileImage(File(path)), context);
      }
    }
  }

  Future<void> _setRate(double rate) async {
    setState(() => _rate = rate);
    await _player.setRate(rate);
  }

  Future<void> _toggleKeep() async {
    if (_frames.isEmpty) return;
    final frame = _frames[_frameIndex];
    await rust.setFrameKeep(
        photoIds: Int64List.fromList([frame.id]), keep: !frame.keep);
    final updated = await rust.getBurstFrames(
        sourceId: ref.read(selectedSourceProvider)!.id, burstId: _burst.id);
    if (mounted) setState(() => _frames = updated);
    ref.read(libraryVersionProvider.notifier).bump();
  }

  Future<void> _toggleKeepVideo() async {
    final next = !_keepVideo;
    await rust.setKeepVideo(burstId: _burst.id, keep: next);
    setState(() => _keepVideoOverride[_burstIndex] = next);
    ref.read(libraryVersionProvider.notifier).bump();
  }

  Future<void> _decide(String status) async {
    await rust.setBurstStatus(burstId: _burst.id, status: status);
    _statusOverride[_burstIndex] = status;
    ref.read(libraryVersionProvider.notifier).bump();
    await _advance(1, closeAtEnd: true);
  }

  Future<void> _advance(int delta, {bool closeAtEnd = false}) async {
    final next = _burstIndex + delta;
    if (next < 0) return;
    if (next >= widget.bursts.length) {
      if (closeAtEnd && mounted) Navigator.of(context).pop();
      return;
    }
    setState(() => _zoom = false);
    await _openBurst(next);
  }

  /// Split at the playhead: current frame becomes the first frame of a new
  /// burst. Videos re-render in the background; the list view refreshes.
  Future<void> _splitAtPlayhead() async {
    if (_busy || _frameIndex <= 0 || _frames.isEmpty) return;
    final source = ref.read(selectedSourceProvider)!;
    setState(() => _busy = true);
    try {
      await _player.pause();
      await rust.splitBurst(
          burstId: _burst.id, atFrameIndex: _frameIndex);
      ref.read(libraryVersionProvider.notifier).bump();
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(const SnackBar(
            content: Text('Burst split — re-rendering videos…')));
        Navigator.of(context).pop(); // indices changed; back to the list
      }
      await reprocessMissing(source.id);
      ref.read(libraryVersionProvider.notifier).bump();
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text('Split failed: $e')));
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// Merge this burst with the next one in the list.
  Future<void> _mergeWithNext() async {
    if (_busy || _burstIndex + 1 >= widget.bursts.length) return;
    final source = ref.read(selectedSourceProvider)!;
    setState(() => _busy = true);
    try {
      await _player.pause();
      await rust.mergeBursts(
          burstIds: Int64List.fromList(
              [_burst.id, widget.bursts[_burstIndex + 1].id]));
      ref.read(libraryVersionProvider.notifier).bump();
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(const SnackBar(
            content: Text('Bursts merged — re-rendering video…')));
        Navigator.of(context).pop();
      }
      await reprocessMissing(source.id);
      ref.read(libraryVersionProvider.notifier).bump();
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text('Merge failed: $e')));
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  void _toggleZoom() {
    if (_playing) return; // zoom applies to the paused still
    setState(() {
      _zoom = !_zoom;
      if (!_zoom) _zoomController.value = Matrix4.identity();
    });
  }

  /// Show the cached MP4 in the system file manager, selected.
  Future<void> _revealVideo() async {
    final path = _burst.videoCachePath;
    if (path == null) return;
    if (Platform.isMacOS) {
      await Process.run('open', ['-R', path]);
    } else if (Platform.isWindows) {
      await Process.run('explorer', ['/select,$path']);
    } else {
      await Process.run('xdg-open', [File(path).parent.path]);
    }
  }

  // ---------- keyboard ----------

  KeyEventResult _onKey(FocusNode node, KeyEvent event) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) {
      return KeyEventResult.ignored;
    }
    final shift = HardwareKeyboard.instance.isShiftPressed;
    switch (event.logicalKey) {
      case LogicalKeyboardKey.space:
        _togglePlay();
      case LogicalKeyboardKey.arrowLeft:
        _step(shift ? -10 : -1);
      case LogicalKeyboardKey.arrowRight:
        _step(shift ? 10 : 1);
      case LogicalKeyboardKey.home:
        _seekToFrame(0);
      case LogicalKeyboardKey.end:
        _seekToFrame(_frames.length - 1);
      case LogicalKeyboardKey.digit1:
        _setRate(0.125);
      case LogicalKeyboardKey.digit2:
        _setRate(0.25);
      case LogicalKeyboardKey.digit3:
        _setRate(0.5);
      case LogicalKeyboardKey.digit4:
        _setRate(1.0);
      case LogicalKeyboardKey.keyL:
        setState(() => _loop = !_loop);
      case LogicalKeyboardKey.keyK || LogicalKeyboardKey.enter:
        _toggleKeep();
      case LogicalKeyboardKey.keyV:
        _toggleKeepVideo();
      case LogicalKeyboardKey.keyX:
        _decide('rejected');
      case LogicalKeyboardKey.keyD:
        _decide('done');
      case LogicalKeyboardKey.keyJ || LogicalKeyboardKey.arrowDown:
        _advance(1);
      case LogicalKeyboardKey.keyU || LogicalKeyboardKey.arrowUp:
        _advance(-1);
      case LogicalKeyboardKey.keyS:
        _splitAtPlayhead();
      case LogicalKeyboardKey.keyM:
        _mergeWithNext();
      case LogicalKeyboardKey.keyF:
        _toggleZoom();
      case LogicalKeyboardKey.escape:
        Navigator.of(context).pop();
      default:
        return KeyEventResult.ignored;
    }
    return KeyEventResult.handled;
  }

  // ---------- UI ----------

  @override
  Widget build(BuildContext context) {
    final currentFrame =
        _frames.isEmpty ? null : _frames[_frameIndex.clamp(0, _frames.length - 1)];
    final stillPath = currentFrame?.previewPath;

    return Focus(
      focusNode: _focus,
      autofocus: true,
      onKeyEvent: _onKey,
      child: Scaffold(
        backgroundColor: Colors.black,
        body: Column(
          children: [
            _header(context),
            Expanded(
              child: Stack(
                fit: StackFit.expand,
                children: [
                  Video(
                    controller: _controller,
                    controls: NoVideoControls,
                    fill: Colors.black,
                  ),
                  // Paused: overlay the exact cached still (full quality).
                  // gaplessPlayback keeps the previous frame on screen while
                  // the next decodes, so rapid stepping never flickers.
                  if (!_playing && stillPath != null)
                    _zoom
                        ? InteractiveViewer(
                            transformationController: _zoomController,
                            maxScale: 8,
                            child: Image.file(File(stillPath),
                                gaplessPlayback: true, fit: BoxFit.contain),
                          )
                        : Image.file(File(stillPath),
                            gaplessPlayback: true, fit: BoxFit.contain),
                  if (_busy)
                    const Center(child: CircularProgressIndicator()),
                  if (_burst.videoCachePath == null)
                    const Center(
                      child: Text(
                        'No cached video for this burst — run preprocessing',
                        style: TextStyle(color: Colors.white70),
                      ),
                    ),
                ],
              ),
            ),
            _transport(context),
            _filmstrip(context),
          ],
        ),
      ),
    );
  }

  Widget _header(BuildContext context) {
    final b = _burst;
    final durationS = (b.endMs - b.startMs) / 1000.0;
    return Container(
      color: Colors.grey.shade900,
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      child: Row(
        children: [
          IconButton(
            icon: const Icon(Icons.close),
            onPressed: () => Navigator.of(context).pop(),
            tooltip: 'Close (Esc)',
          ),
          Text(
            'Burst ${_burstIndex + 1}/${widget.bursts.length}'
            ' · ${b.frameCount} frames · ${durationS.toStringAsFixed(2)}s'
            '${b.fpsEstimate != null ? ' · ~${b.fpsEstimate!.round()} fps' : ''}',
            style: const TextStyle(color: Colors.white70),
          ),
          const Spacer(),
          IconButton(
            icon: const Icon(Icons.folder_open, size: 20),
            tooltip: b.videoCachePath == null
                ? 'No cached video'
                : 'Reveal video in ${Platform.isMacOS ? 'Finder' : 'Explorer'}\n'
                    '${p.basename(b.videoCachePath!)}',
            onPressed: b.videoCachePath == null ? null : _revealVideo,
          ),
          const SizedBox(width: 4),
          _statusChip(_status),
          const SizedBox(width: 8),
          if (_keepVideo)
            const Chip(
              label: Text('VIDEO'),
              visualDensity: VisualDensity.compact,
            ),
        ],
      ),
    );
  }

  Widget _statusChip(String status) {
    final color = switch (status) {
      'done' => Colors.green,
      'rejected' => Colors.red,
      _ => Colors.grey,
    };
    return Chip(
      label: Text(status),
      backgroundColor: color.withValues(alpha: 0.25),
      visualDensity: VisualDensity.compact,
    );
  }

  Widget _transport(BuildContext context) {
    final positionMs = (_frameIndex / _fps * 1000).round();
    return Container(
      color: Colors.grey.shade900,
      padding: const EdgeInsets.symmetric(horizontal: 12),
      child: Row(
        children: [
          IconButton(
            icon: const Icon(Icons.skip_previous),
            onPressed: _burstIndex > 0 ? () => _advance(-1) : null,
            tooltip: 'Previous burst (U or ↑)',
          ),
          IconButton(
            icon: Icon(_playing ? Icons.pause : Icons.play_arrow),
            onPressed: _togglePlay,
            tooltip: 'Play/pause (Space)',
          ),
          IconButton(
            icon: const Icon(Icons.skip_next),
            onPressed: _burstIndex < widget.bursts.length - 1
                ? () => _advance(1)
                : null,
            tooltip: 'Next burst (J or ↓)',
          ),
          Text(
            'f${_frameIndex + 1}/${_frames.length}',
            style: const TextStyle(
                color: Colors.white70, fontFeatures: [FontFeature.tabularFigures()]),
          ),
          Expanded(
            child: Slider(
              value: _frames.isEmpty
                  ? 0
                  : _frameIndex.toDouble().clamp(0, (_frames.length - 1).toDouble()),
              max: _frames.isEmpty ? 1 : (_frames.length - 1).toDouble(),
              onChanged: (v) => _seekToFrame(v.round()),
            ),
          ),
          Text('${(positionMs / 1000).toStringAsFixed(2)}s'
              ' / ${(_duration.inMilliseconds / 1000).toStringAsFixed(2)}s',
              style: const TextStyle(color: Colors.white54, fontSize: 12)),
          const SizedBox(width: 12),
          SegmentedButton<double>(
            segments: const [
              ButtonSegment(value: 0.125, label: Text('⅛×')),
              ButtonSegment(value: 0.25, label: Text('¼×')),
              ButtonSegment(value: 0.5, label: Text('½×')),
              ButtonSegment(value: 1.0, label: Text('1×')),
            ],
            selected: {_rate},
            onSelectionChanged: (s) => _setRate(s.first),
            showSelectedIcon: false,
            style: const ButtonStyle(
                visualDensity: VisualDensity.compact,
                tapTargetSize: MaterialTapTargetSize.shrinkWrap),
          ),
          IconButton(
            icon: Icon(Icons.loop,
                color: _loop ? Colors.tealAccent : Colors.white38),
            onPressed: () => setState(() => _loop = !_loop),
            tooltip: 'Loop (L)',
          ),
          IconButton(
            icon: Icon(
              _keepVideo ? Icons.videocam : Icons.videocam_outlined,
              color: _keepVideo ? Colors.lightBlueAccent : null,
            ),
            onPressed: _toggleKeepVideo,
            tooltip: 'Keep burst as video (V)',
          ),
          IconButton(
            icon: const Icon(Icons.delete_outline, color: Colors.redAccent),
            onPressed: () => _decide('rejected'),
            tooltip: 'Reject burst (X)',
          ),
          IconButton(
            icon: const Icon(Icons.check, color: Colors.greenAccent),
            onPressed: () => _decide('done'),
            tooltip: 'Mark done, next burst (D)',
          ),
        ],
      ),
    );
  }

  Widget _filmstrip(BuildContext context) {
    return SizedBox(
      height: 76,
      child: ListView.builder(
        scrollDirection: Axis.horizontal,
        itemCount: _frames.length,
        itemBuilder: (context, i) {
          final frame = _frames[i];
          final selected = i == _frameIndex;
          return GestureDetector(
            onTap: () => _seekToFrame(i),
            child: Container(
              width: 96,
              margin: const EdgeInsets.all(2),
              decoration: BoxDecoration(
                border: Border.all(
                  color: selected
                      ? Colors.tealAccent
                      : frame.keep
                          ? Colors.amber
                          : Colors.transparent,
                  width: 2,
                ),
              ),
              child: Stack(
                fit: StackFit.expand,
                children: [
                  PhotoThumb(
                    path: frame.thumbPath ?? frame.displayPath,
                    kind: frame.thumbPath != null ? 'jpeg' : frame.displayKind,
                    cacheWidth: 96,
                  ),
                  if (frame.keep)
                    const Positioned(
                      right: 2,
                      top: 2,
                      child:
                          Icon(Icons.star, color: Colors.amber, size: 14),
                    ),
                ],
              ),
            ),
          );
        },
      ),
    );
  }
}
