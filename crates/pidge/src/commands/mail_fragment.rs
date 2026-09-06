//! Shared helpers for mail subcommands that take a fragment of the 8-char
//! short hash (`pidge mail show 3515`, `pidge mail flag 3515`, …).

use anyhow::Result;
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};

use pidge_core::{CacheLookup, CachedMessageRef, MessageCache};

/// Resolve a user-supplied fragment to a single cached message. Errors with a
/// helpful message when the fragment isn't found or matches several entries.
/// On ambiguity also prints a table of matches to stderr.
pub fn resolve(fragment: &str) -> Result<(String, CachedMessageRef)> {
    let cache = MessageCache::load()?;
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

/// Remove an entry from the on-disk cache. Used after server-side state
/// changes (`mark-read`, `archive`, …) so the next list refresh sees the new
/// situation, and after 404s so a stale ID doesn't keep matching.
pub fn purge_from_cache(short_hash: &str) -> Result<()> {
    let mut cache = MessageCache::load()?;
    cache.entries.remove(short_hash);
    cache.save()?;
    Ok(())
}

fn print_ambiguous(matches: &[(String, CachedMessageRef)]) {
    eprintln!("Fragment matches multiple messages:");
    let mut table = Table::new();
    table.load_style(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["ID", "ACCOUNT", "GRAPH ID"]);
    for (hash, r) in matches {
        table.add_row(vec![
            hash.dimmed().to_string(),
            r.account.clone(),
            r.graph_id.chars().take(20).collect::<String>() + "…",
        ]);
    }
    eprintln!("{table}");
}
