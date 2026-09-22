//! Cheap facts about a list item that a harness can triage on. No ranking.
use crate::{FlagStatus, Message};

pub struct UserContext<'a> {
    pub my_addresses: &'a [String],
    pub trusted_senders: &'a [String],
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ItemFlags {
    pub to_me: bool,
    pub trusted: bool,
    pub question: bool,
    pub attachments: bool,
    pub flagged: bool,
    pub unread: bool,
}

impl ItemFlags {
    pub fn labels(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.to_me {
            v.push("to-me");
        }
        if self.trusted {
            v.push("trusted");
        }
        if self.question {
            v.push("question");
        }
        if self.attachments {
            v.push("attachments");
        }
        if self.flagged {
            v.push("flagged");
        }
        if self.unread {
            v.push("unread");
        }
        v
    }
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

pub fn compute_flags(m: &Message, ctx: &UserContext) -> ItemFlags {
    let to_me =
        m.to.iter()
            .any(|r| ctx.my_addresses.iter().any(|mine| eq_ci(&r.address, mine)));
    let trusted = ctx
        .trusted_senders
        .iter()
        .any(|t| eq_ci(t, &m.from.address));
    let question = m.preview.contains('?');
    ItemFlags {
        to_me,
        trusted,
        question,
        attachments: m.has_attachments,
        flagged: m.flag_status == FlagStatus::Flagged,
        unread: !m.is_read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BodyContentType, FlagStatus, Message, MessageFrom};
    use chrono::Utc;

    fn msg(from: &str, to: &[&str], cc: &[&str], preview: &str) -> Message {
        let who = |a: &str| MessageFrom {
            name: String::new(),
            address: a.to_string(),
        };
        Message {
            account: "me@example.com".into(),
            id: "1".into(),
            conversation_id: String::new(),
            from: who(from),
            subject: "s".into(),
            received_at: Utc::now(),
            is_read: false,
            preview: preview.into(),
            flag_status: FlagStatus::NotFlagged,
            has_attachments: false,
            body: String::new(),
            body_content_type: BodyContentType::Text,
            to: to.iter().map(|a| who(a)).collect(),
            cc: cc.iter().map(|a| who(a)).collect(),
        }
    }

    #[test]
    fn to_me_requires_to_not_cc() {
        let mine = vec!["me@example.com".to_string()];
        let ctx = UserContext {
            my_addresses: &mine,
            trusted_senders: &[],
        };
        assert!(compute_flags(&msg("a@x", &["Me@Example.com"], &[], ""), &ctx).to_me);
        assert!(!compute_flags(&msg("a@x", &["b@x"], &["me@example.com"], ""), &ctx).to_me);
    }

    #[test]
    fn trusted_and_question() {
        let mine = vec!["me@example.com".to_string()];
        let trusted = vec!["Anna@Example.com".to_string()];
        let ctx = UserContext {
            my_addresses: &mine,
            trusted_senders: &trusted,
        };
        let f = compute_flags(&msg("anna@example.com", &[], &[], "Can you come?"), &ctx);
        assert!(f.trusted && f.question);
        assert_eq!(f.labels(), vec!["trusted", "question", "unread"]);
    }
}
