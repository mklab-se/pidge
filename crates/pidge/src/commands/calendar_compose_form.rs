//! TUI wizard for `pidge calendar new` (placeholder).
//!
//! The mail compose form is implemented in `compose_form.rs`; the calendar
//! analogue follows the same pattern but with calendar-specific fields. For
//! now we expose a stub so the `--confirm` path can compile; the real wizard
//! lands in a follow-up that mirrors `compose_form::run_form`.

#![allow(dead_code)]

use anyhow::Result;

use crate::cli::CalendarNewArgs;

/// Open a TUI prefilled with `initial` and return the user's filled-in args,
/// or `None` if they cancel. Today this is a no-op stub.
pub fn run_form(_initial: CalendarNewArgs) -> Result<Option<CalendarNewArgs>> {
    anyhow::bail!(
        "The calendar wizard isn't wired yet — pass --title, --start, and other flags directly."
    )
}
