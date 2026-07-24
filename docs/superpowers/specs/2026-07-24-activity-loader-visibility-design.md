# Higher-Contrast Activity Loader Design

## Goal

Make the animated dot-grid loading indicator clearly visible against Threadlane's dark surfaces without changing its size, speed, path, or timing.

## Design

Update the shared `ActivityLoader` shader in `crates/threadlane/src/components/activity_loader.rs` so every use receives the same treatment:

- Replace the muted gray-blue colors with a bright cyan, blue, and violet palette.
- Keep all dot states at full opacity; animation remains communicated by color movement rather than fading.
- Retain edge antialiasing through the existing geometric coverage value.
- Preserve the existing grid geometry, dimensions, speed defaults, and per-instance overrides.

Because `ActivityLoader` is shared, the chat working indicator, session-row indicator, activity status indicator, and updater indicator will remain visually consistent.

## Validation

- Run `cargo check -p threadlane` to validate the Makepad script and Rust integration.
- Run `git diff --check` to validate patch formatting.
- Inspect the diff to confirm only palette and opacity behavior changed.
- Runtime visual verification is still required to assess the final contrast and animation appearance.
