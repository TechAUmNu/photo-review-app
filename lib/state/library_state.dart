import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../src/rust/api/library.dart' as rust;

/// The source (memory card folder) currently being worked on.
final selectedSourceProvider =
    NotifierProvider<SelectedSourceNotifier, rust.SourceInfo?>(
        SelectedSourceNotifier.new);

class SelectedSourceNotifier extends Notifier<rust.SourceInfo?> {
  @override
  rust.SourceInfo? build() => null;

  void set(rust.SourceInfo? source) => state = source;
}

/// Bumped after indexing or decision changes to refresh queries.
final libraryVersionProvider =
    NotifierProvider<LibraryVersionNotifier, int>(LibraryVersionNotifier.new);

class LibraryVersionNotifier extends Notifier<int> {
  @override
  int build() => 0;

  void bump() => state++;
}

/// True once the user has entered review (preprocess complete or overridden).
final reviewUnlockedProvider =
    NotifierProvider<ReviewUnlockedNotifier, bool>(ReviewUnlockedNotifier.new);

class ReviewUnlockedNotifier extends Notifier<bool> {
  @override
  bool build() {
    // Reset whenever the selected source changes.
    ref.watch(selectedSourceProvider);
    return false;
  }

  void unlock() => state = true;
}

final sourcesProvider = FutureProvider<List<rust.SourceInfo>>((ref) async {
  return rust.listSources();
});

final cacheStatusProvider = FutureProvider<rust.CacheStatus?>((ref) async {
  final source = ref.watch(selectedSourceProvider);
  ref.watch(libraryVersionProvider);
  if (source == null) return null;
  return rust.getCacheStatus(sourceId: source.id);
});

final burstsProvider = FutureProvider<List<rust.BurstSummary>>((ref) async {
  final source = ref.watch(selectedSourceProvider);
  ref.watch(libraryVersionProvider);
  if (source == null) return [];
  return rust.listBursts(sourceId: source.id, offset: 0, limit: 10000);
});

final singlesProvider = FutureProvider<List<rust.PhotoSummary>>((ref) async {
  final source = ref.watch(selectedSourceProvider);
  ref.watch(libraryVersionProvider);
  if (source == null) return [];
  return rust.listSingles(sourceId: source.id, offset: 0, limit: 100000);
});

/// Re-run preprocessing quietly (e.g. after split/merge invalidated MP4s).
/// Cached outputs are skipped, so this only renders what's missing.
Future<void> reprocessMissing(int sourceId) async {
  await for (final p in rust.startPreprocess(sourceId: sourceId)) {
    if (p.finished) break;
  }
}

final statsProvider = FutureProvider<rust.ProgressStats?>((ref) async {
  final source = ref.watch(selectedSourceProvider);
  ref.watch(libraryVersionProvider);
  if (source == null) return null;
  return rust.getProgressStats(sourceId: source.id);
});
