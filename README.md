# photo-review-app

Desktop app (macOS) to cull photos from high-rate burst cameras like the
Sony A9III. Groups burst frames into single reviewable items played back
like video, so 40,000 frames become a few hundred decisions.

Built with Flutter (UI) + Rust (indexing/processing core) via
flutter_rust_bridge. Playback uses media_kit (libmpv).

## Workflow

1. **Select source** — point at the memory card (or any folder). Files are
   indexed: EXIF capture times (ms precision) group frames into bursts;
   RAW(.ARW)+JPEG/HEIF pairs are treated as one photo. Originals are never
   modified.
2. **Preprocess** — one up-front pass (set it going, walk away) renders
   every burst to an MP4 and extracts 2048px/320px stills into a cache
   folder, plus a sharpness score per frame. Resumable at any point;
   review only unlocks when it's done, so culling never waits on anything.
3. **Review** —
   - **Bursts** tab: each burst is one card; open it to play the burst as
     a video. Mark keeper frames, keep-as-video, reject or done.
   - **Singles** tab: non-burst photos in a grid with a full-screen loupe.
4. **Export** — copies keeper originals (pairs together) to
   `output/keepers/` and flagged burst MP4s to `output/videos/`.
   Idempotent: re-running skips anything already exported.

## Keyboard (burst player)

| Key | Action |
| --- | --- |
| Space | Play / pause |
| ← / → | Frame step (Shift = ±10) |
| Home / End | First / last frame |
| 1–4 | Speed ⅛× / ¼× / ½× / 1× |
| L | Toggle loop |
| K / Enter | Toggle keep on current frame |
| V | Toggle keep-burst-as-video |
| X | Reject burst (advances) |
| D | Mark burst done (advances) |
| J / ↓ | Next burst |
| U / ↑ | Previous burst |
| S | Split burst at playhead |
| M | Merge with next burst |
| F | Toggle zoom on paused frame |
| Esc | Close player |

When paused, the display switches to the full-quality cached still of the
exact frame, so keep decisions and zoom are always frame-accurate.

## Development

Prereqs: Flutter, Rust, `cargo install flutter_rust_bridge_codegen`,
ffmpeg (`brew install ffmpeg` or set a path in Settings).

```sh
flutter run -d macos              # run the app
cd rust && cargo test             # core tests
flutter test integration_test/simple_test.dart -d macos  # end-to-end
flutter_rust_bridge_codegen generate   # after changing rust/src/api/
```

Layout: `rust/src/` — indexer (walk/EXIF/pair/group), preprocess
(stills/ARW/HEIF/video), export, burst_ops (split/merge/regroup), SQLite
(single source of truth for decisions, keyed by content hash so they
survive card re-mounts). `lib/` — Flutter views/state.
