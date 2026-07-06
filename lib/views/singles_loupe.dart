import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show Int64List;

import '../src/rust/api/library.dart' as rust;
import '../state/library_state.dart';
import '../widgets/photo_thumb.dart';

/// Full-screen loupe for single (non-burst) photos.
/// ←/→ navigate · K/Enter toggle keep · Esc close.
class SinglesLoupeView extends ConsumerStatefulWidget {
  const SinglesLoupeView({
    super.key,
    required this.photos,
    required this.initialIndex,
  });

  final List<rust.PhotoSummary> photos;
  final int initialIndex;

  @override
  ConsumerState<SinglesLoupeView> createState() => _SinglesLoupeViewState();
}

class _SinglesLoupeViewState extends ConsumerState<SinglesLoupeView> {
  late int _index;
  final Map<int, bool> _keepOverride = {};
  final FocusNode _focus = FocusNode();

  rust.PhotoSummary get _photo => widget.photos[_index];
  bool get _keep => _keepOverride[_index] ?? _photo.keep;

  @override
  void initState() {
    super.initState();
    _index = widget.initialIndex;
  }

  @override
  void dispose() {
    _focus.dispose();
    super.dispose();
  }

  Future<void> _toggleKeep() async {
    final next = !_keep;
    await rust.setFrameKeep(
        photoIds: Int64List.fromList([_photo.id]), keep: next);
    setState(() => _keepOverride[_index] = next);
    ref.read(libraryVersionProvider.notifier).bump();
  }

  KeyEventResult _onKey(FocusNode node, KeyEvent event) {
    if (event is! KeyDownEvent && event is! KeyRepeatEvent) {
      return KeyEventResult.ignored;
    }
    switch (event.logicalKey) {
      case LogicalKeyboardKey.arrowLeft:
        if (_index > 0) setState(() => _index--);
      case LogicalKeyboardKey.arrowRight:
        if (_index < widget.photos.length - 1) setState(() => _index++);
      case LogicalKeyboardKey.keyK || LogicalKeyboardKey.enter:
        _toggleKeep();
      case LogicalKeyboardKey.escape:
        Navigator.of(context).pop();
      default:
        return KeyEventResult.ignored;
    }
    return KeyEventResult.handled;
  }

  @override
  Widget build(BuildContext context) {
    final photo = _photo;
    final display = photo.previewPath ?? photo.displayPath;
    final displayable =
        photo.previewPath != null || photo.displayKind == 'jpeg';

    return Focus(
      focusNode: _focus,
      autofocus: true,
      onKeyEvent: _onKey,
      child: Scaffold(
        backgroundColor: Colors.black,
        body: Column(
          children: [
            Container(
              color: Colors.grey.shade900,
              padding:
                  const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
              child: Row(
                children: [
                  IconButton(
                    icon: const Icon(Icons.close),
                    onPressed: () => Navigator.of(context).pop(),
                    tooltip: 'Close (Esc)',
                  ),
                  Text('${_index + 1}/${widget.photos.length}',
                      style: const TextStyle(color: Colors.white70)),
                  const Spacer(),
                  IconButton(
                    icon: Icon(
                      _keep ? Icons.star : Icons.star_border,
                      color: _keep ? Colors.amber : null,
                    ),
                    onPressed: _toggleKeep,
                    tooltip: 'Keep (K)',
                  ),
                ],
              ),
            ),
            Expanded(
              child: displayable
                  ? Image.file(File(display), fit: BoxFit.contain)
                  : const PhotoThumb(path: null, kind: 'raw'),
            ),
          ],
        ),
      ),
    );
  }
}
