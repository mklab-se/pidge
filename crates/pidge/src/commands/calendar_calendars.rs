//! `pidge calendar calendars` — list calendars across signed-in accounts.

use anyhow::Result;
use colored::Colorize;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::{Calendar, Config};

use crate::cli::CalendarsCommands;

pub async fn run(_cmd: CalendarsCommands, json: bool) -> Result<()> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        anyhow::bail!("No signed-in accounts. Run `pidge account add` first.");
    }
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let mut all: Vec<Calendar> = Vec::new();
    for a in &config.accounts {
        let cals = graph.list_calendars(&a.email).await?;
        all.extend(cals);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&all)?);
        return Ok(());
    }
    for c in &all {
        let star = if c.is_default { "*" } else { " " };
        let lock = if !c.can_edit {
            " (read-only)".dimmed().to_string()
        } else {
            String::new()
        };
        println!("{star} {}  {}{lock}", c.account.dimmed(), c.name);
    }
    Ok(())
}
