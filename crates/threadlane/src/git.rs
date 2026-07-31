//! Re-exports from `threadlane_git` and UI integration helpers.

pub use threadlane_git::*;

pub fn open_browser_url(cx: &mut makepad_widgets::Cx, url: &str) {
    use makepad_widgets::{CxOsApi, OpenUrlInPlace};
    cx.open_url(url, OpenUrlInPlace::No);
}
