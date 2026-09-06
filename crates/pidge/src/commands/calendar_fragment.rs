//! Resolve a fragment of the 8-char short hash to a cached event.

use anyhow::Result;
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};

use pidge_core::{CacheLookup, CachedEventRef, EventCache};

pub fn resolve(fragment: &str) -> Result<(String, CachedEventRef)> {
    let cache = EventCache::load()?;
    match cache.find_by_fragment(fragment) {
        CacheLookup::NotFound => Err(pidge_core::FragmentError::NotFound {
            fragment: fragment.to_string(),
        }
        .into()),
        CacheLookup::Ambiguous(matches) => {
            print_ambiguous(&matches);
            Err(pidge_core::FragmentError::Ambiguous {
                fragment: fragment.to_string(),
                count: matches.len(),
            }
            .into())
        }
        CacheLookup::One(h, r) => Ok((h, r)),
    }
}

pub fn purge_from_cache(short_hash: &str) -> Result<()> {
    let mut cache = EventCache::load()?;
    cache.entries.remove(short_hash);
    cache.save()?;
    Ok(())
}

fn print_ambiguous(matches: &[(String, CachedEventRef)]) {
    eprintln!("Fragment matches multiple events:");
    let mut table = Table::new();
    table.load_style(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["ID", "ACCOUNT", "EVENT ID"]);
    for (hash, r) in matches {
        table.add_row(vec![
            hash.dimmed().to_string(),
            r.account.clone(),
            r.event_id.chars().take(20).collect::<String>() + "…",
        ]);
    }
    eprintln!("{table}");
}
