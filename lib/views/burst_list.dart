import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../src/rust/api/library.dart' as rust;
import '../state/library_state.dart';
import '../widgets/photo_thumb.dart';
import 'burst_player.dart';

class BurstListView extends ConsumerWidget {
  const BurstListView({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final bursts = ref.watch(burstsProvider);
    return bursts.when(
      data: (list) => list.isEmpty
          ? const Center(child: Text('No bursts found'))
          : GridView.builder(
              padding: const EdgeInsets.all(12),
              gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
                maxCrossAxisExtent: 280,
                mainAxisSpacing: 12,
                crossAxisSpacing: 12,
                childAspectRatio: 3 / 2.4,
              ),
              itemCount: list.length,
              itemBuilder: (context, i) =>
                  _BurstCard(bursts: list, index: i),
            ),
      error: (e, _) => Center(child: Text('$e')),
      loading: () => const Center(child: CircularProgressIndicator()),
    );
  }
}

class _BurstCard extends ConsumerWidget {
  const _BurstCard({required this.bursts, required this.index});

  final List<rust.BurstSummary> bursts;
  final int index;

  rust.BurstSummary get burst => bursts[index];

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final durationS = (burst.endMs - burst.startMs) / 1000.0;
    final fps = burst.fpsEstimate;
    final rejected = burst.status == 'rejected';

    return Card(
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onTap: () => Navigator.of(context).push(
          MaterialPageRoute(
            builder: (context) =>
                BurstPlayerView(bursts: bursts, initialIndex: index),
          ),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Expanded(
              child: Opacity(
                opacity: rejected ? 0.35 : 1,
                child: Stack(
                  fit: StackFit.expand,
                  children: [
                    PhotoThumb(
                      path: burst.heroThumbPath ?? burst.heroDisplayPath,
                      kind: burst.heroThumbPath != null
                          ? 'jpeg'
                          : burst.heroDisplayKind,
                    ),
                    Positioned(
                      right: 6,
                      top: 6,
                      child: _badge(
                        context,
                        '${burst.frameCount}f',
                      ),
                    ),
                    if (burst.keptCount > 0)
                      Positioned(
                        left: 6,
                        top: 6,
                        child: _badge(context, '★ ${burst.keptCount}',
                            color: Colors.amber.shade700),
                      ),
                    if (burst.keepVideo)
                      Positioned(
                        left: 6,
                        bottom: 6,
                        child: _badge(context, 'VIDEO',
                            color: Colors.blue.shade700),
                      ),
                  ],
                ),
              ),
            ),
            Padding(
              padding: const EdgeInsets.all(8),
              child: Text(
                '${durationS.toStringAsFixed(1)}s'
                '${fps != null ? ' · ~${fps.round()} fps' : ''}'
                ' · ${burst.status}',
                style: Theme.of(context).textTheme.bodySmall,
                overflow: TextOverflow.ellipsis,
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _badge(BuildContext context, String text, {Color? color}) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      decoration: BoxDecoration(
        color: color ?? Colors.black54,
        borderRadius: BorderRadius.circular(4),
      ),
      child: Text(text,
          style: const TextStyle(color: Colors.white, fontSize: 11)),
    );
  }

}
