//! `pidge contacts refresh` — rebuild the local index by scanning recent
//! mail (inbox senders) and calendar (organizer + attendees).
//!
//! Coverage of recipient lists (to/cc/bcc) from inbox is deferred — pidge's
//! current `list_inbox` `$select` doesn't request them. See the design spec
//! `docs/superpowers/specs/2026-05-21-contact-resolution-design.md`.

use anyhow::Result;
use chrono::{Duration, Utc};
use colored::Colorize;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::{Config, ContactSource, ContactsCache};

const INBOX_FETCH_CAP: usize = 1000;
const CALENDAR_FETCH_CAP: usize = 500;

pub async fn run(days: i64, accounts_filter: Vec<String>, json: bool) -> Result<()> {
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

    let now = Utc::now();
    let window_start = now - Duration::days(days);
    let window_end = now + Duration::days(days);

    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let mut cache = ContactsCache::load()?;

    let mut per_account: Vec<(String, usize, usize)> = Vec::new();

    for a in &accounts {
        let mut mail_seen = 0usize;
        let mut cal_seen = 0usize;

        // Mail: senders only (list_inbox doesn't $select recipient lists).
        match graph.list_inbox(&a.email, INBOX_FETCH_CAP, 0, false).await {
            Ok(page) => {
                for m in page.messages {
                    if is_noreply(&m.from.address) {
                        continue;
                    }
                    cache.upsert(
                        &m.from.address,
                        &m.from.name,
                        m.received_at,
                        ContactSource::Mail,
                    );
                    mail_seen += 1;
                }
            }
            Err(e) => {
                eprintln!(
                    "{} {}: {e}",
                    "warning:".yellow(),
                    format!("mail scan failed for {}", a.email).dimmed()
                );
            }
        }

        // Calendar: organizer + every attendee.
        match graph
            .list_calendar_view(&a.email, None, window_start, window_end, CALENDAR_FETCH_CAP)
            .await
        {
            Ok(page) => {
                for ev in page.events {
                    if !is_noreply(&ev.organizer.address) {
                        cache.upsert(
                            &ev.organizer.address,
                            &ev.organizer.name,
                            ev.start.at,
                            ContactSource::Calendar,
                        );
                        cal_seen += 1;
                    }
                    for att in &ev.attendees {
                        if is_noreply(&att.address) {
                            continue;
                        }
                        cache.upsert(
                            &att.address,
                            &att.name,
                            ev.start.at,
                            ContactSource::Calendar,
                        );
                        cal_seen += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "{} {}: {e}",
                    "warning:".yellow(),
                    format!("calendar scan failed for {}", a.email).dimmed()
                );
            }
        }

        per_account.push((a.email.clone(), mail_seen, cal_seen));
    }

    cache.mark_refreshed(now);
    cache.save()?;

    if json {
        let report = serde_json::json!({
            "ok": true,
            "total_contacts": cache.by_email.len(),
            "accounts": per_account
                .iter()
                .map(|(email, m, c)| serde_json::json!({
                    "account": email,
                    "mail_observations": m,
                    "calendar_observations": c,
                }))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("{}", "Contacts refreshed.".bold());
    for (email, m, c) in &per_account {
        println!("  {email}: {m} mail observation(s), {c} calendar observation(s)");
    }
    println!(
        "{} {} contact(s) in the local index.",
        "Total:".dimmed(),
        cache.by_email.len()
    );
    Ok(())
}

fn is_noreply(addr: &str) -> bool {
    let lc = addr.trim().to_lowercase();
    if lc.is_empty() {
        return true;
    }
    let local = lc.split('@').next().unwrap_or("");
    matches!(
        local,
        "noreply" | "no-reply" | "donotreply" | "do-not-reply" | "no_reply"
    ) || local.starts_with("noreply+")
        || local.starts_with("no-reply+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_noreply_matches_common_patterns() {
        assert!(is_noreply("noreply@example.com"));
        assert!(is_noreply("No-Reply@Example.com"));
        assert!(is_noreply("donotreply@x.io"));
        assert!(is_noreply("do-not-reply@x.io"));
        assert!(is_noreply("noreply+orders@store.com"));
        assert!(is_noreply(""));
        assert!(is_noreply("   "));
    }

    #[test]
    fn is_noreply_lets_real_addresses_through() {
        assert!(!is_noreply("dino@needefy.se"));
        assert!(!is_noreply("k.liljeblad@example.com"));
        assert!(!is_noreply("replyable@x.com"));
    }
}
