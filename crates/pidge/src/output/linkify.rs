//! Scan text for URLs and wrap each with an OSC 8 hyperlink.

use linkify::{LinkFinder, LinkKind};

use crate::output::hyperlink::hyperlink;

/// Find URLs in `text` and wrap each with an OSC 8 link pointing at the URL itself.
/// Text without URLs is returned unchanged. Only HTTP/HTTPS-style URLs are wrapped;
/// email addresses (mailto:) are NOT wrapped.
pub fn linkify_text(text: &str) -> String {
    let finder = LinkFinder::new();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for link in finder.links(text) {
        if !matches!(link.kind(), LinkKind::Url) {
            continue;
        }
        let (start, end) = (link.start(), link.end());
        out.push_str(&text[cursor..start]);
        out.push_str(&hyperlink(link.as_str(), link.as_str()));
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_returns_unchanged() {
        let input = "no urls here, just text";
        assert_eq!(linkify_text(input), input);
    }

    #[test]
    fn single_url_gets_wrapped() {
        let out = linkify_text("see https://example.com today");
        assert!(out.contains("\x1b]8;;https://example.com\x1b\\"));
        assert!(out.contains("see "));
        assert!(out.contains(" today"));
    }

    #[test]
    fn multiple_urls_each_get_wrapped() {
        let out = linkify_text("first https://a.com and second https://b.com end");
        let osc8_count = out.matches("\x1b]8;;").count();
        assert_eq!(osc8_count, 4, "got: {out:?}");
    }

    #[test]
    fn email_addresses_are_not_wrapped() {
        let input = "contact me at hello@example.com";
        let out = linkify_text(input);
        assert!(!out.contains("\x1b]8;;"));
        assert!(out.contains("hello@example.com"));
    }
}
