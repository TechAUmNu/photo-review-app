import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/library_state.dart';
import 'burst_list.dart';
import 'export_screen.dart';
import 'settings_dialog.dart';
import 'singles_grid.dart';

/// Main review workspace: Bursts | Singles tabs + progress footer.
class WorkspaceView extends ConsumerWidget {
  const WorkspaceView({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final source = ref.watch(selectedSourceProvider)!;
    final stats = ref.watch(statsProvider).value;

    return DefaultTabController(
      length: 3,
      child: Scaffold(
        appBar: AppBar(
          title: Text(source.rootPath,
              style: Theme.of(context).textTheme.titleSmall),
          leading: IconButton(
            icon: const Icon(Icons.arrow_back),
            tooltip: 'Change source',
            onPressed: () =>
                ref.read(selectedSourceProvider.notifier).set(null),
          ),
          actions: [
            IconButton(
              icon: const Icon(Icons.settings),
              tooltip: 'Settings',
              onPressed: () => showDialog(
                context: context,
                builder: (context) => const SettingsDialog(),
              ),
            ),
          ],
          bottom: const TabBar(tabs: [
            Tab(text: 'Bursts'),
            Tab(text: 'Singles'),
            Tab(text: 'Export'),
          ]),
        ),
        body: const TabBarView(
          children: [BurstListView(), SinglesGridView(), ExportScreen()],
        ),
        bottomNavigationBar: stats == null
            ? null
            : Container(
                padding:
                    const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
                color: Theme.of(context).colorScheme.surfaceContainerHigh,
                child: Text(
                  'Bursts decided: ${stats.decidedBursts}/${stats.totalBursts}'
                  ' · Singles: ${stats.totalSingles}'
                  ' · Keepers: ${stats.keptPhotos} photos,'
                  ' ${stats.keptVideos} videos',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ),
      ),
    );
  }
}
