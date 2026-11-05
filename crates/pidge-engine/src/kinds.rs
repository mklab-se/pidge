//! What a message *is*, from cheap deterministic signals: the provider's own
//! sorting (Focused/Other, Gmail's tabs), list headers, sender shapes and a
//! few subject words in Swedish and English. A kind is either confident or
//! not given at all: "unsure" is a fine answer, and the harness or the
//! user's AI decides those.

use pidge_core::provider::{BulkHint, MailItem};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Receipts, invoices, bills, order and payment confirmations.
    Receipt,
    /// Editorial mail the user subscribed to.
    Newsletter,
    /// Marketing, offers, ads ("commercials").
    Promotion,
    /// Automated notices from services: alerts, codes, shipping, status.
    Notification,
    /// Social network notices.
    Social,
    /// Meeting invitations.
    Invite,
    /// From a person, written to the user.
    Personal,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Receipt,
        Kind::Newsletter,
        Kind::Promotion,
        Kind::Notification,
        Kind::Social,
        Kind::Invite,
        Kind::Personal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Receipt => "receipt",
            Kind::Newsletter => "newsletter",
            Kind::Promotion => "promotion",
            Kind::Notification => "notification",
            Kind::Social => "social",
            Kind::Invite => "invite",
            Kind::Personal => "personal",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "receipt" | "receipts" | "invoice" | "invoices" | "bill" | "bills" => Kind::Receipt,
            "newsletter" | "newsletters" => Kind::Newsletter,
            "promotion" | "promotions" | "commercial" | "commercials" | "ad" | "ads"
            | "marketing" | "spam" => Kind::Promotion,
            "notification" | "notifications" | "automated" => Kind::Notification,
            "social" => Kind::Social,
            "invite" | "invites" | "invitation" => Kind::Invite,
            "personal" => Kind::Personal,
            _ => return None,
        })
    }

    /// Plural, for counts read aloud.
    pub fn plural(self) -> &'static str {
        match self {
            Kind::Receipt => "receipts and bills",
            Kind::Newsletter => "newsletters",
            Kind::Promotion => "promotions",
            Kind::Notification => "notifications",
            Kind::Social => "social notices",
            Kind::Invite => "invitations",
            Kind::Personal => "personal messages",
        }
    }

    /// Bulk kinds: safe to clear in a sweep.
    pub fn is_bulk(self) -> bool {
        matches!(
            self,
            Kind::Newsletter | Kind::Promotion | Kind::Notification | Kind::Social
        )
    }
}

const RECEIPT_WORDS: &[&str] = &[
    "kvitto",
    "faktura",
    "receipt",
    "invoice",
    "orderbekräftelse",
    "order confirmation",
    "your order",
    "din order",
    "din beställning",
    "betalningsbekräftelse",
    "payment confirmation",
    "payment received",
    "betalning mottagen",
    "betalningspåminnelse",
    "payment reminder",
    "förfallodag",
    "due date",
    "your bill",
    "din räkning",
    "e-faktura",
    "billing statement",
    "tack för ditt köp",
    "thanks for your purchase",
    "thank you for your purchase",
    "thank you for your order",
    "tack för din beställning",
];

const RECEIPT_SENDERS: &[&str] = &[
    "klarna.",
    "paypal.",
    "stripe.com",
    "billing",
    "invoice",
    "faktura",
    "receipts",
    "kvitto",
];

const PROMO_WORDS: &[&str] = &[
    "rea",
    "sale",
    "erbjudande",
    "kampanj",
    "rabatt",
    "discount",
    "% off",
    "deal",
    "black friday",
    "cyber monday",
    "fri frakt",
    "free shipping",
    "nyheter i butik",
    "shop now",
    "handla nu",
    "limited time",
    "endast idag",
    "only today",
    "last chance",
    "sista chansen",
    "spara ",
    "save ",
    "%",
];

const NEWSLETTER_WORDS: &[&str] = &[
    "newsletter",
    "nyhetsbrev",
    "digest",
    "veckobrev",
    "weekly",
    "månadsbrev",
    "monthly update",
    "this week in",
    "veckans",
];

const NOTIFY_WORDS: &[&str] = &[
    "verification code",
    "verifieringskod",
    "engångskod",
    "security alert",
    "säkerhetsvarning",
    "sign-in",
    "inloggning",
    "password",
    "lösenord",
    "your package",
    "ditt paket",
    "has shipped",
    "har skickats",
    "delivery",
    "leverans",
    "reminder:",
    "påminnelse:",
    "notification",
    "avisering",
    "status update",
];

const SOCIAL_DOMAINS: &[&str] = &[
    "linkedin.com",
    "facebookmail.com",
    "facebook.com",
    "instagram.com",
    "x.com",
    "twitter.com",
    "strava.com",
    "meetup.com",
];

fn automated_sender(address: &str) -> bool {
    let local = address.split('@').next().unwrap_or("").to_ascii_lowercase();
    ["noreply", "no-reply", "donotreply", "do-not-reply", "notifications", "notification", "notify", "alerts", "mailer-daemon", "bounce", "info", "news", "nyhetsbrev", "newsletter", "marketing", "kampanj", "hello", "team"]
        .iter()
        .any(|w| local == *w || local.starts_with(&format!("{w}.")) || local.starts_with(&format!("{w}-")) || local.starts_with(&format!("{w}+")) || local.contains("noreply") || local.contains("no-reply"))
}

fn has_any(text: &str, words: &[&str]) -> bool {
    words.iter().any(|w| {
        if w.len() <= 4 && !w.contains(' ') && !w.contains('%') {
            // Short words must stand alone: "rea" is not "area".
            text.split(|c: char| !c.is_alphanumeric())
                .any(|t| t == *w)
        } else {
            text.contains(w)
        }
    })
}

/// What the facts say, with confidence: `Some` only when it is near certain.
/// `knows_sender`: the user has written to this sender before.
pub fn signal_kind(m: &MailItem, knows_sender: bool) -> Option<Kind> {
    if m.is_invite {
        return Some(Kind::Invite);
    }
    let subject = m.subject.to_lowercase();
    let preview = m.preview.to_lowercase();
    let text = format!("{subject} {preview}");
    let from = m.from.address.to_ascii_lowercase();
    let domain = from.rsplit('@').next().unwrap_or("");
    let automated = automated_sender(&from);

    if has_any(&subject, RECEIPT_WORDS)
        || (RECEIPT_SENDERS.iter().any(|s| from.contains(s)) && has_any(&text, RECEIPT_WORDS))
    {
        return Some(Kind::Receipt);
    }
    if knows_sender && !automated && !m.list_unsubscribe {
        return Some(Kind::Personal);
    }
    if SOCIAL_DOMAINS.iter().any(|d| domain == *d || domain.ends_with(&format!(".{d}")))
        || m.bulk_hint == Some(BulkHint::Social)
    {
        return Some(Kind::Social);
    }
    match m.bulk_hint {
        Some(BulkHint::Promotions) => return Some(Kind::Promotion),
        Some(BulkHint::Forums) => return Some(Kind::Newsletter),
        _ => {}
    }
    let bulky = m.list_unsubscribe
        || matches!(m.bulk_hint, Some(BulkHint::Other) | Some(BulkHint::Updates))
        || automated;
    if bulky && has_any(&subject, PROMO_WORDS) {
        return Some(Kind::Promotion);
    }
    if bulky && has_any(&subject, NEWSLETTER_WORDS) {
        return Some(Kind::Newsletter);
    }
    if automated && has_any(&text, NOTIFY_WORDS) {
        return Some(Kind::Notification);
    }
    if m.bulk_hint == Some(BulkHint::Updates) && automated {
        return Some(Kind::Notification);
    }
    if m.list_unsubscribe && m.bulk_hint == Some(BulkHint::Other) {
        // A list in Other: bulk for sure, newsletter vs promotion unclear;
        // promotion-like when the sender is a marketing address.
        return Some(if from.contains("news") || from.contains("letter") {
            Kind::Newsletter
        } else {
            Kind::Promotion
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pidge_core::MessageFrom;
    use pidge_core::provider::Folder;

    fn m(from: &str, subject: &str, hint: Option<BulkHint>, list: bool) -> MailItem {
        MailItem {
            account: "me@x".into(),
            id: "1".into(),
            thread_id: "t".into(),
            folder: Folder::Inbox,
            from: MessageFrom {
                name: String::new(),
                address: from.into(),
            },
            to: vec![],
            cc: vec![],
            subject: subject.into(),
            received_at: Utc::now(),
            preview: String::new(),
            unread: true,
            flagged: false,
            has_attachments: false,
            categories: vec![],
            bulk_hint: hint,
            list_unsubscribe: list,
            is_invite: false,
        }
    }

    #[test]
    fn receipts_by_subject_in_both_languages() {
        assert_eq!(signal_kind(&m("no-reply@telia.se", "Din faktura för oktober", None, false), false), Some(Kind::Receipt));
        assert_eq!(signal_kind(&m("orders@shop.com", "Your receipt from Shop", None, false), false), Some(Kind::Receipt));
    }

    #[test]
    fn promotions_need_a_bulk_signal() {
        assert_eq!(signal_kind(&m("news@shop.se", "REA: 30% på allt", Some(BulkHint::Other), true), false), Some(Kind::Promotion));
        assert_eq!(signal_kind(&m("x@y.se", "Hej", Some(BulkHint::Promotions), false), false), Some(Kind::Promotion));
        // A colleague writing "rea" is not a promotion.
        assert_eq!(signal_kind(&m("anna@firma.se", "Om rean", Some(BulkHint::Focused), false), false), None);
    }

    #[test]
    fn short_words_stand_alone() {
        assert!(!has_any("the area office", &["rea"]));
        assert!(has_any("stor rea nu", &["rea"]));
    }

    #[test]
    fn known_people_are_personal() {
        assert_eq!(signal_kind(&m("anna@x.se", "Middag?", Some(BulkHint::Focused), false), true), Some(Kind::Personal));
    }

    #[test]
    fn notifications_and_social() {
        assert_eq!(signal_kind(&m("noreply@bank.se", "Ny inloggning", None, false), false), Some(Kind::Notification));
        assert_eq!(signal_kind(&m("messages-noreply@linkedin.com", "You appeared in 4 searches", None, true), false), Some(Kind::Social));
    }

    #[test]
    fn kind_names_parse_plurals_and_synonyms() {
        assert_eq!(Kind::parse("Commercials"), Some(Kind::Promotion));
        assert_eq!(Kind::parse("invoices"), Some(Kind::Receipt));
        assert_eq!(Kind::parse("weather"), None);
    }
}
