import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:media_kit/media_kit.dart';
import 'package:path/path.dart' as p;
import 'package:path_provider/path_provider.dart';

import 'src/rust/api/library.dart' as rust;
import 'src/rust/frb_generated.dart';
import 'state/library_state.dart';
import 'views/preprocess_dashboard.dart';
import 'views/source_setup.dart';
import 'views/workspace.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  MediaKit.ensureInitialized();
  await RustLib.init();
  final support = await getApplicationSupportDirectory();
  await rust.initDb(dbPath: p.join(support.path, 'library.db'));
  runApp(const ProviderScope(child: PhotoReviewApp()));
}

class PhotoReviewApp extends ConsumerWidget {
  const PhotoReviewApp({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final source = ref.watch(selectedSourceProvider);
    final unlocked = ref.watch(reviewUnlockedProvider);
    return MaterialApp(
      title: 'Burst Photo Review',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: Colors.teal,
          brightness: Brightness.dark,
        ),
        useMaterial3: true,
      ),
      home: source == null
          ? const Scaffold(body: SourceSetupView())
          : unlocked
              ? const WorkspaceView()
              : const PreprocessDashboard(),
    );
  }
}
