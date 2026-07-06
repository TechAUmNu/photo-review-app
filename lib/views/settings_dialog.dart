import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../src/rust/api/library.dart' as rust;
import '../state/library_state.dart';

/// Minimal settings: burst gap regroup and ffmpeg path override.
class SettingsDialog extends ConsumerStatefulWidget {
  const SettingsDialog({super.key});

  @override
  ConsumerState<SettingsDialog> createState() => _SettingsDialogState();
}

class _SettingsDialogState extends ConsumerState<SettingsDialog> {
  double _gapMs = 250;
  final _ffmpegController = TextEditingController();
  bool _working = false;
  String? _message;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final gap = await rust.getAppSetting(key: 'gap_ms');
    final ffmpeg = await rust.getAppSetting(key: 'ffmpeg_path');
    if (!mounted) return;
    setState(() {
      _gapMs = double.tryParse(gap ?? '') ?? 250;
      _ffmpegController.text = ffmpeg ?? '';
    });
  }

  @override
  void dispose() {
    _ffmpegController.dispose();
    super.dispose();
  }

  Future<void> _regroup() async {
    final source = ref.read(selectedSourceProvider)!;
    setState(() {
      _working = true;
      _message = null;
    });
    try {
      final n = await rust.regroup(
        sourceId: source.id,
        gapMs: _gapMs.round(),
        minBurstLen: 3,
      );
      ref.read(libraryVersionProvider.notifier).bump();
      setState(() => _message =
          'Regrouped into $n bursts. Re-rendering missing videos…');
      await reprocessMissing(source.id);
      ref.read(libraryVersionProvider.notifier).bump();
      if (mounted) setState(() => _message = 'Regrouped into $n bursts. Done.');
    } catch (e) {
      if (mounted) setState(() => _message = 'Regroup failed: $e');
    } finally {
      if (mounted) setState(() => _working = false);
    }
  }

  Future<void> _saveFfmpeg() async {
    await rust.setAppSetting(
        key: 'ffmpeg_path', value: _ffmpegController.text.trim());
    if (mounted) setState(() => _message = 'ffmpeg path saved');
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Settings'),
      content: SizedBox(
        width: 460,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('Burst gap: ${_gapMs.round()} ms',
                style: Theme.of(context).textTheme.titleSmall),
            const Text(
              'Frames closer together than this belong to the same burst. '
              'Regrouping keeps manually split/merged bursts intact but '
              'rebuilds everything else (decisions on rebuilt bursts reset).',
              style: TextStyle(fontSize: 12),
            ),
            Slider(
              value: _gapMs,
              min: 20,
              max: 2000,
              divisions: 99,
              label: '${_gapMs.round()} ms',
              onChanged:
                  _working ? null : (v) => setState(() => _gapMs = v),
            ),
            Align(
              alignment: Alignment.centerRight,
              child: OutlinedButton(
                onPressed: _working ? null : _regroup,
                child: const Text('Regroup now'),
              ),
            ),
            const Divider(height: 32),
            Text('ffmpeg path (blank = bundled/system)',
                style: Theme.of(context).textTheme.titleSmall),
            Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _ffmpegController,
                    decoration:
                        const InputDecoration(hintText: '/opt/homebrew/bin/ffmpeg'),
                  ),
                ),
                TextButton(onPressed: _saveFfmpeg, child: const Text('Save')),
              ],
            ),
            if (_message != null)
              Padding(
                padding: const EdgeInsets.only(top: 12),
                child: Text(_message!),
              ),
            if (_working)
              const Padding(
                padding: EdgeInsets.only(top: 12),
                child: LinearProgressIndicator(),
              ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: const Text('Close'),
        ),
      ],
    );
  }
}
