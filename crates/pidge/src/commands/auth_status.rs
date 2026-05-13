//! `pidge auth status` — summary of accounts and defaults.

use anyhow::Result;
use colored::Colorize;

use pidge_core::Config;

pub fn run() -> Result<()> {
    let config = Config::load()?;

    let n = config.accounts.len();
    println!("{} account{} signed in.", n, if n == 1 { "" } else { "s" });

    if n == 0 {
        println!();
        println!("Run {} to add one.", "`pidge auth login`".cyan());
        return Ok(());
    }

    println!();
    println!("{}", "Defaults:".bold());
    println!(
        "  send:     {}",
        config.defaults.send.as_deref().unwrap_or("(none)")
    );
    println!(
        "  calendar: {}",
        config.defaults.calendar.as_deref().unwrap_or("(none)")
    );

    Ok(())
}
