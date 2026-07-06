import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/library_state.dart';
import '../widgets/photo_thumb.dart';
import 'singles_loupe.dart';

class SinglesGridView extends ConsumerWidget {
  const SinglesGridView({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final singles = ref.watch(singlesProvider);
    return singles.when(
      data: (list) => list.isEmpty
          ? const Center(child: Text('No single photos'))
          : GridView.builder(
              padding: const EdgeInsets.all(12),
              gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
                maxCrossAxisExtent: 200,
                mainAxisSpacing: 8,
                crossAxisSpacing: 8,
              ),
              itemCount: list.length,
              itemBuilder: (context, i) {
                final photo = list[i];
                return GestureDetector(
                  onTap: () => Navigator.of(context).push(
                    MaterialPageRoute(
                      builder: (context) =>
                          SinglesLoupeView(photos: list, initialIndex: i),
                    ),
                  ),
                  child: Stack(
                    fit: StackFit.expand,
                    children: [
                      PhotoThumb(
                        path: photo.thumbPath ?? photo.displayPath,
                        kind: photo.thumbPath != null
                            ? 'jpeg'
                            : photo.displayKind,
                      ),
                      if (photo.keep)
                        const Positioned(
                          right: 4,
                          top: 4,
                          child: Icon(Icons.star,
                              color: Colors.amber, size: 20),
                        ),
                    ],
                  ),
                );
              },
            ),
      error: (e, _) => Center(child: Text('$e')),
      loading: () => const Center(child: CircularProgressIndicator()),
    );
  }
}
