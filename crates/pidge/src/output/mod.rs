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

/// Install a global `inquire` render config that styles every prompt label
/// (`To:`, `Subject:`, ...) in bold cyan so users can tell prompts apart
/// from informational output at a glance. The default config leaves the
/// prompt text white-on-default, which can blend with surrounding lines.
///
/// Called once from `main`; honors `--no-color` by leaving the (uncolored)
/// default render config in place.
pub fn install_inquire_theme() {
    if no_color() {
        return;
    }
    use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};
    let label_style = StyleSheet::new()
        .with_fg(Color::LightCyan)
        .with_attr(Attributes::BOLD);
    let mut config = RenderConfig::default()
        .with_prompt_prefix(Styled::new("?").with_fg(Color::LightYellow))
        .with_answered_prompt_prefix(Styled::new("✔").with_fg(Color::LightGreen))
        .with_help_message(StyleSheet::new().with_fg(Color::DarkGrey))
        .with_default_value(
            StyleSheet::new()
                .with_fg(Color::DarkGrey)
                .with_attr(Attributes::ITALIC),
        )
        .with_canceled_prompt_indicator(Styled::new("<canceled>").with_fg(Color::LightRed));
    // `prompt` and `editor_prompt` are public fields on RenderConfig that
    // style the label text for `Text`/`Confirm`/`Select` and `Editor`
    // respectively. They have no builder method so we set them directly.
    config.prompt = label_style;
    config.editor_prompt = label_style;
    inquire::set_global_render_config(config);
}
