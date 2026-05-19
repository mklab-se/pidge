//! Parsing of RFC 2369 `List-Unsubscribe` and RFC 8058
//! `List-Unsubscribe-Post` headers — no I/O.
//!
//! See:
//! - <https://www.rfc-editor.org/rfc/rfc2369> (List-Unsubscribe)
//! - <https://www.rfc-editor.org/rfc/rfc8058> (one-click POST)
//! - <https://www.rfc-editor.org/rfc/rfc6068> (mailto: URI)

use url::Url;

/// The opt-out method picked from a message's unsubscribe headers, in
/// preference order: `OneClickPost` → `Mailto` → `HttpsOnly` → `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsubscribeMethod {
    /// RFC 8058 one-click: POST `List-Unsubscribe=One-Click`
    /// (`application/x-www-form-urlencoded`) to this URL. No browser
    /// interaction needed.
    OneClickPost(String),

    /// RFC 2369 `mailto:` — send an e-mail to this address. Per RFC 6068
    /// the URL may carry `?subject=` / `?body=` that override our defaults.
    Mailto {
        address: String,
        subject: Option<String>,
        body: Option<String>,
    },

    /// HTTPS URL exists but no one-click marker. Won't auto-drive; the
    /// caller should surface the URL for a manual click.
    /// HTTPS only — plaintext `http://` entries are ignored on purpose.
    HttpsOnly(String),

    /// No `List-Unsubscribe` header at all.
    None,
}

/// Pick the best `UnsubscribeMethod` for the given message headers.
///
/// Header name comparison is case-insensitive (RFC 5322).
pub fn parse_unsubscribe(headers: &[(String, String)]) -> UnsubscribeMethod {
    let Some(raw) = find_header(headers, "List-Unsubscribe") else {
        return UnsubscribeMethod::None;
    };
    let post = find_header(headers, "List-Unsubscribe-Post");

    let mut https_url: Option<String> = None;
    let mut mailto_entry: Option<(String, Option<String>, Option<String>)> = None;

    for entry in split_entries(raw) {
        if let Some(rest) = entry.strip_prefix("mailto:") {
            if mailto_entry.is_none() {
                mailto_entry = parse_mailto(rest);
            }
        } else if entry.starts_with("https://") && https_url.is_none() {
            https_url = Some(entry.to_string());
        }
    }

    let one_click = post
        .map(|v| v.trim().eq_ignore_ascii_case("List-Unsubscribe=One-Click"))
        .unwrap_or(false);

    match (one_click, https_url, mailto_entry) {
        (true, Some(url), _) => UnsubscribeMethod::OneClickPost(url),
        (_, _, Some((address, subject, body))) => UnsubscribeMethod::Mailto {
            address,
            subject,
            body,
        },
        (_, Some(url), _) => UnsubscribeMethod::HttpsOnly(url),
        _ => UnsubscribeMethod::None,
    }
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Split a comma-separated `List-Unsubscribe` value, respecting `<>` so URLs
/// with commas in their query string survive intact. Strips the brackets.
fn split_entries(raw: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let bytes = raw.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'<' => depth += 1,
            b'>' => depth -= 1,
            b',' if depth == 0 => {
                out.push(strip_brackets(&raw[start..i]));
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < raw.len() {
        out.push(strip_brackets(&raw[start..]));
    }
    out.into_iter().filter(|e| !e.is_empty()).collect()
}

fn strip_brackets(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix('<').unwrap_or(s);
    let s = s.strip_suffix('>').unwrap_or(s);
    s.trim()
}

fn parse_mailto(rest: &str) -> Option<(String, Option<String>, Option<String>)> {
    // Prepend the scheme back and let `url` handle percent-decoding for us.
    let full = format!("mailto:{rest}");
    let url = Url::parse(&full).ok()?;
    if url.scheme() != "mailto" {
        return None;
    }
    let address = url.path().to_string();
    if address.is_empty() {
        return None;
    }
    let mut subject = None;
    let mut body = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "subject" => subject = Some(v.into_owned()),
            "body" => body = Some(v.into_owned()),
            _ => {}
        }
    }
    Some((address, subject, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdr(name: &str, value: &str) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    #[test]
    fn no_header_returns_none() {
        assert_eq!(parse_unsubscribe(&[]), UnsubscribeMethod::None);
    }

    #[test]
    fn only_mailto_picks_mailto() {
        let h = vec![hdr(
            "List-Unsubscribe",
            "<mailto:unsub-abc@news.example.com>",
        )];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::Mailto {
                address: "unsub-abc@news.example.com".into(),
                subject: None,
                body: None,
            }
        );
    }

    #[test]
    fn only_https_without_one_click_returns_https_only() {
        let h = vec![hdr("List-Unsubscribe", "<https://example.com/u?token=abc>")];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::HttpsOnly("https://example.com/u?token=abc".into())
        );
    }

    #[test]
    fn https_with_one_click_picks_post() {
        let h = vec![
            hdr("List-Unsubscribe", "<https://example.com/u?token=abc>"),
            hdr("List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
        ];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::OneClickPost("https://example.com/u?token=abc".into())
        );
    }

    #[test]
    fn both_mailto_and_one_click_prefers_one_click() {
        let h = vec![
            hdr(
                "List-Unsubscribe",
                "<mailto:unsub@example.com>, <https://example.com/u?t=a>",
            ),
            hdr("List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
        ];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::OneClickPost("https://example.com/u?t=a".into())
        );
    }

    #[test]
    fn mailto_with_subject_and_body_query_params() {
        let h = vec![hdr(
            "List-Unsubscribe",
            "<mailto:unsub@example.com?subject=unsub&body=Please%20remove%20me>",
        )];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::Mailto {
                address: "unsub@example.com".into(),
                subject: Some("unsub".into()),
                body: Some("Please remove me".into()),
            }
        );
    }

    #[test]
    fn header_name_is_case_insensitive() {
        let h = vec![
            hdr("list-unsubscribe", "<https://x/u>"),
            hdr("LIST-UNSUBSCRIBE-POST", "List-Unsubscribe=One-Click"),
        ];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::OneClickPost("https://x/u".into())
        );
    }

    #[test]
    fn commas_inside_url_brackets_do_not_split_entries() {
        let h = vec![hdr(
            "List-Unsubscribe",
            "<https://example.com/u?token=a,b,c>",
        )];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::HttpsOnly("https://example.com/u?token=a,b,c".into())
        );
    }

    #[test]
    fn one_click_marker_is_case_insensitive() {
        let h = vec![
            hdr("List-Unsubscribe", "<https://x/u>"),
            hdr("List-Unsubscribe-Post", "list-unsubscribe=one-click"),
        ];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::OneClickPost("https://x/u".into())
        );
    }

    #[test]
    fn mailto_with_no_address_is_rejected() {
        let h = vec![hdr("List-Unsubscribe", "<mailto:>")];
        assert_eq!(parse_unsubscribe(&h), UnsubscribeMethod::None);
    }

    #[test]
    fn malformed_header_with_only_whitespace_is_none() {
        let h = vec![hdr("List-Unsubscribe", "   ")];
        assert_eq!(parse_unsubscribe(&h), UnsubscribeMethod::None);
    }

    #[test]
    fn one_click_marker_without_https_falls_back_to_mailto() {
        // Sender includes `List-Unsubscribe-Post` but only a mailto URL.
        // Per RFC 8058 the marker only applies to HTTPS; we must fall
        // back to the mailto rather than picking nothing.
        let h = vec![
            hdr("List-Unsubscribe", "<mailto:unsub@example.com>"),
            hdr("List-Unsubscribe-Post", "List-Unsubscribe=One-Click"),
        ];
        assert_eq!(
            parse_unsubscribe(&h),
            UnsubscribeMethod::Mailto {
                address: "unsub@example.com".into(),
                subject: None,
                body: None,
            }
        );
    }
}
