//! The outbox: sending with a hold instead of a question.
//!
//! `mail_send` assesses the draft (who it goes to, what it contains), holds
//! it for 10 s to 2 min depending on risk, or keeps it until the user
//! approves when it looks sensitive. Anything due is sent by [`tick`], which
//! runs before every operation, on a timer in the servers, and in a detached
//! `pidge outbox run` process for the CLI.

use std::sync::Arc;
use std::time::Duration;

use ailloy::{Question, Questions};
use chrono::{DateTime, Utc};
use pidge_core::provider::Draft;
use pidge_store::OutboxEntry;
use serde_json::json;

use crate::ctx::{Frontend, User};
use crate::render;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Low,
    Normal,
    High,
    /// Held until the user approves.
    Approval,
}

impl Level {
    pub fn hold(self) -> Option<chrono::Duration> {
        match self {
            Level::Low => Some(chrono::Duration::seconds(10)),
            Level::Normal => Some(chrono::Duration::seconds(30)),
            Level::High => Some(chrono::Duration::seconds(120)),
            Level::Approval => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Risk {
    pub level: Level,
    pub reasons: Vec<String>,
}

const CONSUMER_DOMAINS: &[&str] = &[
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "live.se",
    "hotmail.se",
    "icloud.com",
    "me.com",
    "yahoo.com",
    "proton.me",
    "protonmail.com",
];

const SENSITIVE_WORDS: &[&str] = &[
    "password",
    "lösenord",
    "passwd",
    "pin-kod",
    "pin code",
    "bankid",
    "personnummer",
    "social security",
    "diagnos",
    "diagnosis",
    "sjukskriv",
    "medical record",
    "journalanteckning",
    "konfidentiell",
    "confidential",
    "sekretess",
];

/// Digits only, keeping their positions in runs separated by spaces/dashes.
fn digit_runs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut gap = 0;
    for c in text.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
            gap = 0;
        } else if (c == ' ' || c == '-' || c == '+') && !cur.is_empty() && gap == 0 {
            gap += 1;
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            gap = 0;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn luhn(d: &str) -> bool {
    let mut sum = 0;
    for (i, c) in d.chars().rev().enumerate() {
        let mut n = c.to_digit(10).unwrap_or(0);
        if i % 2 == 1 {
            n *= 2;
            if n > 9 {
                n -= 9;
            }
        }
        sum += n;
    }
    sum % 10 == 0
}

/// Things in a text that should not leave by accident.
pub fn sensitive_findings(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = text.to_lowercase();
    for run in digit_runs(text) {
        // Swedish personal identity number: YYMMDD-NNNN / YYYYMMDDNNNN.
        let pnr = (run.len() == 10 || run.len() == 12) && luhn(&run[run.len() - 10..]) && {
            let d = &run[run.len() - 10..];
            let month: u32 = d[2..4].parse().unwrap_or(0);
            let day: u32 = d[4..6].parse().unwrap_or(0);
            (1..=12).contains(&month) && (1..=31).contains(&day)
        };
        if pnr {
            out.push("what looks like a personal identity number".into());
        } else if (13..=19).contains(&run.len()) && luhn(&run) {
            out.push("what looks like a card number".into());
        }
    }
    // IBAN-ish: two letters, two digits, 10+ alphanumerics.
    for word in text.split_whitespace() {
        let w: String = word.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if w.len() >= 15
            && w.len() <= 34
            && w[..2].chars().all(|c| c.is_ascii_uppercase())
            && w[2..4].chars().all(|c| c.is_ascii_digit())
        {
            out.push("what looks like a bank account (IBAN)".into());
            break;
        }
    }
    for w in SENSITIVE_WORDS {
        if lower.contains(w) {
            out.push(format!("the word \"{w}\""));
            break;
        }
    }
    out.sort();
    out.dedup();
    out
}

fn domain(a: &str) -> String {
    a.rsplit('@').next().unwrap_or("").to_ascii_lowercase()
}

/// How risky is sending this draft from `from`? `forward`: it forwards.
pub async fn assess(user: &User, draft: &Draft, from: &str, forward: bool, reply: bool) -> Risk {
    let mut level = Level::Normal;
    let mut reasons = Vec::new();
    let all: Vec<&pidge_core::MessageFrom> =
        draft.to.iter().chain(&draft.cc).chain(&draft.bcc).collect();
    let raise = |level: &mut Level, to: Level| {
        if to > *level {
            *level = to;
        }
    };
    if all.len() > 20 {
        raise(&mut level, Level::Approval);
        reasons.push(format!("{} recipients", all.len()));
    } else if all.len() > 5 {
        raise(&mut level, Level::High);
        reasons.push(format!("{} recipients", all.len()));
    }
    let own_domain = domain(from);
    let work = !CONSUMER_DOMAINS.contains(&own_domain.as_str());
    for r in &all {
        let known = user.store.sent_to_count(&r.address).unwrap_or(0) > 0;
        if !known {
            let external = work && domain(&r.address) != own_domain;
            raise(&mut level, Level::High);
            reasons.push(format!(
                "first mail to {}{}",
                r.address,
                if external {
                    " (outside your organisation)"
                } else {
                    ""
                }
            ));
        }
    }
    if forward {
        raise(&mut level, Level::High);
        reasons.push("a forward".into());
    }
    if draft.has_attachments {
        raise(&mut level, Level::High);
        reasons.push("attachments".into());
    }
    let findings = sensitive_findings(&draft.body_text);
    if !findings.is_empty() {
        raise(&mut level, Level::Approval);
        reasons.push(format!("it contains {}", findings.join(", ")));
    }
    if level == Level::Normal
        && reply
        && all.len() == 1
        && draft.body_text.chars().count() < 1500
        && reasons.is_empty()
    {
        level = Level::Low;
    }
    // The user's own AI, when configured, may raise the level.
    if level < Level::Approval
        && let Some(client) = user.ai()
    {
        let mut q = Questions::new();
        q.insert(
            "sensitivity".into(),
            Question::score(
                "How sensitive is sending this e-mail (`email`): could it cause harm if it went to the wrong person or was sent by mistake?",
            )
            .levels([
                "Routine: nothing private or consequential",
                "Somewhat personal or consequential",
                "Private, financial, legal or commitments the sender may regret",
                "Highly sensitive: identity numbers, health, credentials, legal or financial secrets",
            ]),
        );
        let state = json!({ "email": {
            "to": all.iter().map(|r| &r.address).collect::<Vec<_>>(),
            "subject": draft.subject,
            "body": draft.body_text.chars().take(4000).collect::<String>(),
        }});
        if let Ok(Ok(resp)) =
            tokio::time::timeout(Duration::from_secs(8), client.eval(state, &q)).await
            && let Some(score) = resp
                .answers
                .get("sensitivity")
                .and_then(|a| a.normalized_score())
        {
            if score >= 0.85 {
                raise(&mut level, Level::Approval);
                reasons.push("your AI judged it highly sensitive".into());
            } else if score >= 0.6 {
                raise(&mut level, Level::High);
                reasons.push("your AI judged it sensitive".into());
            }
        }
    }
    Risk { level, reasons }
}

/// Send everything due. Safe to call often and from several processes.
pub async fn tick(user: &Arc<User>) {
    let now = Utc::now();
    while let Ok(Some(mut e)) = user.store.claim_due(now) {
        let result = match user.provider(&e.account) {
            Ok(p) => p.send_draft(&e.draft_id).await.map_err(|e| e.to_string()),
            Err(err) => Err(err.message),
        };
        match result {
            Ok(()) => {
                let _ = user.store.remove_outbox(&e.draft);
                let _ = user.store.push_note(&format!("Sent {}.", e.summary));
            }
            Err(why) => {
                e.status = "failed".into();
                e.error = Some(why.clone());
                let _ = user.store.put_outbox(&e);
                let _ = user.store.push_note(&format!(
                    "Sending {} failed ({}); it is still a draft ({}).",
                    e.summary,
                    render::cap_inline(&why, 120),
                    e.draft
                ));
            }
        }
    }
}

/// Make sure what is held goes out on time.
pub fn schedule(user: &Arc<User>) {
    let Ok(Some(at)) = user.store.next_due() else {
        return;
    };
    match user.frontend() {
        Frontend::Cli => user.host.outbox_due(at),
        Frontend::Mcp => {
            let u = user.clone();
            let handle = tokio::spawn(async move {
                loop {
                    let Ok(Some(at)) = u.store.next_due() else {
                        return;
                    };
                    let wait = (at - Utc::now()).to_std().unwrap_or_default();
                    tokio::time::sleep(wait + Duration::from_millis(200)).await;
                    tick(&u).await;
                }
            });
            let mut slot = user.outbox_timer.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(old) = slot.replace(handle) {
                old.abort();
            }
        }
    }
}

/// For `pidge outbox run`: send until nothing is held.
pub async fn run_until_empty(user: &Arc<User>) {
    loop {
        tick(user).await;
        match user.store.next_due() {
            Ok(Some(at)) => {
                let wait = (at - Utc::now()).to_std().unwrap_or_default();
                tokio::time::sleep(wait + Duration::from_millis(200)).await;
                user.reload_prefs().await;
            }
            _ => return,
        }
    }
}

/// A held entry, for the outbox listing.
pub fn describe(e: &OutboxEntry, now: DateTime<Utc>) -> String {
    match (e.status.as_str(), e.send_at) {
        ("held", Some(t)) => format!(
            "{} {}: goes out in {} s",
            e.draft,
            e.summary,
            (t - now).num_seconds().max(0)
        ),
        ("approval", _) => format!(
            "{} {}: waiting for approval ({})",
            e.draft,
            e.summary,
            e.reasons.join("; ")
        ),
        ("failed", _) => format!(
            "{} {}: failed ({})",
            e.draft,
            e.summary,
            e.error.clone().unwrap_or_default()
        ),
        (s, _) => format!("{} {}: {s}", e.draft, e.summary),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_card_numbers_are_caught() {
        assert!(!sensitive_findings("mitt personnummer är 811218-9876").is_empty());
        // A valid test card number (Luhn).
        assert!(
            sensitive_findings("kort 4111 1111 1111 1111")
                .iter()
                .any(|f| f.contains("card"))
        );
        assert!(sensitive_findings("Ses kl 14, rum 4012").is_empty());
        assert!(
            sensitive_findings("IBAN SE4550000000058398257466")
                .iter()
                .any(|f| f.contains("IBAN"))
        );
    }

    #[test]
    fn levels_hold_longer_as_they_rise() {
        assert!(Level::Low.hold() < Level::Normal.hold());
        assert!(Level::Normal.hold() < Level::High.hold());
        assert_eq!(Level::Approval.hold(), None);
    }
}
