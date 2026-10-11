//! The user's mail categories: what a review sorts into, edited through
//! `mail_categories`. Defaults are offered on first use; `Other` is
//! implicit, always last, and cannot be removed or renamed.

use rmcp::schemars;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The implicit catch-all category.
pub const OTHER: &str = "Other";

pub const MAX_CATEGORIES: usize = 12;
pub const MAX_RULES: usize = 20;
pub const MAX_EXAMPLES: usize = 5;
pub const MAX_NAME: usize = 40;
pub const MAX_DESCRIPTION: usize = 300;
/// Characters kept of an example's sender and subject.
const MAX_EXAMPLE_FIELD: usize = 120;

/// How the no-AI fallback maps Graph signals onto a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    /// Mailing-list mail (a `List-Unsubscribe` or `Precedence: bulk` header).
    Bulk,
    /// Machine-sent mail (`noreply` senders, `Auto-Submitted`).
    Automated,
}

/// A real message the user pointed at, as a hint for the classifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Example {
    pub from: String,
    pub subject: String,
}

impl Example {
    pub fn new(from: &str, subject: &str) -> Self {
        Self {
            from: clip(from, MAX_EXAMPLE_FIELD),
            subject: clip(subject, MAX_EXAMPLE_FIELD),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `sender:<address>` or `domain:<domain>`, lower-cased.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<Example>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<Signal>,
    /// Offer "archive all of these" when a review starts.
    #[serde(default)]
    pub bulk_suggestion: bool,
}

impl Category {
    fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            rules: Vec::new(),
            examples: Vec::new(),
            signal: None,
            bulk_suggestion: false,
        }
    }
}

/// The categories a user gets until they define their own.
pub fn defaults() -> Vec<Category> {
    let bulk = |mut c: Category| {
        c.signal = Some(Signal::Bulk);
        c.bulk_suggestion = true;
        c
    };
    let mut notifications = Category::new(
        "Notifications",
        "Automated messages from services and systems: alerts, confirmations of account activity, status updates, shipping updates",
    );
    notifications.signal = Some(Signal::Automated);
    vec![
        Category::new(
            "Important",
            "Needs the user's own attention or action soon: a question to them, a decision, a deadline, a problem",
        ),
        Category::new(
            "Personal",
            "From friends and family, about private life, not work or commerce",
        ),
        Category::new(
            "Bills and receipts",
            "Invoices, payment reminders, receipts and order confirmations with an amount",
        ),
        bulk(Category::new(
            "Newsletters",
            "Subscribed editorial content and digests: articles, updates from organisations the user follows",
        )),
        bulk(Category::new(
            "Promotions",
            "Marketing, offers, sales, discounts and advertising",
        )),
        notifications,
    ]
}

/// A trimmed, single-line category name, refused if empty, too long, or
/// the reserved `Other`.
pub fn clean_name(name: &str) -> Result<String, String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("a category needs a name".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("category names are at most {MAX_NAME} characters"));
    }
    if name.eq_ignore_ascii_case(OTHER) {
        return Err(format!(
            "\"{OTHER}\" is built in: it holds whatever fits no other category"
        ));
    }
    Ok(name)
}

/// A single-line description of at most [`MAX_DESCRIPTION`] characters.
pub fn clean_description(description: &str) -> Result<String, String> {
    let d = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if d.chars().count() > MAX_DESCRIPTION {
        return Err(format!(
            "descriptions are at most {MAX_DESCRIPTION} characters"
        ));
    }
    Ok(d)
}

/// A rule in its stored form: `sender:<address>` or `domain:<domain>`,
/// lower-cased. A bare address or domain is accepted and prefixed.
pub fn parse_rule(rule: &str) -> Result<String, String> {
    let rule = rule.trim().to_ascii_lowercase();
    let (kind, value) = match rule.split_once(':') {
        Some((k, v)) => (
            k.trim().to_string(),
            v.trim().trim_start_matches('@').to_string(),
        ),
        None if rule.contains('@') && !rule.starts_with('@') => ("sender".into(), rule.clone()),
        None => ("domain".into(), rule.trim_start_matches('@').to_string()),
    };
    let ok_chars = |s: &str| {
        !s.is_empty()
            && s.len() <= 254
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "@.-_+".contains(c))
    };
    match kind.as_str() {
        "sender" if ok_chars(&value) && value.matches('@').count() == 1 => {
            Ok(format!("sender:{value}"))
        }
        "domain" if ok_chars(&value) && !value.contains('@') && value.contains('.') => {
            Ok(format!("domain:{value}"))
        }
        _ => {
            Err("a rule is sender:<address> or domain:<domain>, e.g. domain:climbingclub.se".into())
        }
    }
}

/// Whether a stored rule matches a sender address.
pub fn rule_matches(rule: &str, from_address: &str) -> bool {
    let addr = from_address.trim().to_ascii_lowercase();
    match rule.split_once(':') {
        Some(("sender", v)) => addr == v,
        Some(("domain", v)) => addr
            .rsplit_once('@')
            .is_some_and(|(_, d)| d == v || d.ends_with(&format!(".{v}"))),
        _ => false,
    }
}

/// The first category with a rule matching `from_address`.
pub fn by_rule<'a>(categories: &'a [Category], from_address: &str) -> Option<&'a Category> {
    categories
        .iter()
        .find(|c| c.rules.iter().any(|r| rule_matches(r, from_address)))
}

/// A short stable fingerprint of the categories, for cache keys: any edit
/// changes it, so classifications made under other categories are not reused.
pub fn fingerprint(categories: &[Category]) -> String {
    let json = serde_json::to_string(categories).unwrap_or_default();
    Sha256::digest(json.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The index of `name` (case-insensitive) in `categories`.
pub fn position(categories: &[Category], name: &str) -> Option<usize> {
    categories
        .iter()
        .position(|c| c.name.eq_ignore_ascii_case(name.trim()))
}

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_put_bulk_categories_on_offer() {
        let d = defaults();
        assert_eq!(d.len(), 6);
        for c in &d {
            assert_eq!(clean_name(&c.name).unwrap(), c.name);
            assert!(clean_description(&c.description).is_ok());
        }
        let bulk: Vec<&str> = d
            .iter()
            .filter(|c| c.bulk_suggestion)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(bulk, ["Newsletters", "Promotions"]);
    }

    #[test]
    fn names_are_cleaned_and_other_is_reserved() {
        assert_eq!(clean_name("  Leisure \n time ").unwrap(), "Leisure time");
        assert!(clean_name("other").is_err());
        assert!(clean_name("  ").is_err());
        assert!(clean_name(&"x".repeat(MAX_NAME + 1)).is_err());
    }

    #[test]
    fn rules_parse_and_match() {
        assert_eq!(
            parse_rule("Anna@Example.com").unwrap(),
            "sender:anna@example.com"
        );
        assert_eq!(
            parse_rule("@climbingclub.se").unwrap(),
            "domain:climbingclub.se"
        );
        assert_eq!(
            parse_rule("domain: news.example.com").unwrap(),
            "domain:news.example.com"
        );
        assert!(parse_rule("sender:not-an-address").is_err());
        assert!(parse_rule("domain:evil/../x").is_err());
        assert!(parse_rule("folder:inbox").is_err());

        assert!(rule_matches("sender:anna@example.com", "ANNA@example.com"));
        assert!(rule_matches("domain:example.com", "x@mail.example.com"));
        assert!(rule_matches("domain:example.com", "x@example.com"));
        assert!(!rule_matches("domain:example.com", "x@notexample.com"));
    }

    #[test]
    fn fingerprint_changes_with_any_edit() {
        let mut c = defaults();
        let a = fingerprint(&c);
        c[0].description.push('!');
        assert_ne!(a, fingerprint(&c));
    }
}
