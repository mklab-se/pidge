//! Text rendering shared by the CLI and the MCP server: HTML → text and
//! quoted-history stripping. Pure functions, no terminal assumptions.

use html2text::render::{RichAnnotation, TaggedLineElement};

/// How `<a href>` spans are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStyle {
    /// OSC 8 hyperlink escapes around the link text (terminals).
    Osc8,
    /// `text (url)` — for plain-text consumers such as an AI harness.
    Inline,
    /// Link text only.
    Plain,
}

/// Removes control characters from third-party text before it can reach a
/// terminal: everything `char::is_control` matches (C0, DEL, C1) except
/// `\n`, `\r` and `\t`. An e-mail can otherwise carry `ESC ] 8 ; ;` to
/// forge a hyperlink, move the cursor over earlier lines, retitle the
/// window, or write the clipboard. Applied where Graph data becomes
/// `pidge-core` types, and again on rendered HTML (entities decode late).
pub fn strip_controls(text: &str) -> String {
    if !text.chars().any(is_hostile_control) {
        return text.to_string();
    }
    text.chars().filter(|&c| !is_hostile_control(c)).collect()
}

fn is_hostile_control(c: char) -> bool {
    c.is_control() && !matches!(c, '\n' | '\r' | '\t')
}

/// Render an HTML body to text.
///
/// - Uses html2text's `raw_mode` which traverses HTML `<table>` elements as a
///   sequence of paragraphs (every cell becomes its own row, no column layout,
///   no ASCII borders). Marketing emails are almost entirely layout tables; this
///   keeps the reading flow.
/// - Suppresses `<img>` alt-text entirely (no `[[Logo]]` noise from email
///   tracking pixels and logo images).
/// - Folds NBSP to a plain space so NBSP-padded table cells collapse like
///   ordinary blank runs.
/// - Collapses runs of more than two blank lines down to two.
pub fn render_html(html: &str, width: usize, links: LinkStyle) -> String {
    let lines = match html2text::config::rich()
        .raw_mode(true)
        .lines_from_read(html.as_bytes(), width)
    {
        Ok(l) => l,
        Err(_) => return html.to_string(),
    };
    let mut out = String::new();
    for line in lines {
        for elem in line.iter() {
            let TaggedLineElement::Str(ts) = elem else {
                continue;
            };
            let mut url: Option<&str> = None;
            let mut is_image = false;
            for ann in &ts.tag {
                match ann {
                    RichAnnotation::Image(_) => is_image = true,
                    RichAnnotation::Link(u) => url = Some(u.as_str()),
                    _ => {}
                }
            }
            if is_image {
                continue;
            }
            let text = strip_controls(&ts.s).replace('\u{00A0}', " ");
            let url = url.map(strip_controls);
            match (url.as_deref(), links) {
                (Some(u), LinkStyle::Osc8) => {
                    out.push_str("\x1b]8;;");
                    out.push_str(u);
                    out.push_str("\x1b\\");
                    out.push_str(&text);
                    out.push_str("\x1b]8;;\x1b\\");
                }
                (Some(u), LinkStyle::Inline) => {
                    out.push_str(&text);
                    out.push_str(" (");
                    out.push_str(u);
                    out.push(')');
                }
                _ => out.push_str(&text),
            }
        }
        out.push('\n');
    }
    collapse_blank_runs(&out)
}

/// Collapse runs of 3+ blank lines down to 2, and strip tracking-pixel
/// padding characters that marketing emails use to distort preview-pane
/// summaries: zero-width non-joiner (U+200C), zero-width space (U+200B),
/// hair space (U+200A), and combining grapheme joiner (U+034F).
pub fn collapse_blank_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_streak = 0;
    for line in text.lines() {
        let cleaned: String = line
            .chars()
            .filter(|&c| !matches!(c, '\u{200C}' | '\u{200B}' | '\u{200A}' | '\u{034F}'))
            .collect();
        let cleaned = cleaned.trim_end_matches(' ');
        if cleaned.is_empty() {
            blank_streak += 1;
            if blank_streak <= 2 {
                out.push('\n');
            }
        } else {
            blank_streak = 0;
            out.push_str(cleaned);
            out.push('\n');
        }
    }
    out
}

/// Cut a plain-text body at the first sign of quoted history: an Outlook
/// header block (`From:` followed within three lines by `Sent:`/`Subject:`),
/// an `On … wrote:` line, or the first run of `>`-quoted lines.
pub fn strip_quoted_history(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut cut = lines.len();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let is_outlook_header = t.starts_with("From:")
            && lines[i..(i + 4).min(lines.len())].iter().any(|l| {
                l.trim_start().starts_with("Sent:") || l.trim_start().starts_with("Subject:")
            });
        let is_wrote = t.starts_with("On ") && t.trim_end().ends_with("wrote:");
        let is_quote = t.starts_with('>');
        if is_outlook_header || is_wrote || is_quote {
            cut = i;
            break;
        }
    }
    lines[..cut].join("\n").trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_controls_drops_escapes_and_c1_but_keeps_line_structure() {
        assert_eq!(
            strip_controls("a\x1b]8;;https://evil.test\x1b\\b\n\tc\r\n\u{85}\u{7f}d"),
            "a]8;;https://evil.test\\b\n\tc\r\nd"
        );
        assert_eq!(strip_controls("plain åäö"), "plain åäö");
    }

    #[test]
    fn render_html_strips_control_characters_including_decoded_entities() {
        // `&#27;` decodes to ESC only after parsing, so stripping at the
        // Graph boundary would miss it.
        let html = r#"<p>Hi &#27;]8;;https://evil.test&#27;\see&#27;]8;;&#27;\ <a href="https://ok.test/&#27;x">link</a></p>"#;
        for style in [LinkStyle::Plain, LinkStyle::Inline] {
            let out = render_html(html, 80, style);
            assert!(!out.contains('\x1b'), "{out:?}");
        }
        let out = render_html(html, 80, LinkStyle::Osc8);
        assert_eq!(
            out.matches("\x1b]8;;").count(),
            2,
            "only pidge's own link escapes: {out:?}"
        );
        assert!(out.contains("\x1b]8;;https://ok.test/x\x1b\\"), "{out:?}");
    }

    #[test]
    fn inline_links_render_as_text_and_url() {
        let out = render_html(
            r#"<p>See <a href="https://x.test/a">the page</a>.</p>"#,
            80,
            LinkStyle::Inline,
        );
        assert_eq!(out.trim(), "See the page (https://x.test/a).");
    }

    #[test]
    fn plain_links_render_text_only() {
        let out = render_html(
            r#"<p>See <a href="https://x.test/a">the page</a>.</p>"#,
            80,
            LinkStyle::Plain,
        );
        assert_eq!(out.trim(), "See the page.");
    }

    #[test]
    fn osc8_links_wrap_text_in_escape() {
        let out = render_html(r#"<a href="https://x.test/a">go</a>"#, 80, LinkStyle::Osc8);
        assert!(out.contains("\x1b]8;;https://x.test/a\x1b\\"));
        assert!(out.contains("go"));
    }

    #[test]
    fn images_are_suppressed_and_blank_runs_collapse() {
        let out = render_html(
            "<p>a</p><img alt=\"Logo\"><br><br><br><br><p>b</p>",
            80,
            LinkStyle::Plain,
        );
        assert!(!out.contains("Logo"));
        assert!(!out.contains("\n\n\n\n"));
    }

    #[test]
    fn strips_outlook_style_quoted_history() {
        let text = "Thanks, sounds good.\n\nFrom: Jane <jane@example.com>\nSent: Monday\nSubject: Re: x\n\nEarlier text";
        assert_eq!(strip_quoted_history(text), "Thanks, sounds good.");
    }

    #[test]
    fn strips_on_wrote_and_angle_quotes() {
        let text = "Yes.\n\nOn Mon, Jan 1, Jane wrote:\n> old\n> older";
        assert_eq!(strip_quoted_history(text), "Yes.");
        let text2 = "Yes.\n> old\n> older";
        assert_eq!(strip_quoted_history(text2), "Yes.");
    }

    #[test]
    fn keeps_text_without_quotes() {
        assert_eq!(strip_quoted_history("Hello\nworld"), "Hello\nworld");
    }
}
