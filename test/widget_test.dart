import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:photo_review_app/main.dart';

void main() {
  testWidgets('app shows source setup when no source selected',
      (tester) async {
    // PhotoReviewApp reads only providers here; the Rust bridge is not
    // exercised until a source is picked, so this runs without RustLib.
    await tester.pumpWidget(const ProviderScope(child: PhotoReviewApp()));
    expect(find.text('Burst Photo Review'), findsOneWidget);
    expect(find.textContaining('Select photo folder'), findsOneWidget);
  });
}
