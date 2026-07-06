import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../src/rust/api/library.dart' as rust;
import '../state/library_state.dart';

/// Runs the upfront preprocessing pass (all stills + all burst MP4s) and
/// gates entry into review until the cache is complete. Set it going and
/// walk away — it is resumable at any point.
class PreprocessDashboard extends ConsumerStatefulWidget {
  const PreprocessDashboard({super.key});

  @override
  ConsumerState<PreprocessDashboard> createState() =>
      _PreprocessDashboardState();
}

class _PreprocessDashboardState extends ConsumerState<PreprocessDashboard> {
  rust.PreprocessProgress? _progress;
  bool _running = false;
  bool _reindexing = false;
  String? _reindexStatus;
  String? _error;
  DateTime? _phaseStart;
  String? _phase;

  /// Move the cache to a user-chosen folder (e.g. the fast external SSD).
  /// Existing cached files are keyed by content hash, so a fresh location
  /// simply re-renders; pointing back later reuses the old files.
  Future<void> _pickCacheFolder(rust.SourceInfo source) async {
    final path = await getDirectoryPath();
    if (path == null) return;
    await rust.setCacheFolder(sourceId: source.id, path: path);
    final refreshed = await rust.selectSource(rootPath: source.rootPath);
    ref.read(selectedSourceProvider.notifier).set(refreshed);
    ref.read(libraryVersionProvider.notifier).bump();
  }

  Future<void> _reindex(rust.SourceInfo source) async {
    setState(() {
      _reindexing = true;
      _reindexStatus = 'Indexing…';
      _error = null;
    });
    try {
      await for (final p in rust.startIndex(sourceId: source.id)) {
        if (!mounted) return;
        setState(() => _reindexStatus = p.finished
            ? 'Indexed ${p.photos} photos → ${p.bursts} bursts, '
                '${p.singles} singles'
            : '${p.phase}: ${p.done}/${p.total}');
        if (p.finished) break;
      }
      ref.read(libraryVersionProvider.notifier).bump();
    } catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _reindexing = false);
    }
  }

  Future<void> _start(rust.SourceInfo source) async {
    setState(() {
      _running = true;
      _error = null;
      _progress = null;
    });
    try {
      await for (final p in rust.startPreprocess(sourceId: source.id)) {
        if (!mounted) return;
        if (p.phase != _phase) {
          _phase = p.phase;
          _phaseStart = DateTime.now();
        }
        setState(() => _progress = p);
        if (p.finished) break;
      }
      ref.read(libraryVersionProvider.notifier).bump();
    } catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _running = false);
    }
  }

  String? _eta(rust.PreprocessProgress p) {
    final start = _phaseStart;
    if (start == null || p.done == BigInt.zero || p.finished) return null;
    final elapsed = DateTime.now().difference(start).inSeconds;
    if (elapsed < 5) return null;
    final rate = p.done.toDouble() / elapsed;
    final remaining = (p.total - p.done).toDouble() / rate;
    final d = Duration(seconds: remaining.round());
    if (d.inHours > 0) return '${d.inHours}h ${d.inMinutes % 60}m left';
    if (d.inMinutes > 0) return '${d.inMinutes}m ${d.inSeconds % 60}s left';
    return '${d.inSeconds}s left';
  }

  @override
  Widget build(BuildContext context) {
    final source = ref.watch(selectedSourceProvider)!;
    final status = ref.watch(cacheStatusProvider).value;
    final p = _progress;
    final complete = status != null &&
        status.stillsTotal > BigInt.zero &&
        status.stillsCached == status.stillsTotal &&
        status.videosCached == status.videosTotal;

    return Scaffold(
      appBar: AppBar(
        title: const Text('Preprocess'),
        leading: IconButton(
          icon: const Icon(Icons.arrow_back),
          onPressed: () =>
              ref.read(selectedSourceProvider.notifier).set(null),
        ),
      ),
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 640),
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                'Prepare previews and burst videos',
                style: Theme.of(context).textTheme.titleLarge,
              ),
              const SizedBox(height: 8),
              Text(
                'All processing happens now so review has zero delays. '
                'This can run unattended (hours for a large card) and can be '
                'safely interrupted and resumed.',
                style: Theme.of(context).textTheme.bodyMedium,
              ),
              const SizedBox(height: 16),
              ListTile(
                contentPadding: EdgeInsets.zero,
                leading: const Icon(Icons.storage),
                title: const Text('Cache location (put this on a fast SSD)'),
                subtitle: Text(source.cachePath ?? 'not set'),
                trailing: OutlinedButton(
                  onPressed:
                      _running || _reindexing ? null : () => _pickCacheFolder(source),
                  child: const Text('Change…'),
                ),
              ),
              Row(
                children: [
                  TextButton.icon(
                    onPressed:
                        _running || _reindexing ? null : () => _reindex(source),
                    icon: const Icon(Icons.refresh, size: 18),
                    label: const Text('Re-index source'),
                  ),
                  if (_reindexStatus != null)
                    Expanded(
                      child: Text(_reindexStatus!,
                          style: Theme.of(context).textTheme.bodySmall,
                          overflow: TextOverflow.ellipsis),
                    ),
                ],
              ),
              const SizedBox(height: 16),
              if (status != null) ...[
                _statusRow(context, 'Stills', status.stillsCached,
                    status.stillsTotal),
                const SizedBox(height: 8),
                _statusRow(context, 'Burst videos', status.videosCached,
                    status.videosTotal),
                const SizedBox(height: 24),
              ],
              if (_running && p != null) ...[
                LinearProgressIndicator(
                  value: p.total > BigInt.zero
                      ? p.done.toDouble() / p.total.toDouble()
                      : null,
                ),
                const SizedBox(height: 8),
                Text(
                  '${p.phase}: ${p.done}/${p.total}'
                  '${p.failed > BigInt.zero ? ' (${p.failed} failed)' : ''}'
                  '${_eta(p) != null ? ' · ${_eta(p)}' : ''}',
                ),
                const SizedBox(height: 16),
              ],
              if (p != null && p.finished) ...[
                Text(
                  'Done: ${p.stillsProcessed} stills processed '
                  '(${p.stillsSkipped} cached), ${p.videosRendered} videos '
                  'rendered (${p.videosSkipped} cached)'
                  '${p.failures.isNotEmpty ? ' · ${p.failures.length} failures' : ''}',
                ),
                if (p.failures.isNotEmpty)
                  Padding(
                    padding: const EdgeInsets.only(top: 8),
                    child: Text(
                      p.failures.take(5).join('\n'),
                      style: Theme.of(context)
                          .textTheme
                          .bodySmall
                          ?.copyWith(color: Colors.orange),
                    ),
                  ),
                const SizedBox(height: 16),
              ],
              if (_error != null)
                Padding(
                  padding: const EdgeInsets.only(bottom: 16),
                  child: Text(_error!,
                      style: TextStyle(
                          color: Theme.of(context).colorScheme.error)),
                ),
              Row(
                mainAxisAlignment: MainAxisAlignment.center,
                children: [
                  if (!_running)
                    FilledButton.icon(
                      onPressed: _reindexing ? null : () => _start(source),
                      icon: const Icon(Icons.play_arrow),
                      label: Text(complete
                          ? 'Re-check / process new files'
                          : 'Start preprocessing'),
                    )
                  else
                    OutlinedButton.icon(
                      onPressed: () => rust.cancelPreprocess(),
                      icon: const Icon(Icons.stop),
                      label: const Text('Cancel (resumable)'),
                    ),
                  const SizedBox(width: 12),
                  FilledButton.tonalIcon(
                    onPressed: complete || !_running
                        ? () =>
                            ref.read(reviewUnlockedProvider.notifier).unlock()
                        : null,
                    icon: const Icon(Icons.grid_view),
                    label: Text(
                        complete ? 'Start review' : 'Review anyway (partial)'),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _statusRow(
      BuildContext context, String label, BigInt cached, BigInt total) {
    final done = total > BigInt.zero && cached == total;
    return Row(
      children: [
        Icon(
          done ? Icons.check_circle : Icons.pending_outlined,
          size: 18,
          color: done ? Colors.green : null,
        ),
        const SizedBox(width: 8),
        Text('$label: $cached / $total cached'),
      ],
    );
  }
}
