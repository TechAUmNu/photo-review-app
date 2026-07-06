import 'dart:io';

import 'package:flutter/material.dart';

/// Displays an original camera file as a thumbnail. Temporary M1 helper:
/// once the preview cache exists (M2) this will read cached JPEGs instead.
/// RAW files (and HEIF, which Flutter can't decode) show a placeholder.
class PhotoThumb extends StatelessWidget {
  const PhotoThumb({
    super.key,
    required this.path,
    required this.kind,
    this.cacheWidth = 320,
  });

  final String? path;
  final String? kind;
  final int cacheWidth;

  @override
  Widget build(BuildContext context) {
    if (path == null || kind == 'raw') {
      return _placeholder(context, Icons.raw_on);
    }
    return Image.file(
      File(path!),
      fit: BoxFit.cover,
      cacheWidth: cacheWidth,
      errorBuilder: (context, error, stackTrace) =>
          _placeholder(context, Icons.broken_image_outlined),
    );
  }

  Widget _placeholder(BuildContext context, IconData icon) {
    return Container(
      color: Theme.of(context).colorScheme.surfaceContainerHighest,
      alignment: Alignment.center,
      child: Icon(icon, size: 32),
    );
  }
}
