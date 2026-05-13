//! `pidge auth default` — show or set default accounts.

use anyhow::Result;
use colored::Colorize;

use pidge_core::Config;

pub fn run(send: Option<String>, calendar: Option<String>) -> Result<()> {
    let mut config = Config::load()?;
    let mut changed = false;

    if let Some(email) = send {
        config.set_default_send(&email)?;
        println!("{} Default send account → {email}", "✔".green());
        changed = true;
    }
    if let Some(email) = calendar {
        config.set_default_calendar(&email)?;
        println!("{} Default calendar account → {email}", "✔".green());
        changed = true;
    }

    if changed {
        config.save()?;
        return Ok(());
    }

    // No flags → show current
    println!(
        "send:     {}",
        config.defaults.send.as_deref().unwrap_or("(none)")
    );
    println!(
        "calendar: {}",
        config.defaults.calendar.as_deref().unwrap_or("(none)")
    );
    Ok(())
}
