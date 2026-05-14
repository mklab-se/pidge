//! `pidge auth status` — summary of accounts and defaults.

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use pidge_core::Config;

pub fn run(json: bool) -> Result<()> {
    let config = Config::load()?;

    if json {
        return emit_json(&config);
    }

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

#[derive(Serialize)]
struct StatusOut {
    accounts: usize,
    defaults: DefaultsOut,
}

#[derive(Serialize)]
struct DefaultsOut {
    send: Option<String>,
    calendar: Option<String>,
}

fn emit_json(config: &Config) -> Result<()> {
    let out = StatusOut {
        accounts: config.accounts.len(),
        defaults: DefaultsOut {
            send: config.defaults.send.clone(),
            calendar: config.defaults.calendar.clone(),
        },
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
