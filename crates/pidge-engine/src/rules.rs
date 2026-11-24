//! "Always do this": the user's standing rules, applied to mail that arrives
//! after they said so. A rule never touches mail that matters, everything it
//! does can be undone, and the user hears of it once.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use pidge_core::provider::Folder;
use pidge_store::{Filter, Stored};

use crate::ctx::User;
use crate::kinds::Kind;
use crate::ops::mail_act;
use crate::prefs::{Always, matches_any};

pub fn matches(rule: &Always, s: &Stored) -> bool {
    if rule.since.is_none_or(|t| s.item.received_at < t) {
        return false;
    }
    if rule.sender.is_none() && rule.kind.is_none() {
        return false;
    }
    if let Some(sender) = &rule.sender
        && !matches_any(std::slice::from_ref(sender), &s.item.from.address)
    {
        return false;
    }
    if let Some(kind) = &rule.kind
        && s.class.kind.as_deref() != Some(kind.as_str())
    {
        return false;
    }
    if let Some(status) = &rule.status
        && crate::delivery::status(&s.item).map(|x| x.as_str()) != Some(status.as_str())
    {
        return false;
    }
    true
}

/// A rule as a person would say it.
pub fn describe(r: &Always) -> String {
    let what = match (&r.kind, &r.status) {
        (Some(k), Some(st)) if k == Kind::Delivery.as_str() => {
            format!("parcels that are {}", st.replace('_', " "))
        }
        (Some(k), _) => Kind::parse(k).map_or(k.clone(), |k| k.plural().to_string()),
        (None, _) => "mail".into(),
    };
    let from = r
        .sender
        .as_deref()
        .map(|s| {
            format!(
                " from {}",
                s.trim_start_matches("sender:")
                    .trim_start_matches("domain:")
            )
        })
        .unwrap_or_default();
    let how = match r.action.as_str() {
        "move" => format!("move to {}", r.to.as_deref().unwrap_or("")),
        "categorize" => format!("categorize as {}", r.categories.join(", ")),
        a => a.to_string(),
    };
    format!("{how} {what}{from}")
}

/// Apply the rules to new mail in the inbox.
pub async fn apply(user: &Arc<User>) {
    let prefs = user.prefs().await;
    if prefs.always.is_empty() {
        return;
    }
    // Only what just came. An index built anew has forgotten what was done
    // before: there, only what came since it started.
    let now = Utc::now();
    let since = if user.host.always_on() {
        (user.started - chrono::Duration::minutes(10)).max(now - chrono::Duration::days(1))
    } else {
        now - chrono::Duration::days(1)
    };
    let fresh = match user.store.query(&Filter {
        folder: Some(Folder::Inbox),
        since: Some(since),
        ..Default::default()
    }) {
        Ok(items) => items,
        Err(_) => return,
    };
    let mut by: BTreeMap<usize, Vec<(String, String)>> = BTreeMap::new();
    for s in fresh.iter().filter(|s| s.classified_at.is_some()) {
        // By Message-ID: a message the user took back from a rule may come
        // back to the inbox as another row.
        let key = if s.item.message_id.is_empty() {
            format!("{}|{}", s.item.account, s.item.id)
        } else {
            format!("{}|{}", s.item.account, s.item.message_id)
        };
        if matches!(user.store.get_doc::<bool>("ruled", &key), Ok(Some(_))) {
            continue;
        }
        let _ = user
            .store
            .put_doc("ruled", &key, &true, Some(now + chrono::Duration::days(3)));
        let matters = s.class.importance == 2 || s.item.flagged || s.item.is_invite;
        if !matters && let Some(n) = prefs.always.iter().position(|r| matches(r, s)) {
            by.entry(n)
                .or_default()
                .push((s.item.account.clone(), s.item.id.clone()));
        }
    }
    for (n, targets) in by {
        let rule = &prefs.always[n];
        let action = mail_act::Action {
            action: rule.action.clone(),
            to: rule.to.clone(),
            categories: Some(rule.categories.clone()).filter(|c| !c.is_empty()),
            ..Default::default()
        };
        // What the user's policy wants asked about is not done unasked.
        if !matches!(
            mail_act::allowed(user, &action, targets.len(), true).await,
            Ok(true)
        ) {
            continue;
        }
        match mail_act::carry_out_by_rule(user, &action, &targets).await {
            Ok(done) => {
                let _ = user.store.push_note(&format!(
                    "{} {} on its own, as the user said to always do ({}; {}).",
                    mail_act::verb(&rule.action),
                    targets.len(),
                    describe(rule),
                    done.ids.join(", ")
                ));
            }
            Err(e) => tracing::warn!(error = %e.message, "a rule could not be applied"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mail;
    use pidge_core::provider::BulkHint;
    use pidge_store::Classification;

    fn stored(kind: &str, subject: &str, age_h: i64) -> Stored {
        Stored {
            handle: "m1".into(),
            item: mail(
                "1",
                "no@postnord.se",
                subject,
                BulkHint::Other,
                false,
                age_h,
            ),
            class: Classification {
                kind: Some(kind.into()),
                importance: 1,
                ..Default::default()
            },
            classified_at: Some(Utc::now()),
            happening: None,
            time_checked: true,
        }
    }

    #[test]
    fn a_rule_is_for_mail_that_comes_after_it_was_said() {
        let rule = Always {
            kind: Some("delivery".into()),
            status: Some("delivered".into()),
            action: "archive".into(),
            since: Some(Utc::now() - chrono::Duration::hours(2)),
            ..Default::default()
        };
        assert!(matches(
            &rule,
            &stored("delivery", "Paketet är levererat", 1)
        ));
        assert!(!matches(
            &rule,
            &stored("delivery", "Paketet är levererat", 3)
        ));
        assert!(!matches(&rule, &stored("delivery", "Paketet är på väg", 1)));
        assert!(!matches(
            &rule,
            &stored("notification", "Paketet är levererat", 1)
        ));
        assert_eq!(describe(&rule), "archive parcels that are delivered");
    }

    #[test]
    fn a_rule_without_a_target_matches_nothing() {
        let rule = Always {
            action: "delete".into(),
            since: Some(Utc::now() - chrono::Duration::days(1)),
            ..Default::default()
        };
        assert!(!matches(&rule, &stored("delivery", "x", 1)));
    }
}
