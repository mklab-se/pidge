//! Output formatting utilities.

pub mod hyperlink;
pub mod linkify;

pub use linkify::linkify_text;

use std::sync::atomic::{AtomicBool, Ordering};

static NO_COLOR: AtomicBool = AtomicBool::new(false);

/// Disable all decorative escape sequences (ANSI colors and OSC 8 hyperlinks).
/// Called from `main` when `--no-color` is passed.
pub fn set_no_color(value: bool) {
    NO_COLOR.store(value, Ordering::Relaxed);
}

/// True when the user has asked for plain output (no ANSI colors, no OSC 8 links).
pub fn no_color() -> bool {
    NO_COLOR.load(Ordering::Relaxed)
}
