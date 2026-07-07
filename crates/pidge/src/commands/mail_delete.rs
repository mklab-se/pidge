//! `pidge mail delete` — single or bulk delete with safety gates.
//!
//! Graph's DELETE /me/messages/{id} moves the message to Deleted Items (it's
//! not a hard delete). Recovery is still possible from Outlook's Deleted
//! Items folder.
//!
//! Safety:
//! - Single delete asks for an interactive Confirm by default; `-y` skips.
//! - Bulk delete (`--older-than`) ALWAYS requires `-y`. There is no
//!   interactive prompt — bulk mode is intended for scripts where typing
//!   `y` would be inconvenient, and forcing a flag means the user can't
//!   trip into it accidentally.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};
use colored::Colorize;
use futures::future::join_all;
use inquire::Confirm;
use std::collections::HashSet;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::Config;

use crate::commands::mail_fragment::{purge_from_cache, resolve};

pub async fn run(
    fragment: Option<String>,
    from: Vec<String>,
    older_than: Option<String>,
    accounts: Vec<String>,
    yes: bool,
) -> Result<()> {
    match (fragment, from.is_empty(), older_than.as_ref()) {
        (Some(f), true, None) => delete_single(f, yes).await,
        (None, false, _) | (None, _, Some(_)) => delete_bulk(from, older_than, accounts, yes).await,
        (None, true, None) => Err(anyhow!(
            "Specify a fragment, `--from <sender>`, or `--older-than <spec>`. \
             Run `pidge mail delete --help`."
        )),
        (Some(_), _, _) => unreachable!("clap enforces conflicts_with"),
    }
}

async fn delete_single(fragment: String, yes: bool) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let gate = crate::guardrail::gate(
        crate::guardrail::GuardrailAction::Delete,
        &format!("delete message {short} (moves to Deleted Items)"),
    )?;
    if gate == crate::guardrail::Gate::DryRun {
        return Ok(());
    }
    if !yes {
        let confirmed = Confirm::new(&format!(
            "Delete message {} (moves to Deleted Items)?",
            short.dimmed()
        ))
        .with_default(false)
        .prompt()
        .map_err(|e| anyhow!("prompt cancelled: {e}"))?;
        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    graph
        .delete_message(&msg.account, &msg.graph_id)
        .await
        .context("Microsoft Graph rejected DELETE")?;
    let _ = purge_from_cache(&short);
    println!("{} Deleted {}.", "✔".green(), short.dimmed());
    Ok(())
}

async fn delete_bulk(
    from: Vec<String>,
    older_than: Option<String>,
    account_filter: Vec<String>,
    yes: bool,
) -> Result<()> {
    let gate = crate::guardrail::gate(
        crate::guardrail::GuardrailAction::Bulk,
        &format!("bulk delete: from={from:?} older_than={older_than:?}"),
    )?;
    if gate == crate::guardrail::Gate::DryRun {
        return Ok(());
    }

    if !yes {
        return Err(anyhow!(
            "Bulk delete requires explicit `-y` confirmation — there is no \
             interactive prompt. Re-run with `-y` if you really mean it."
        ));
    }

    let cutoff: Option<DateTime<Utc>> = older_than.as_deref().map(parse_older_than).transpose()?;
    let from_set: HashSet<String> = from.iter().map(|s| s.to_ascii_lowercase()).collect();

    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge account add` to add one."
        ));
    }
    let target_emails: Vec<String> = if account_filter.is_empty() {
        config.accounts.iter().map(|a| a.email.clone()).collect()
    } else {
        for f in &account_filter {
            if config.find(f).is_none() {
                return Err(anyhow!("not signed in to {f}"));
            }
        }
        account_filter
    };

    let scope = if from_set.is_empty() {
        "Inbox"
    } else {
        "mailbox"
    };
    let filter_desc = describe_filter(&from_set, cutoff.as_ref(), older_than.as_deref());
    println!(
        "{} Deleting {scope} messages where {}…",
        "Bulk".yellow().bold(),
        filter_desc
    );

    let graph = GraphClient::new(AuthClient::from_env()?)?;
    const PAGE_SIZE: usize = 50;
    const MAX_PAGES: usize = 200;

    let mut total_deleted = 0usize;
    for email in &target_emails {
        let count = if from_set.is_empty() {
            // Date-only mode: same shape as before — walk Inbox newest-first
            // and stop once we cross the cutoff.
            delete_bulk_for_account(
                &graph,
                email,
                cutoff.expect("date-only mode implies cutoff is Some"),
                PAGE_SIZE,
                MAX_PAGES,
            )
            .await?
        } else {
            // Sender-filter mode: search the whole mailbox per sender so
            // we sweep across all folders (Inbox + Junk + Archive + …).
            delete_bulk_by_sender_for_account(&graph, email, &from_set, cutoff).await?
        };
        total_deleted += count;
        println!(
            "{} {}: removed {count} message{}",
            "✔".green(),
            email,
            if count == 1 { "" } else { "s" }
        );
    }
    println!("{} Total: {total_deleted}.", "✔".green().bold());
    Ok(())
}

fn describe_filter(
    from: &HashSet<String>,
    cutoff: Option<&DateTime<Utc>>,
    older_than_spec: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !from.is_empty() {
        let mut senders: Vec<&str> = from.iter().map(String::as_str).collect();
        senders.sort_unstable();
        parts.push(format!("from is one of [{}]", senders.join(", ")));
    }
    if let (Some(c), Some(spec)) = (cutoff, older_than_spec) {
        parts.push(format!(
            "received before {} (cutoff {})",
            spec,
            c.format("%Y-%m-%d %H:%M UTC")
        ));
    }
    parts.join(" AND ")
}

/// Sender-filter bulk: one Graph search per sender across the whole
/// mailbox, then delete each match. Mirrors the corresponding archive
/// helper — see `mail_actions::archive_bulk_by_sender_for_account` for
/// the rationale (in short: marketing often auto-routes to Junk, so an
/// Inbox-only scan would miss it).
async fn delete_bulk_by_sender_for_account(
    graph: &GraphClient,
    account: &str,
    from_set: &HashSet<String>,
    cutoff: Option<DateTime<Utc>>,
) -> Result<usize> {
    const SEARCH_LIMIT: usize = 1000;
    let mut deleted = 0usize;
    for sender in from_set {
        let messages = match graph
            .search_messages(account, &format!("from:{sender}"), SEARCH_LIMIT)
            .await
        {
            Ok(page) => page.messages,
            Err(e) => {
                eprintln!("  {} search failed for {}: {e}", "!".red(), sender.dimmed());
                continue;
            }
        };
        let matches: Vec<&pidge_core::Message> = messages
            .iter()
            .filter(|m| {
                m.from.address.to_ascii_lowercase() == *sender
                    && cutoff.is_none_or(|c| m.received_at < c)
            })
            .collect();
        deleted += delete_messages(graph, account, &matches).await;
    }
    Ok(deleted)
}

/// Concurrently delete a batch of messages with the same throttling
/// safeguards as `move_to_archive`: 4 inflight, retry on HTTP 429.
async fn delete_messages(
    graph: &GraphClient,
    account: &str,
    messages: &[&pidge_core::Message],
) -> usize {
    use pidge_client::graph::batch::BatchRequest;
    let requests: Vec<BatchRequest> = messages
        .iter()
        .map(|m| {
            BatchRequest::bare(
                pidge_core::short_hash(&m.id),
                "DELETE",
                format!("/me/messages/{}", m.id),
            )
        })
        .collect();
    let responses = match graph.batch_all(account, requests).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  {} bulk delete failed: {e}", "!".red());
            return 0;
        }
    };
    let mut ok = 0usize;
    for item in responses {
        if item.is_success() {
            ok += 1;
            let _ = purge_from_cache(&item.id);
        } else if item.status == 404 {
            // already gone
        } else {
            eprintln!(
                "  {} failed to delete {}: HTTP {}",
                "!".red(),
                item.id.dimmed(),
                item.status
            );
        }
    }
    ok
}

async fn delete_bulk_for_account(
    graph: &GraphClient,
    account: &str,
    cutoff: DateTime<Utc>,
    page_size: usize,
    max_pages: usize,
) -> Result<usize> {
    let mut deleted = 0usize;
    for page in 0..max_pages {
        let skip = page * page_size;
        let result = graph
            .list_inbox(account, page_size, skip, false)
            .await
            .context("listing inbox for bulk delete")?;
        if result.messages.is_empty() {
            break;
        }
        // Filter to messages strictly older than cutoff.
        let to_delete: Vec<&pidge_core::Message> = result
            .messages
            .iter()
            .filter(|m| m.received_at < cutoff)
            .collect();

        // Delete in parallel within a small concurrency window — keeps Graph
        // happy and the user feedback timely.
        let futures = to_delete.iter().map(|m| {
            let id = m.graph_id_alias();
            async move { graph.delete_message(account, &id).await }
        });
        for (i, result) in join_all(futures).await.into_iter().enumerate() {
            match result {
                Ok(()) => {
                    deleted += 1;
                    let _ = purge_from_cache(&pidge_core::short_hash(&to_delete[i].id));
                }
                Err(ClientError::Graph { status: 404, .. }) => {
                    // already gone — ignore
                }
                Err(e) => {
                    eprintln!(
                        "  {} failed to delete {}: {e}",
                        "!".red(),
                        pidge_core::short_hash(&to_delete[i].id).dimmed()
                    );
                }
            }
        }

        // Stop paging if the OLDEST item on this page is still newer than
        // the cutoff — everything beyond is even newer.
        let oldest = result
            .messages
            .iter()
            .map(|m| m.received_at)
            .min()
            .unwrap_or(cutoff);
        if oldest >= cutoff {
            break;
        }
        if !result.has_more {
            break;
        }
    }
    Ok(deleted)
}

/// Resolve a `--older-than` argument to an absolute UTC cutoff.
///
/// Accepts:
/// - `YYYY-MM-DD` — interpreted as midnight UTC on that date
/// - `Nd` / `Nw` / `Nm` / `Ny` — N days/weeks/months/years before now
pub fn parse_older_than(spec: &str) -> Result<DateTime<Utc>> {
    let trimmed = spec.trim();
    // Absolute date?
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let dt = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| anyhow!("invalid date"))?;
        return Ok(DateTime::from_naive_utc_and_offset(dt, Utc));
    }
    // Relative duration?
    if let Some(unit_pos) = trimmed.find(|c: char| !c.is_ascii_digit()) {
        if unit_pos > 0 {
            let n: i64 = trimmed[..unit_pos]
                .parse()
                .map_err(|_| anyhow!("'{spec}' is not a valid duration"))?;
            let unit = &trimmed[unit_pos..];
            let now = Utc::now();
            let cutoff = match unit {
                "d" => now - Duration::days(n),
                "w" => now - Duration::weeks(n),
                "m" => subtract_months(now, n)?,
                "y" => subtract_months(now, n * 12)?,
                other => {
                    return Err(anyhow!(
                        "unknown duration unit '{other}' in '{spec}'. Use d/w/m/y."
                    ));
                }
            };
            return Ok(cutoff);
        }
    }
    Err(anyhow!(
        "'{spec}' is not a date (YYYY-MM-DD) or duration (e.g. 30d, 6m, 1y)."
    ))
}

/// Subtract N months from a UTC datetime, clamping the day to the new month's
/// length (so 2026-03-31 minus 1 month is 2026-02-28, not invalid).
fn subtract_months(dt: DateTime<Utc>, months: i64) -> Result<DateTime<Utc>> {
    let total = dt.year() as i64 * 12 + (dt.month() as i64 - 1) - months;
    let new_year = (total / 12) as i32;
    let new_month = (total % 12 + 1) as u32;
    let last_day_of_month = match new_month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            // crude leap-year check; close enough for "30 days ago" bucket
            if new_year % 4 == 0 && (new_year % 100 != 0 || new_year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => return Err(anyhow!("month overflow")),
    };
    let day = dt.day().min(last_day_of_month);
    let nd = NaiveDate::from_ymd_opt(new_year, new_month, day)
        .ok_or_else(|| anyhow!("date overflow"))?;
    let naive = nd
        .and_hms_opt(dt.hour(), dt.minute(), dt.second())
        .ok_or_else(|| anyhow!("time overflow"))?;
    Ok(DateTime::from_naive_utc_and_offset(naive, Utc))
}

// Thin alias so the call site reads better.
trait MessageExt {
    fn graph_id_alias(&self) -> String;
}
impl MessageExt for pidge_core::Message {
    fn graph_id_alias(&self) -> String {
        self.id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_absolute_date() {
        let d = parse_older_than("2026-01-01").unwrap();
        assert_eq!(d.format("%Y-%m-%d").to_string(), "2026-01-01");
    }

    #[test]
    fn parse_days() {
        let now = Utc::now();
        let d = parse_older_than("30d").unwrap();
        let delta = now - d;
        assert!(delta.num_hours() >= 29 * 24);
        assert!(delta.num_hours() <= 30 * 24 + 1);
    }

    #[test]
    fn parse_months() {
        let now = Utc::now();
        let d = parse_older_than("6m").unwrap();
        let delta = now - d;
        // ~6 months = ~180 days, allow a fairly wide band for varying month lengths
        assert!(delta.num_days() > 150);
        assert!(delta.num_days() < 200);
    }

    #[test]
    fn parse_unknown_unit_rejects() {
        assert!(parse_older_than("30x").is_err());
    }

    #[test]
    fn parse_garbage_rejects() {
        assert!(parse_older_than("yesterday").is_err());
        assert!(parse_older_than("").is_err());
    }

    #[test]
    fn subtract_months_clamps_day() {
        let dt = DateTime::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 3, 31)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            Utc,
        );
        let result = subtract_months(dt, 1).unwrap();
        assert_eq!(result.month(), 2);
        assert_eq!(result.day(), 28); // 2026 is not a leap year
    }
}
