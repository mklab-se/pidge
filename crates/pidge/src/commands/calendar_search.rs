//! `pidge calendar search <query>` — KQL-style search across calendars.

use anyhow::Result;
use colored::Colorize;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::{CachedEventRef, Config, Event, EventCache};
// note: `Config` is loaded only for the account list; `from_env()` reads creds

use crate::commands::calendar_list::events_to_json;
use crate::commands::time::format_when;
use crate::output::resolve_tz;

pub async fn run(
    query: String,
    accounts_filter: Vec<String>,
    _calendar: Option<String>,
    limit: usize,
    json: bool,
) -> Result<()> {
    let config = Config::load()?;
    let accounts: Vec<_> = if accounts_filter.is_empty() {
        config.accounts.clone()
    } else {
        config
            .accounts
            .iter()
            .filter(|a| accounts_filter.iter().any(|f| f == &a.email))
            .cloned()
            .collect()
    };
    if accounts.is_empty() {
        anyhow::bail!("No signed-in accounts.");
    }
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let mut all: Vec<Event> = Vec::new();
    for a in &accounts {
        let events = graph.search_events(&a.email, &query, limit).await?;
        all.extend(events);
    }
    let mut cache = EventCache::load()?;
    let refs: Vec<CachedEventRef> = all
        .iter()
        .map(|e| CachedEventRef::new(e.id.clone(), e.calendar_id.clone(), e.account.clone()))
        .collect();
    cache.insert_many(&refs);
    cache.save()?;

    if json {
        println!("{}", events_to_json(&all)?);
        return Ok(());
    }
    let tz = resolve_tz(None);
    if all.is_empty() {
        println!("{}", "No matching events.".dimmed());
        return Ok(());
    }
    for e in &all {
        let id = pidge_core::short_hash(&format!("{}|{}", e.account, e.id));
        println!(
            "{}  {}  {}",
            id.dimmed(),
            format_when(e.start.at, &tz).cyan(),
            e.subject
        );
    }
    Ok(())
}
