# Vendored crates

## i-slint-backend-android-activity 1.18.1

Slint's Android backend from crates.io (GPL-3.0-only OR the Slint licenses,
see its LICENSES), with one change marked `DUORAY` in `lib.rs` and `vsync.rs`:
a pending redraw waits for the next display vsync (the Choreographer thread the
backend already runs for animations) instead of rendering at once.

Rendered at once on every touch event, a scroll queued two or three frames
ahead of the display: SurfaceFlinger showed each frame 40–44 ms after it was
posted at 60 Hz, and the list trailed the finger.

When Slint is updated, copy the new release of the crate here and apply the
change again, or drop the `[patch.crates-io]` entry in `Cargo.toml` if
upstream renders on the vsync by itself.
