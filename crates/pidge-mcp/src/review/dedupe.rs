//! Copies of one message become one review item: the same mail delivered
//! twice or to two of the user's mailboxes (same `Message-ID`), a sender
//! sending the same thing again within minutes, or the user forwarding a
//! message to another of their own mailboxes. The user hears it once, and
//! whatever they decide applies to every copy.

use chrono::Duration;

use super::classify::Candidate;

/// Same sender and subject within this long counts as one message.
const WINDOW: Duration = Duration::minutes(10);

/// Reply and forward prefixes, in the languages pidge's users write.
const REPLY: &[&str] = &["re", "sv", "aw", "antw", "vs"];
const FORWARD: &[&str] = &["fw", "fwd", "vb", "wg", "tr", "vl", "doorst"];

/// The subject without reply/forward prefixes, lower-cased, whitespace
/// collapsed; and whether one of the prefixes was a forward.
pub fn normalized_subject(subject: &str) -> (String, bool) {
    let mut s = subject.trim();
    let mut forwarded = false;
    while let Some((head, rest)) = s.split_once(':') {
        let head = head.trim().to_lowercase();
        if FORWARD.contains(&head.as_str()) {
            forwarded = true;
        } else if !REPLY.contains(&head.as_str()) {
            break;
        }
        s = rest.trim_start();
    }
    let norm = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    (norm, forwarded)
}

fn from_me(c: &Candidate, my_addresses: &[String]) -> bool {
    my_addresses
        .iter()
        .any(|m| m.eq_ignore_ascii_case(&c.from_address))
}

/// Whether `a` and `b` are copies of one message.
fn same(a: &Candidate, b: &Candidate, my_addresses: &[String]) -> bool {
    if let (Some(x), Some(y)) = (&a.internet_id, &b.internet_id)
        && x == y
    {
        return true;
    }
    let (sa, fa) = normalized_subject(&a.subject);
    let (sb, fb) = normalized_subject(&b.subject);
    if sa.is_empty() || sa != sb {
        return false;
    }
    let close = (a.received_at - b.received_at).abs() <= WINDOW;
    if close && a.from_address.eq_ignore_ascii_case(&b.from_address) {
        return true;
    }
    // The user's own forward of the other one: sent after it, within
    // [`FORWARD_WINDOW`], and quoting its sender (forwards start with the
    // original's "From:" line), so an unrelated mail that happens to share
    // the subject is not swept in.
    let forward_of = |fwd: &Candidate, orig: &Candidate| {
        let lag = fwd.received_at - orig.received_at;
        let preview = fwd.preview.to_ascii_lowercase();
        lag >= chrono::Duration::zero()
            && lag <= FORWARD_WINDOW
            && !orig.from_address.is_empty()
            && preview.contains(&orig.from_address.to_ascii_lowercase())
    };
    (fa && from_me(a, my_addresses) && !from_me(b, my_addresses) && forward_of(a, b))
        || (fb && from_me(b, my_addresses) && !from_me(a, my_addresses) && forward_of(b, a))
}

/// How long after the original a user's forward is still taken as a copy.
const FORWARD_WINDOW: chrono::Duration = chrono::Duration::days(3);

/// Groups `candidates` (in their order) into `(primary, copies)`. The
/// primary is the first copy not sent by the user (the original, not their
/// forward of it).
pub fn merge(
    candidates: Vec<Candidate>,
    my_addresses: &[String],
) -> Vec<(Candidate, Vec<Candidate>)> {
    let mut groups: Vec<Vec<Candidate>> = Vec::new();
    for c in candidates {
        match groups
            .iter_mut()
            .find(|g| g.iter().any(|m| same(m, &c, my_addresses)))
        {
            Some(g) => g.push(c),
            None => groups.push(vec![c]),
        }
    }
    groups
        .into_iter()
        .map(|mut g| {
            let first = g
                .iter()
                .position(|c| !from_me(c, my_addresses))
                .unwrap_or(0);
            let primary = g.remove(first);
            (primary, g)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn c(id: &str, account: &str, from: &str, subject: &str, minutes_ago: i64) -> Candidate {
        Candidate {
            account: account.into(),
            id: id.into(),
            from_address: from.into(),
            subject: subject.into(),
            received_at: Utc::now() - Duration::minutes(minutes_ago),
            ..Default::default()
        }
    }

    #[test]
    fn prefixes_are_stripped_in_several_languages() {
        assert_eq!(
            normalized_subject("VB: Sv: RE:  Faktura   441 kr"),
            ("faktura 441 kr".into(), true)
        );
        assert_eq!(normalized_subject("Re: Weekend"), ("weekend".into(), false));
        assert_eq!(
            normalized_subject("Meeting: Monday"),
            ("meeting: monday".into(), false)
        );
    }

    #[test]
    fn copies_merge_and_unrelated_mail_does_not() {
        let me = ["jane@example.com".to_string(), "jane@work.example".into()];
        let mut same_id_a = c("a1", "jane@example.com", "x@shop.example", "Order", 50);
        let mut same_id_b = c("b1", "jane@work.example", "x@shop.example", "Your order", 5);
        same_id_a.internet_id = Some("<m1@shop>".into());
        same_id_b.internet_id = Some("<m1@shop>".into());
        let groups = merge(
            vec![
                c("p1", "jane@example.com", "bill@telco.example", "Invoice", 3),
                c("p2", "jane@example.com", "Bill@Telco.example", "Invoice", 9),
                c(
                    "p3",
                    "jane@example.com",
                    "bill@telco.example",
                    "Invoice",
                    40,
                ),
                Candidate {
                    preview: "From: Anna <anna@example.com> Sent: Saturday".into(),
                    ..c(
                        "f1",
                        "jane@work.example",
                        "jane@example.com",
                        "Fwd: Weekend",
                        1,
                    )
                },
                c("o1", "jane@example.com", "anna@example.com", "Weekend", 300),
                // The same subject from someone the forward doesn't quote:
                // unrelated mail, not a copy.
                c("u1", "jane@example.com", "bob@example.com", "Weekend", 200),
                same_id_a,
                same_id_b,
            ],
            &me,
        );
        let shape: Vec<(String, Vec<String>)> = groups
            .iter()
            .map(|(p, cs)| (p.id.clone(), cs.iter().map(|c| c.id.clone()).collect()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("p1".to_string(), vec!["p2".to_string()]),
                // Same sender and subject, but half an hour later: new mail.
                ("p3".to_string(), vec![]),
                // The original is the item; the user's forward is a copy.
                ("o1".to_string(), vec!["f1".to_string()]),
                ("u1".to_string(), vec![]),
                ("a1".to_string(), vec!["b1".to_string()]),
            ]
        );
    }
}
