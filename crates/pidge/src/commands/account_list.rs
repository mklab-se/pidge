//! `pidge account list` — display signed-in accounts with default markers
//! and storage-backend info (replaces the old `auth status` command).

use anyhow::Result;
use chrono::Utc;
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use serde::Serialize;

use pidge_core::{Config, TokenStorage};

pub fn run(json: bool) -> Result<()> {
    let config = Config::load()?;

    if json {
        return emit_json(&config);
    }

    if config.accounts.is_empty() {
        println!(
            "No accounts signed in. Run {} to add one.",
            "`pidge account add`".cyan()
        );
        return Ok(());
    }

    let mut table = Table::new();
    table
        .set_header(vec!["ACCOUNT", "TENANT", "STORAGE", "ADDED", ""])
        .set_content_arrangement(ContentArrangement::Dynamic);

    let now = Utc::now();
    for account in &config.accounts {
        let mut markers = Vec::new();
        if config.defaults.send.as_deref() == Some(account.email.as_str()) {
            markers.push("[e-mail]".yellow().to_string());
        }
        if config.defaults.calendar.as_deref() == Some(account.email.as_str()) {
            markers.push("[calendar]".yellow().to_string());
        }
        table.add_row(vec![
            account.email.clone(),
            account.tenant_label(),
            storage_label(account.storage).to_string(),
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

fn storage_label(s: TokenStorage) -> &'static str {
    match s {
        TokenStorage::Keychain => "keychain",
        TokenStorage::File => "file",
    }
}

#[derive(Serialize)]
struct AccountOut {
    email: String,
    tenant_id: String,
    home_account_id: String,
    storage: TokenStorage,
    added_at: chrono::DateTime<Utc>,
    is_default_email: bool,
    is_default_calendar: bool,
}

fn emit_json(config: &Config) -> Result<()> {
    let out: Vec<AccountOut> = config
        .accounts
        .iter()
        .map(|a| AccountOut {
            email: a.email.clone(),
            tenant_id: a.tenant_id.clone(),
            home_account_id: a.home_account_id.clone(),
            storage: a.storage,
            added_at: a.added_at,
            is_default_email: config.defaults.send.as_deref() == Some(a.email.as_str()),
            is_default_calendar: config.defaults.calendar.as_deref() == Some(a.email.as_str()),
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&out)?);
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
