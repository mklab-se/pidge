//! `pidge auth list` — display signed-in accounts.

use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};

use pidge_core::Config;

pub fn run() -> Result<()> {
    let config = Config::load()?;

    if config.accounts.is_empty() {
        println!(
            "No accounts signed in. Run {} to add one.",
            "`pidge auth login`".cyan()
        );
        return Ok(());
    }

    let mut table = Table::new();
    table
        .set_header(vec!["ACCOUNT", "TENANT", "ADDED", ""])
        .set_content_arrangement(ContentArrangement::Dynamic);

    let now = Utc::now();
    for account in &config.accounts {
        let mut markers = Vec::new();
        if config.defaults.send.as_deref() == Some(account.email.as_str()) {
            markers.push("[send]".yellow().to_string());
        }
        if config.defaults.calendar.as_deref() == Some(account.email.as_str()) {
            markers.push("[calendar]".yellow().to_string());
        }
        table.add_row(vec![
            account.email.clone(),
            account.tenant_label(),
            relative_time(now, account.added_at),
            markers.join(" "),
        ]);
    }

    println!("{table}");
    println!();
    println!(
        "{} account{} signed in.",
        config.accounts.len(),
        if config.accounts.len() == 1 { "" } else { "s" }
    );
    Ok(())
}

fn relative_time(now: chrono::DateTime<Utc>, then: chrono::DateTime<Utc>) -> String {
    let delta = now - then;
    if delta.num_days() >= 1 {
        format!("{}d ago", delta.num_days())
    } else if delta.num_hours() >= 1 {
        format!("{}h ago", delta.num_hours())
    } else if delta.num_minutes() >= 1 {
        format!("{}m ago", delta.num_minutes())
    } else {
        "just now".to_string()
    }
}
