import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../src/rust/api/library.dart' as rust;
import '../state/library_state.dart';

/// Export keepers (originals, pairs together) and keep-video MP4s to the
/// output folder. Idempotent — safe to re-run any time.
class ExportScreen extends ConsumerStatefulWidget {
  const ExportScreen({super.key});

  @override
  ConsumerState<ExportScreen> createState() => _ExportScreenState();
}

class _ExportScreenState extends ConsumerState<ExportScreen> {
  rust.ExportPlan? _plan;
  rust.ExportProgress? _progress;
  bool _running = false;
  String? _error;
  String? _outputPath;

  @override
  void initState() {
    super.initState();
    _refreshPlan();
  }

  Future<void> _refreshPlan() async {
    final source = ref.read(selectedSourceProvider)!;
    try {
      final plan = await rust.planExport(sourceId: source.id);
      if (mounted) {
        setState(() {
          _plan = plan;
          _outputPath = plan.outputPath;
          _error = null;
        });
      }
    } catch (e) {
      if (mounted) setState(() => _error = '$e');
    }
  }

  Future<void> _pickOutput() async {
    final source = ref.read(selectedSourceProvider)!;
    final path = await getDirectoryPath();
    if (path == null) return;
    await rust.setOutputFolder(sourceId: source.id, path: path);
    setState(() => _outputPath = path);
  }

  Future<void> _start() async {
    final source = ref.read(selectedSourceProvider)!;
    setState(() {
      _running = true;
      _error = null;
      _progress = null;
    });
    try {
      await for (final p in rust.startExport(sourceId: source.id)) {
        if (!mounted) return;
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

  String _bytes(BigInt b) {
    final gb = b.toDouble() / (1024 * 1024 * 1024);
    if (gb >= 1) return '${gb.toStringAsFixed(1)} GB';
    return '${(b.toDouble() / (1024 * 1024)).toStringAsFixed(0)} MB';
  }

  @override
  Widget build(BuildContext context) {
    final plan = _plan;
    final p = _progress;

    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 640),
        child: ListView(
          shrinkWrap: true,
          padding: const EdgeInsets.all(24),
          children: [
            Text('Export', style: Theme.of(context).textTheme.titleLarge),
            const SizedBox(height: 8),
            const Text(
              'Copies keeper originals (RAW+JPEG pairs stay together) into '
              'keepers/ and flagged burst videos into videos/. Originals are '
              'never touched. Re-running skips anything already exported.',
            ),
            const SizedBox(height: 16),
            ListTile(
              contentPadding: EdgeInsets.zero,
              leading: const Icon(Icons.drive_file_move),
              title: Text(_outputPath ?? 'No output folder selected'),
              trailing: OutlinedButton(
                onPressed: _running ? null : _pickOutput,
                child: const Text('Choose…'),
              ),
            ),
            const Divider(),
            if (plan != null) ...[
              Text(
                '${plan.keeperPhotos} keeper photos '
                '(${plan.keeperFiles} files, ${_bytes(plan.keeperBytes)})'
                ' · ${plan.videos} videos (${_bytes(plan.videoBytes)})',
              ),
              const SizedBox(height: 16),
            ],
            if (_running && p != null) ...[
              LinearProgressIndicator(
                value: p.total > BigInt.zero
                    ? p.done.toDouble() / p.total.toDouble()
                    : null,
              ),
              const SizedBox(height: 8),
              Text('${p.phase}: ${p.done}/${p.total} — ${p.current}'),
              const SizedBox(height: 16),
            ],
            if (p != null && p.finished) ...[
              Text(
                'Done: ${p.filesCopied} files copied '
                '(${p.filesSkipped} already there'
                '${p.filesRenamed > BigInt.zero ? ', ${p.filesRenamed} renamed' : ''}), '
                '${p.videosCopied} videos copied '
                '(${p.videosSkipped} already there)'
                '${p.failures.isNotEmpty ? ' · ${p.failures.length} FAILURES' : ''}',
              ),
              if (p.failures.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Text(
                    p.failures.take(8).join('\n'),
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
                padding: const EdgeInsets.only(bottom: 12),
                child: Text(_error!,
                    style:
                        TextStyle(color: Theme.of(context).colorScheme.error)),
              ),
            Row(
              children: [
                if (!_running)
                  FilledButton.icon(
                    onPressed: _outputPath == null ? null : _start,
                    icon: const Icon(Icons.upload),
                    label: const Text('Export'),
                  )
                else
                  OutlinedButton.icon(
                    onPressed: () => rust.cancelExport(),
                    icon: const Icon(Icons.stop),
                    label: const Text('Cancel (resumable)'),
                  ),
                const SizedBox(width: 12),
                TextButton(
                  onPressed: _running ? null : _refreshPlan,
                  child: const Text('Refresh plan'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
