//! `pidge account default ...` — show or set default accounts.

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use pidge_core::Config;

/// Set the default account used for e-mail. `auth default --send` in v0.2.
pub fn set_email(email: &str) -> Result<()> {
    let mut config = Config::load()?;
    config.set_default_send(email)?;
    config.save()?;
    println!("{} Default e-mail account → {email}", "✔".green());
    Ok(())
}

/// Set the default account used for calendar events and invitations.
pub fn set_calendar(email: &str) -> Result<()> {
    let mut config = Config::load()?;
    config.set_default_calendar(email)?;
    config.save()?;
    println!("{} Default calendar account → {email}", "✔".green());
    Ok(())
}

/// Print both current defaults. Used when `pidge account default` is called
/// without a subcommand.
pub fn print_current(json: bool) -> Result<()> {
    let config = Config::load()?;
    if json {
        #[derive(Serialize)]
        struct DefaultsOut<'a> {
            email: Option<&'a str>,
            calendar: Option<&'a str>,
        }
        let out = DefaultsOut {
            email: config.defaults.send.as_deref(),
            calendar: config.defaults.calendar.as_deref(),
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "e-mail:   {}",
            config.defaults.send.as_deref().unwrap_or("(none)")
        );
        println!(
            "calendar: {}",
            config.defaults.calendar.as_deref().unwrap_or("(none)")
        );
    }
    Ok(())
}
