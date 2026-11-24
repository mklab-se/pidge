//! Parcels: where a delivery mail says one is, and which mails are about
//! the same one. Words in Swedish and English, no model.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use pidge_core::provider::MailItem;
use pidge_store::Stored;
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    OnItsWay,
    OutForDelivery,
    ToCollect,
    Delivered,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::OnItsWay => "on_its_way",
            Status::OutForDelivery => "out_for_delivery",
            Status::ToCollect => "to_collect",
            Status::Delivered => "delivered",
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            Status::OnItsWay => "on its way",
            Status::OutForDelivery => "out for delivery",
            Status::ToCollect => "to collect",
            Status::Delivered => "delivered",
        }
    }
}

const CARRIERS: &[&str] = &[
    "postnord",
    "dhl",
    "fedex",
    "ups.com",
    "bring.",
    "budbee",
    "instabox",
    "instabee",
    "earlybird",
    "schenker",
    "airmee",
    "gls-",
    "dpd.",
    "hermes",
    "royalmail",
    "usps",
    "posten.",
];

const PARCEL_WORDS: &[&str] = &[
    "paket",
    "package",
    "parcel",
    "försändelse",
    "shipment",
    "sändning",
    "order",
    "beställning",
    "leverans",
    "delivery",
    "köp",
    "purchase",
];

const DELIVERED: &[&str] = &[
    "levererad",
    "levererat",
    "har levererats",
    "är framme",
    "delivered",
    "utlämnad",
    "utlämnat",
    "uthämtat",
];
const TO_COLLECT: &[&str] = &[
    "att hämta",
    "kan hämtas",
    "redo att hämtas",
    "klart att hämta",
    "hämta ditt paket",
    "hämta din",
    "redo för upphämtning",
    "klar för upphämtning",
    "ready for pickup",
    "ready to collect",
    "ready for collection",
    "pick up your",
];
const OUT: &[&str] = &[
    "ute för leverans",
    "ute för utkörning",
    "levereras idag",
    "levereras i dag",
    "kommer idag",
    "out for delivery",
    "arriving today",
];
const ON_ITS_WAY: &[&str] = &[
    "på väg",
    "är skickad",
    "är skickat",
    "har skickats",
    "har skickat",
    "skickats",
    "has been sent",
    "shipped",
    "on its way",
    "on the way",
    "dispatched",
    "in transit",
    "kommande leverans",
    "spåra din",
    "spåra ditt",
    "track your",
    "information om din leverans",
    "estimated delivery",
];
const NOT_A_PARCEL: &[&str] = &[
    "fri leverans",
    "fri frakt",
    "free delivery",
    "free shipping",
    "% ",
    "undelivered",
    "delivery status notification",
    "mail delivery",
];

fn has(text: &str, words: &[&str]) -> bool {
    words.iter().any(|w| text.contains(w))
}

fn stage(text: &str) -> Option<Status> {
    if has(text, TO_COLLECT) {
        Some(Status::ToCollect)
    } else if has(text, OUT) {
        Some(Status::OutForDelivery)
    } else if has(text, DELIVERED) {
        Some(Status::Delivered)
    } else if has(text, ON_ITS_WAY) {
        Some(Status::OnItsWay)
    } else {
        None
    }
}

fn from_carrier(m: &MailItem) -> bool {
    let domain = m
        .from
        .address
        .rsplit('@')
        .next()
        .unwrap_or("")
        .to_lowercase();
    has(&format!("{domain}."), CARRIERS)
}

/// Where the parcel is, when the mail is about one: the subject decides.
pub fn status(m: &MailItem) -> Option<Status> {
    let subject = m.subject.to_lowercase();
    if has(&subject, NOT_A_PARCEL) {
        return None;
    }
    let carrier = from_carrier(m);
    if (carrier || has(&subject, PARCEL_WORDS))
        && let Some(s) = stage(&subject)
    {
        return Some(s);
    }
    // "Levererad: ..." as shops put it, first in the subject.
    if subject.starts_with("levererad:") || subject.starts_with("delivered:") {
        return Some(Status::Delivered);
    }
    (carrier && has(&subject, PARCEL_WORDS)).then_some(Status::OnItsWay)
}

static NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"#\s?\d{3,}|\b[A-Z]{0,4}\d{6,}[A-Z]{0,3}\b").expect("valid"));
static QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("[\"\u{201D}\u{201C}\u{2019}]([^\"\u{201D}\u{201C}\u{2019}]{3,40})").expect("valid")
});

/// What the mails about one parcel share: its number, what is in it, or the
/// conversation.
fn key(s: &Stored) -> String {
    let i = &s.item;
    let sender = i.from.address.rsplit('@').next().unwrap_or("");
    if let Some(m) = NUMBER.find(&i.subject).or_else(|| NUMBER.find(&i.preview)) {
        return m.as_str().replace(['#', ' '], "");
    }
    if let Some(c) = QUOTED.captures(&i.subject) {
        let what: String = c[1].chars().take(14).collect();
        return format!("{sender}|{}", what.to_lowercase());
    }
    format!("{sender}|{}", i.thread_id)
}

/// One parcel: its latest mail says where it is.
pub struct Parcel<'a> {
    pub status: Status,
    /// Newest first.
    pub mails: Vec<&'a Stored>,
}

/// The parcels among some mail, undelivered first, newest first.
pub fn parcels(items: &[Stored]) -> Vec<Parcel<'_>> {
    let mut by: BTreeMap<String, Vec<&Stored>> = BTreeMap::new();
    for s in items {
        if status(&s.item).is_some() {
            by.entry(key(s)).or_default().push(s);
        }
    }
    let mut out: Vec<Parcel<'_>> = by
        .into_values()
        .filter_map(|mut mails| {
            mails.sort_by_key(|s| std::cmp::Reverse(s.item.received_at));
            let status = status(&mails[0].item)?;
            Some(Parcel { status, mails })
        })
        .collect();
    out.sort_by_key(|p| {
        (
            p.status == Status::Delivered,
            std::cmp::Reverse(p.mails[0].item.received_at),
        )
    });
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tests::mail;
    use pidge_core::provider::BulkHint;

    fn at(from: &str, subject: &str) -> Option<Status> {
        status(&mail("1", from, subject, BulkHint::Focused, false, 1))
    }

    #[test]
    fn real_subjects_say_where_the_parcel_is() {
        for (from, subject, want) in [
            (
                "e@fedex.com",
                "Vi har tagit emot din begäran om att behålla din försändelse för upphämtning 123456789012",
                Status::OnItsWay,
            ),
            (
                "a@anker.com",
                "Ditt paket har redan skickats.",
                Status::OnItsWay,
            ),
            (
                "h@socks.example.com",
                "Yay! Your order has been sent!",
                Status::OnItsWay,
            ),
            (
                "service@paypal.se",
                "Mer information om din leverans",
                Status::OnItsWay,
            ),
            (
                "hej@shop.no",
                "Din beställning är skickad",
                Status::OnItsWay,
            ),
            (
                "no@bring.com",
                "Kommande leverans från Exempelbutiken",
                Status::OnItsWay,
            ),
            (
                "t@fedex.com",
                "Din försändelse är på väg 123456789012",
                Status::OnItsWay,
            ),
            (
                "a@amazon.se",
                "Levererad: \u{201D}Exempel laddare,...\u{201D}",
                Status::Delivered,
            ),
            (
                "a@amazon.se",
                "Ute för leverans: \u{201D}Exempel BDXL-1\" och 1 mer objekt",
                Status::OutForDelivery,
            ),
            (
                "g@shop.se",
                "En försändelse för order #1234 är på väg",
                Status::OnItsWay,
            ),
            (
                "no@postnord.se",
                "Ditt paket finns att hämta",
                Status::ToCollect,
            ),
            (
                "t@fedex.com",
                "Your package has been delivered",
                Status::Delivered,
            ),
        ] {
            let got = at(from, subject);
            // "Ute för leverans:" names the goods, not the parcel: the
            // delivery word is in the phrase itself.
            assert_eq!(got, Some(want), "{subject}");
        }
    }

    #[test]
    fn offers_and_bounces_are_not_parcels() {
        for (from, subject) in [
            (
                "noreply@food.example.com",
                "Maten är redo för upphämtning Exempelkrogen",
            ),
            (
                "no@gifts.example.com",
                "Your order #AB1234567 is being processed!",
            ),
            ("news@shop.se", "Fri leverans hela helgen"),
            ("mailer-daemon@x.se", "Mail Delivery Subsystem: undelivered"),
            ("anna@x.se", "Är du på väg?"),
            ("t@fedex.com", "FedEx-kontoverifiering"),
        ] {
            assert_eq!(at(from, subject), None, "{subject}");
        }
    }
}
