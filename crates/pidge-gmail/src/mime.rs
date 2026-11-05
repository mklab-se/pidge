//! RFC 5322 in and out: raw Gmail messages parsed with mail-parser, drafts
//! built with mail-builder.

use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE_NO_PAD, URL_SAFE_PAD_INDIFFERENT};
use chrono::{DateTime, Utc};
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mail_builder::headers::message_id::MessageId;
use mail_parser::{HeaderValue, MessageParser, MimeHeaders, PartType};
use pidge_core::provider::{DraftKind, DraftSpec, ProviderError, ProviderResult};
use pidge_core::{BodyContentType, FlagStatus, FullMessage, MessageFrom};

use crate::model::{GmailMessage, addresses_of};

/// Gmail's base64url (padding optional on the way in).
pub(crate) fn decode_b64url(data: &str) -> ProviderResult<Vec<u8>> {
    let cleaned: String = data.chars().filter(|c| !c.is_whitespace()).collect();
    URL_SAFE_PAD_INDIFFERENT
        .decode(cleaned.trim_end_matches('='))
        .or_else(|_| URL_SAFE_PAD_INDIFFERENT.decode(&cleaned))
        .map_err(|e| ProviderError::Other(format!("undecodable data from Gmail: {e}")))
}

pub(crate) fn encode_b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn parse(raw: &[u8]) -> ProviderResult<mail_parser::Message<'_>> {
    MessageParser::default()
        .parse(raw)
        .ok_or_else(|| ProviderError::Other("Gmail returned a message pidge cannot parse".into()))
}

fn addrs(a: Option<&mail_parser::Address<'_>>) -> Vec<MessageFrom> {
    a.map(addresses_of).unwrap_or_default()
}

/// The message's body: its HTML part when it has one, else its text.
fn body_of(msg: &mail_parser::Message<'_>) -> (BodyContentType, String) {
    let has_html = msg
        .html_body
        .first()
        .and_then(|&i| msg.parts.get(i as usize))
        .is_some_and(|p| matches!(p.body, PartType::Html(_)));
    if has_html && let Some(html) = msg.body_html(0) {
        return (BodyContentType::Html, html.into_owned());
    }
    (BodyContentType::Text, text_of(msg))
}

/// The message's text, converted from HTML when it has no text part.
fn text_of(msg: &mail_parser::Message<'_>) -> String {
    msg.body_text(0)
        .map(|t| t.replace("\r\n", "\n"))
        .unwrap_or_default()
}

/// A `format=raw` message as a [`FullMessage`]. Labels give read, starred
/// and draft state; `internalDate` the received time.
pub(crate) fn full_message(account: &str, m: &GmailMessage) -> ProviderResult<FullMessage> {
    let raw = decode_b64url(m.raw.as_deref().unwrap_or(""))?;
    let msg = parse(&raw)?;
    let received_at = m.received_at();
    let sent_at = msg
        .date()
        .and_then(|d| DateTime::<Utc>::from_timestamp(d.to_timestamp(), 0))
        .unwrap_or(received_at);
    let (body_content_type, body_content) = body_of(&msg);
    let is_invite = msg
        .parts
        .iter()
        .any(|p| p.is_content_type("text", "calendar"));
    Ok(FullMessage {
        account: account.to_string(),
        id: m.id.clone(),
        conversation_id: m.thread_id.clone(),
        from: addrs(msg.from()).into_iter().next().unwrap_or(MessageFrom {
            name: String::new(),
            address: String::new(),
        }),
        to: addrs(msg.to()),
        cc: addrs(msg.cc()),
        bcc: addrs(msg.bcc()),
        subject: msg.subject().unwrap_or("").trim().to_string(),
        received_at,
        sent_at,
        is_read: !m.has_label("UNREAD"),
        body_content_type,
        body_content,
        has_attachments: msg.attachment_count() > 0,
        flag_status: if m.has_label("STARRED") {
            FlagStatus::Flagged
        } else {
            FlagStatus::NotFlagged
        },
        is_invite,
        event_id: None,
        is_draft: m.has_label("DRAFT"),
    })
}

/// What a reply or forward needs from the message it answers.
#[derive(Debug, Clone, Default)]
pub(crate) struct Original {
    pub thread_id: String,
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub subject: String,
    pub from: Vec<MessageFrom>,
    pub reply_to: Vec<MessageFrom>,
    pub to: Vec<MessageFrom>,
    pub cc: Vec<MessageFrom>,
    /// The `Date` header as written (RFC 822 form).
    pub date: Option<String>,
    pub text: String,
}

fn id_list(v: &HeaderValue<'_>) -> Vec<String> {
    match v {
        HeaderValue::Text(t) => vec![t.to_string()],
        HeaderValue::TextList(l) => l.iter().map(|t| t.to_string()).collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn original(m: &GmailMessage) -> ProviderResult<Original> {
    let raw = decode_b64url(m.raw.as_deref().unwrap_or(""))?;
    let msg = parse(&raw)?;
    Ok(Original {
        thread_id: m.thread_id.clone(),
        message_id: msg.message_id().map(str::to_string),
        references: id_list(msg.references()),
        subject: msg.subject().unwrap_or("").trim().to_string(),
        from: addrs(msg.from()),
        reply_to: addrs(msg.reply_to()),
        to: addrs(msg.to()),
        cc: addrs(msg.cc()),
        date: msg.date().map(|d| d.to_rfc822()),
        text: text_of(&msg),
    })
}

/// A draft as stored (from `drafts.get?format=raw`).
pub(crate) struct ParsedDraft {
    pub to: Vec<MessageFrom>,
    pub cc: Vec<MessageFrom>,
    pub bcc: Vec<MessageFrom>,
    pub subject: String,
    pub body_text: String,
    pub has_attachments: bool,
}

pub(crate) fn parse_draft(raw_b64: &str) -> ProviderResult<ParsedDraft> {
    let raw = decode_b64url(raw_b64)?;
    let msg = parse(&raw)?;
    Ok(ParsedDraft {
        to: addrs(msg.to()),
        cc: addrs(msg.cc()),
        bcc: addrs(msg.bcc()),
        subject: msg.subject().unwrap_or("").trim().to_string(),
        body_text: text_of(&msg),
        has_attachments: msg.attachment_count() > 0,
    })
}

/// A draft ready to upload.
#[derive(Debug, Clone)]
pub(crate) struct BuiltDraft {
    /// The RFC 5322 message.
    pub raw: String,
    pub thread_id: Option<String>,
    pub to: Vec<MessageFrom>,
    pub cc: Vec<MessageFrom>,
    pub bcc: Vec<MessageFrom>,
    pub subject: String,
    pub body_text: String,
}

fn has_prefix(subject: &str, prefixes: &[&str]) -> bool {
    let lower = subject.trim_start().to_ascii_lowercase();
    prefixes.iter().any(|p| lower.starts_with(p))
}

fn reply_subject(s: &str) -> String {
    if has_prefix(s, &["re:"]) {
        s.to_string()
    } else {
        format!("Re: {s}")
    }
}

fn forward_subject(s: &str) -> String {
    if has_prefix(s, &["fwd:", "fw:"]) {
        s.to_string()
    } else {
        format!("Fwd: {s}")
    }
}

fn display(a: &MessageFrom) -> String {
    if a.name.is_empty() {
        a.address.clone()
    } else {
        format!("{} <{}>", a.name, a.address)
    }
}

fn display_list(list: &[MessageFrom]) -> String {
    list.iter().map(display).collect::<Vec<_>>().join(", ")
}

/// Append `extra` to `list`, skipping addresses already present (or `me`).
fn push_unique(list: &mut Vec<MessageFrom>, extra: &[MessageFrom], me: &str) {
    for a in extra {
        let taken = a.address.eq_ignore_ascii_case(me)
            || list
                .iter()
                .any(|b| b.address.eq_ignore_ascii_case(&a.address));
        if !taken && !a.address.is_empty() {
            list.push(a.clone());
        }
    }
}

fn quote(text: &str) -> String {
    text.trim_end()
        .lines()
        .map(|l| {
            if l.is_empty() {
                ">".to_string()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn to_builder_list(list: &[MessageFrom]) -> Address<'static> {
    Address::new_list(
        list.iter()
            .map(|a| {
                let name = (!a.name.is_empty()).then(|| a.name.clone());
                Address::new_address(name, a.address.clone())
            })
            .collect(),
    )
}

/// Build the draft for `spec`, sent as `me`. `orig` is the message a reply
/// or forward answers (required for those kinds).
pub(crate) fn build_draft(
    me: &str,
    spec: &DraftSpec,
    orig: Option<&Original>,
) -> ProviderResult<BuiltDraft> {
    let need_orig =
        || orig.ok_or_else(|| ProviderError::Invalid("the original message is required".into()));
    let mut to: Vec<MessageFrom> = Vec::new();
    let mut cc: Vec<MessageFrom> = Vec::new();
    let mut bcc: Vec<MessageFrom> = Vec::new();
    let body = spec.body.trim_end().to_string();
    let (subject, body_text, thread_id) = match &spec.kind {
        DraftKind::New => (spec.subject.clone().unwrap_or_default(), body.clone(), None),
        DraftKind::Reply { .. } | DraftKind::ReplyAll { .. } => {
            let o = need_orig()?;
            let from_me = o.from.iter().any(|a| a.address.eq_ignore_ascii_case(me));
            let base: &[MessageFrom] = if !o.reply_to.is_empty() {
                &o.reply_to
            } else if from_me {
                // Replying to one's own sent message goes to its recipients.
                &o.to
            } else {
                &o.from
            };
            push_unique(&mut to, base, me);
            if matches!(spec.kind, DraftKind::ReplyAll { .. }) {
                push_unique(&mut to, &o.to, me);
                let mut seen = to.clone();
                for a in &o.cc {
                    if !a.address.eq_ignore_ascii_case(me)
                        && !seen
                            .iter()
                            .any(|b| b.address.eq_ignore_ascii_case(&a.address))
                    {
                        cc.push(a.clone());
                        seen.push(a.clone());
                    }
                }
            }
            let who = o.from.first().map(display).unwrap_or_default();
            let intro = match &o.date {
                Some(d) => format!("On {d}, {who} wrote:"),
                None => format!("{who} wrote:"),
            };
            let text = format!("{body}\n\n{intro}\n{}\n", quote(&o.text));
            (reply_subject(&o.subject), text, Some(o.thread_id.clone()))
        }
        DraftKind::Forward { .. } => {
            let o = need_orig()?;
            let mut header = String::from("---------- Forwarded message ---------\n");
            header.push_str(&format!("From: {}\n", display_list(&o.from)));
            if let Some(d) = &o.date {
                header.push_str(&format!("Date: {d}\n"));
            }
            header.push_str(&format!("Subject: {}\n", o.subject));
            if !o.to.is_empty() {
                header.push_str(&format!("To: {}\n", display_list(&o.to)));
            }
            if !o.cc.is_empty() {
                header.push_str(&format!("Cc: {}\n", display_list(&o.cc)));
            }
            let text = format!("{body}\n\n{header}\n{}\n", o.text.trim_end());
            (forward_subject(&o.subject), text, Some(o.thread_id.clone()))
        }
    };
    push_unique(&mut to, &spec.to, "");
    let mut taken = to.clone();
    for a in &spec.cc {
        if !taken
            .iter()
            .any(|b| b.address.eq_ignore_ascii_case(&a.address))
        {
            cc.push(a.clone());
            taken.push(a.clone());
        }
    }
    push_unique(&mut bcc, &spec.bcc, "");
    let thread_id = thread_id.filter(|t| !t.is_empty());

    let mut b = MessageBuilder::new()
        .from(Address::new_address(None::<String>, me.to_string()))
        .subject(subject.clone())
        .text_body(body_text.clone());
    if !to.is_empty() {
        b = b.to(to_builder_list(&to));
    }
    if !cc.is_empty() {
        b = b.cc(to_builder_list(&cc));
    }
    if !bcc.is_empty() {
        b = b.bcc(to_builder_list(&bcc));
    }
    if let (DraftKind::Reply { .. } | DraftKind::ReplyAll { .. }, Some(o)) = (&spec.kind, orig)
        && let Some(mid) = &o.message_id
    {
        let mut refs = o.references.clone();
        if !refs.contains(mid) {
            refs.push(mid.clone());
        }
        b = b
            .in_reply_to(MessageId::new(mid.clone()))
            .references(MessageId::new_list(refs.into_iter()));
    }
    let raw = b
        .write_to_string()
        .map_err(|e| ProviderError::Other(format!("could not build the draft: {e}")))?;
    Ok(BuiltDraft {
        raw,
        thread_id,
        to,
        cc,
        bcc,
        subject,
        body_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: &str, a: &str) -> MessageFrom {
        MessageFrom {
            name: name.into(),
            address: a.into(),
        }
    }

    fn orig() -> Original {
        Original {
            thread_id: "t1".into(),
            message_id: Some("abc@mail.example.com".into()),
            references: vec!["root@mail.example.com".into()],
            subject: "Lunch".into(),
            from: vec![addr("Bob", "bob@example.com")],
            reply_to: vec![],
            to: vec![
                addr("Jane", "jane@example.com"),
                addr("Carl", "carl@example.com"),
            ],
            cc: vec![addr("", "dana@example.com"), addr("", "JANE@example.com")],
            date: Some("Thu, 05 Nov 2026 10:00:00 +0100".into()),
            text: "Noon?\n\nB".into(),
        }
    }

    fn spec(kind: DraftKind) -> DraftSpec {
        DraftSpec {
            kind,
            to: vec![],
            cc: vec![],
            bcc: vec![],
            subject: None,
            body: "Sure.".into(),
        }
    }

    #[test]
    fn reply_all_drops_self_and_quotes() {
        let d = build_draft(
            "jane@example.com",
            &spec(DraftKind::ReplyAll { to_id: "m".into() }),
            Some(&orig()),
        )
        .unwrap();
        let to: Vec<_> = d.to.iter().map(|a| a.address.as_str()).collect();
        assert_eq!(to, ["bob@example.com", "carl@example.com"]);
        let cc: Vec<_> = d.cc.iter().map(|a| a.address.as_str()).collect();
        assert_eq!(cc, ["dana@example.com"]);
        assert_eq!(d.subject, "Re: Lunch");
        assert_eq!(d.thread_id.as_deref(), Some("t1"));
        assert!(d.body_text.starts_with("Sure.\n\nOn Thu, 05 Nov 2026"));
        assert!(
            d.body_text
                .contains("Bob <bob@example.com> wrote:\n> Noon?\n>\n> B")
        );
        assert!(d.raw.contains("In-Reply-To: <abc@mail.example.com>"));
        assert!(d.raw.contains("<root@mail.example.com>"));
    }

    #[test]
    fn reply_does_not_double_the_prefix_and_honours_reply_to() {
        let mut o = orig();
        o.subject = "RE: Lunch".into();
        o.reply_to = vec![addr("", "list@example.com")];
        let d = build_draft(
            "jane@example.com",
            &spec(DraftKind::Reply { to_id: "m".into() }),
            Some(&o),
        )
        .unwrap();
        assert_eq!(d.subject, "RE: Lunch");
        assert_eq!(d.to, vec![addr("", "list@example.com")]);
        assert!(d.cc.is_empty());
    }

    #[test]
    fn forward_has_a_header_block_and_no_threading_headers() {
        let mut s = spec(DraftKind::Forward { to_id: "m".into() });
        s.to = vec![addr("Eve", "eve@example.com")];
        let d = build_draft("jane@example.com", &s, Some(&orig())).unwrap();
        assert_eq!(d.subject, "Fwd: Lunch");
        assert!(
            d.body_text
                .contains("---------- Forwarded message ---------\nFrom: Bob <bob@example.com>")
        );
        assert!(!d.raw.contains("In-Reply-To"));
        assert_eq!(d.to, vec![addr("Eve", "eve@example.com")]);
    }

    #[test]
    fn new_mail_round_trips_through_the_parser() {
        let mut s = spec(DraftKind::New);
        s.subject = Some("Hej d\u{e5}".into());
        s.to = vec![addr("Bob", "bob@example.com")];
        s.bcc = vec![addr("", "hidden@example.com")];
        s.body = "Line one\n\nL\u{e4}ne two".into();
        let d = build_draft("jane@example.com", &s, None).unwrap();
        let p = parse_draft(&encode_b64url(d.raw.as_bytes())).unwrap();
        assert_eq!(p.subject, "Hej d\u{e5}");
        assert_eq!(p.to, vec![addr("Bob", "bob@example.com")]);
        assert_eq!(p.bcc, vec![addr("", "hidden@example.com")]);
        assert_eq!(p.body_text.trim_end(), "Line one\n\nL\u{e4}ne two");
        assert!(
            d.raw.contains("From: <jane@example.com>") || d.raw.contains("From: jane@example.com")
        );
    }

    #[test]
    fn reply_needs_the_original() {
        assert!(matches!(
            build_draft(
                "jane@example.com",
                &spec(DraftKind::Reply { to_id: "m".into() }),
                None
            ),
            Err(ProviderError::Invalid(_))
        ));
    }
}
