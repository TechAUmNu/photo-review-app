import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';

import '../src/rust/api/library.dart' as rust;
import '../state/library_state.dart';

/// Pick the input folder (memory card) and run indexing.
class SourceSetupView extends ConsumerStatefulWidget {
  const SourceSetupView({super.key});

  @override
  ConsumerState<SourceSetupView> createState() => _SourceSetupViewState();
}

class _SourceSetupViewState extends ConsumerState<SourceSetupView> {
  rust.IndexProgress? _progress;
  bool _indexing = false;
  String? _error;

  /// Cache defaults into app support; users with a fast external SSD can
  /// point it elsewhere later (settings, M5).
  Future<rust.SourceInfo> _ensureCacheFolder(rust.SourceInfo source) async {
    if (source.cachePath != null) return source;
    final support = await getApplicationSupportDirectory();
    final cache = p.join(support.path, 'cache', 'src_${source.id}');
    await rust.setCacheFolder(sourceId: source.id, path: cache);
    return rust.selectSource(rootPath: source.rootPath); // refreshed row
  }

  /// Index fully (progress shown here), THEN switch to the next screen.
  /// Setting selectedSource earlier would unmount this view mid-stream and
  /// leave the preprocess screen showing stale zero counts.
  Future<void> _pickAndIndex() async {
    final path = await getDirectoryPath();
    if (path == null) return;
    try {
      var source = await rust.selectSource(rootPath: path);
      source = await _ensureCacheFolder(source);
      final ok = await _runIndex(source);
      if (ok && mounted) {
        ref.read(libraryVersionProvider.notifier).bump();
        ref.read(selectedSourceProvider.notifier).set(source);
      }
    } catch (e) {
      setState(() => _error = '$e');
    }
  }

  Future<void> _reopen(rust.SourceInfo source) async {
    source = await _ensureCacheFolder(source);
    // Already indexed before: enter directly (re-index available inside).
    if (source.lastIndexedAt != null) {
      ref.read(libraryVersionProvider.notifier).bump();
      ref.read(selectedSourceProvider.notifier).set(source);
      return;
    }
    final ok = await _runIndex(source);
    if (ok && mounted) {
      ref.read(libraryVersionProvider.notifier).bump();
      ref.read(selectedSourceProvider.notifier).set(source);
    }
  }

  Future<bool> _runIndex(rust.SourceInfo source) async {
    setState(() {
      _indexing = true;
      _error = null;
      _progress = null;
    });
    var finished = false;
    try {
      await for (final p in rust.startIndex(sourceId: source.id)) {
        if (!mounted) return false;
        setState(() => _progress = p);
        if (p.finished) {
          finished = true;
          break;
        }
      }
    } catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _indexing = false);
    }
    return finished;
  }

  @override
  Widget build(BuildContext context) {
    final sources = ref.watch(sourcesProvider);
    final p = _progress;

    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 560),
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('Burst Photo Review',
                style: Theme.of(context).textTheme.headlineMedium,
                textAlign: TextAlign.center),
            const SizedBox(height: 24),
            FilledButton.icon(
              onPressed: _indexing ? null : _pickAndIndex,
              icon: const Icon(Icons.sd_card),
              label: const Text('Select photo folder (memory card)…'),
            ),
            const SizedBox(height: 16),
            if (_indexing || p != null) ...[
              LinearProgressIndicator(
                value: (p != null && p.total > BigInt.zero && !p.finished)
                    ? p.done.toDouble() / p.total.toDouble()
                    : (p?.finished == true ? 1.0 : null),
              ),
              const SizedBox(height: 8),
              Text(
                p == null
                    ? 'Starting…'
                    : p.finished
                        ? 'Indexed ${p.photos} photos → ${p.bursts} bursts, '
                            '${p.singles} singles'
                        : '${p.phase}: ${p.done}/${p.total}',
                textAlign: TextAlign.center,
              ),
            ],
            if (_error != null)
              Padding(
                padding: const EdgeInsets.only(top: 8),
                child: Text(_error!,
                    style: TextStyle(
                        color: Theme.of(context).colorScheme.error)),
              ),
            const SizedBox(height: 32),
            sources.when(
              data: (list) => list.isEmpty
                  ? const SizedBox.shrink()
                  : Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        const Text('Recent sources'),
                        const SizedBox(height: 8),
                        for (final s in list)
                          ListTile(
                            leading: const Icon(Icons.folder),
                            title: Text(s.rootPath),
                            dense: true,
                            onTap: _indexing ? null : () => _reopen(s),
                          ),
                      ],
                    ),
              error: (e, _) => Text('$e'),
              loading: () => const SizedBox.shrink(),
            ),
          ],
        ),
      ),
    );
  }
}
