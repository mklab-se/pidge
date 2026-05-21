//! `pidge contacts find <query>` — look up known contacts locally.

use anyhow::Result;
use colored::Colorize;
use pidge_core::{Contact, ContactsCache};

use crate::commands::name_resolve;

pub async fn run(query: String, limit: usize, json: bool) -> Result<()> {
    let cache = ContactsCache::load()?;

    let mut matches: Vec<Contact> = if query.trim().is_empty() {
        cache.by_email.values().cloned().collect()
    } else {
        let q = query.trim().to_lowercase();
        cache
            .by_email
            .values()
            .filter(|c| name_resolve::contact_matches_public(c, &q))
            .cloned()
            .collect()
    };
    matches.sort_by_key(|c| std::cmp::Reverse(c.last_seen));
    matches.truncate(limit);

    if json {
        println!("{}", serde_json::to_string_pretty(&matches)?);
        return Ok(());
    }
    if matches.is_empty() {
        if cache.by_email.is_empty() {
            println!(
                "{}",
                "No contacts in the local index — run `pidge contacts refresh` first.".dimmed()
            );
        } else {
            println!("{}", "No matching contacts.".dimmed());
        }
        return Ok(());
    }
    for c in &matches {
        let name = if c.display_name.is_empty() {
            "(no name)".dimmed().to_string()
        } else {
            c.display_name.bold().to_string()
        };
        let last_seen = c.last_seen.format("%Y-%m-%d").to_string();
        println!(
            "{}  {}  {}  (mail {}, cal {})",
            last_seen.dimmed(),
            name,
            c.email,
            c.seen_in_mail,
            c.seen_in_calendar
        );
    }
    Ok(())
}
