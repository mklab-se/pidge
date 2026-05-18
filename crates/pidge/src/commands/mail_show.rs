//! `pidge mail show <fragment>` — display a single message with full body.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Local, Utc};
use colored::Colorize;
use comfy_table::{ContentArrangement, Table};
use serde::Serialize;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{
    Attachment, BodyContentType, CacheLookup, Config, FullMessage, MessageCache, MessageFrom,
};

use crate::output::linkify_text;

pub async fn run(
    fragment: String,
    mark_read: bool,
    show_images: bool,
    raw_html: bool,
    json: bool,
) -> Result<()> {
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge account add` to add one."
        ));
    }

    // Look up the fragment in the cache.
    let cache = MessageCache::load()?;
    let (short_hash, message_ref) = match cache.find_by_fragment(&fragment) {
        CacheLookup::NotFound => {
            return Err(anyhow!(
                "No message found for fragment '{fragment}'. Run `pidge mail list` to refresh the cache."
            ));
        }
        CacheLookup::Ambiguous(matches) => {
            print_ambiguous(&matches);
            return Err(anyhow!("Please provide more characters."));
        }
        CacheLookup::One(h, r) => (h, r),
    };

    // Fetch full message from Graph.
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let full = match graph
        .get_message(&message_ref.account, &message_ref.graph_id)
        .await
    {
        Ok(m) => m,
        Err(ClientError::Graph { status: 404, .. }) => {
            // Purge stale cache entry.
            purge_from_cache(&short_hash)?;
            return Err(anyhow!(
                "Message not found on server. It may have been deleted. Run `pidge mail list` to refresh."
            ));
        }
        Err(e) => return Err(e.into()),
    };

    // Fetch attachments if the message has any. Non-fatal if it fails.
    let attachments: Vec<Attachment> = if full.has_attachments {
        match graph.list_attachments(&message_ref.account, &full.id).await {
            Ok(atts) => atts,
            Err(e) => {
                eprintln!(
                    "{} could not list attachments: {e}",
                    "WARNING:".yellow().bold()
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // Debug: dump the raw body and stop. Used to capture fixtures for the
    // HTML-rendering test suite (see crates/pidge/tests/render_html.rs).
    if raw_html {
        print!("{}", full.body_content);
        return Ok(());
    }

    // Decide whether to render inline images (trusted sender OR --show-images flag).
    let render_inline = config.is_sender_trusted(&full.from.address) || show_images;

    // Render.
    if json {
        render_json(&short_hash, &full, &attachments)?;
    } else {
        render_header_and_body(&full)?;
        if render_inline {
            render_inline_images_block(&graph, &message_ref.account, &full, &attachments).await;
        }
        render_attachments_block(&attachments)?;
    }

    // Optional: mark as read.
    if mark_read {
        if let Err(e) = graph.mark_read(&message_ref.account, &full.id).await {
            eprintln!(
                "{} could not mark message as read: {e}",
                "WARNING:".yellow().bold()
            );
        }
    }

    Ok(())
}

fn purge_from_cache(short_hash: &str) -> Result<()> {
    let mut cache = MessageCache::load()?;
    cache.entries.remove(short_hash);
    cache.save()?;
    Ok(())
}

fn print_ambiguous(matches: &[(String, pidge_core::CachedMessageRef)]) {
    println!("Fragment matches multiple messages:");
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec!["ID", "ACCOUNT", "GRAPH ID"]);
    for (hash, r) in matches {
        table.add_row(vec![
            hash.dimmed().to_string(),
            r.account.clone(),
            r.graph_id.chars().take(20).collect::<String>() + "…",
        ]);
    }
    println!("{table}");
}

fn render_header_and_body(full: &FullMessage) -> Result<()> {
    // Header block
    println!("{}      {}", "From:".bold(), format_recipient(&full.from));
    if !full.to.is_empty() {
        println!(
            "{}        {}",
            "To:".bold(),
            format_recipient_list(&full.to)
        );
    }
    if !full.cc.is_empty() {
        println!(
            "{}        {}",
            "Cc:".bold(),
            format_recipient_list(&full.cc)
        );
    }
    println!(
        "{}   {}{}",
        "Subject:".bold(),
        crate::commands::mail::flag_marker(full.flag_status),
        full.subject.bold().bright_yellow()
    );
    println!(
        "{}  {} ({})",
        "Received:".bold(),
        format_local_datetime(full.received_at),
        relative_time(full.received_at),
    );
    println!();
    println!("{}", separator());
    println!();

    // `render_body` returns a string that already has OSC 8 hyperlinks for HTML
    // bodies. For plain-text bodies it returns raw text; we run `linkify_text` to
    // detect bare URLs there. We never double-linkify HTML output (that would wrap
    // the URL inside the existing OSC 8 open sequence and produce nested escapes).
    let body_text = render_body(full);
    let body_out = match full.body_content_type {
        BodyContentType::Html => body_text,
        BodyContentType::Text => linkify_text(&body_text),
    };
    println!("{}", body_out);
    Ok(())
}

fn render_attachments_block(attachments: &[Attachment]) -> Result<()> {
    let visible_attachments: Vec<&Attachment> =
        attachments.iter().filter(|a| !a.is_inline).collect();
    if visible_attachments.is_empty() {
        return Ok(());
    }
    println!();
    println!("{}", separator());
    println!();
    println!("{}", "Attachments:".bold());
    let mut table = Table::new();
    table.load_preset(comfy_table::presets::NOTHING);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    for att in visible_attachments {
        table.add_row(vec![
            format!("  {}", att.name),
            humansize::format_size(att.size_bytes, humansize::DECIMAL),
        ]);
    }
    println!("{table}");
    Ok(())
}

async fn render_inline_images_block(
    graph: &GraphClient,
    account: &str,
    full: &FullMessage,
    attachments: &[Attachment],
) {
    let inline_images: Vec<&Attachment> = attachments
        .iter()
        .filter(|a| a.is_inline && is_image_content_type(&a.content_type))
        .collect();
    if inline_images.is_empty() {
        return;
    }
    println!();
    println!("{}", separator());
    println!();
    println!("{}", "Inline images:".bold());

    for att in inline_images {
        match graph.get_attachment_bytes(account, &full.id, &att.id).await {
            Ok(bytes) => {
                if !try_render_image(&bytes) {
                    println!(
                        "  [image: {} ({})] (terminal does not support inline images)",
                        att.name,
                        humansize::format_size(att.size_bytes, humansize::DECIMAL)
                    );
                }
            }
            Err(e) => {
                eprintln!("  [image: {} — fetch failed: {e}]", att.name);
            }
        }
    }
}

fn try_render_image(bytes: &[u8]) -> bool {
    let img = match image::load_from_memory(bytes) {
        Ok(i) => i,
        Err(_) => return false,
    };
    let conf = viuer::Config {
        absolute_offset: false,
        width: Some(60),
        ..Default::default()
    };
    viuer::print(&img, &conf).is_ok()
}

fn is_image_content_type(ct: &str) -> bool {
    let ct = ct.to_lowercase();
    matches!(
        ct.as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/webp" | "image/gif"
    )
}

fn render_body(full: &FullMessage) -> String {
    match full.body_content_type {
        BodyContentType::Text => full.body_content.clone(),
        BodyContentType::Html => {
            let width = terminal_width().min(100);
            render_html_body(&full.body_content, width)
        }
    }
}

/// Render an HTML body into a terminal-friendly string.
///
/// - Uses html2text's `raw_mode` which traverses HTML `<table>` elements as a
///   sequence of paragraphs (every cell becomes its own row, no column layout,
///   no ASCII borders). Marketing emails are almost entirely layout tables; this
///   keeps the reading flow.
/// - Wraps every `<a href="…">text</a>` span in an OSC 8 escape sequence so
///   modern terminals make link text clickable. No reference-style footnotes.
/// - Suppresses `<img>` alt-text entirely (no `[[Logo]]` noise from email
///   tracking pixels and logo images).
fn render_html_body(html: &str, width: usize) -> String {
    use html2text::render::text_renderer::{RichAnnotation, TaggedLineElement};

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
            if let Some(u) = url {
                // OSC 8 makes terminals like Ghostty / iTerm2 recognize the span as
                // a hyperlink, but the modifier-click affordance is only obvious when
                // the cursor is exactly over the link text. Style the visible text
                // with underline + cyan so it's identifiable at a glance without
                // hovering. `colored` respects `--no-color` / NO_COLOR, so plain
                // pipelines stay clean.
                let styled = ts.s.cyan().underline().to_string();
                out.push_str("\x1b]8;;");
                out.push_str(u);
                out.push_str("\x1b\\");
                out.push_str(&styled);
                out.push_str("\x1b]8;;\x1b\\");
            } else {
                out.push_str(&ts.s);
            }
        }
        out.push('\n');
    }
    // Collapse runs of 3+ blank lines down to 2 — raw_mode + flattened tables
    // can leave large gaps that look worse than the original layout would.
    collapse_blank_runs(&out)
}

fn collapse_blank_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_streak = 0;
    for line in text.lines() {
        // Strip tracking-pixel padding characters that marketing emails use to
        // distort preview-pane summaries: zero-width non-joiner (U+200C), zero-
        // width space (U+200B), hair space (U+200A), and combining grapheme
        // joiner (U+034F). Then strip trailing ASCII spaces.
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

fn terminal_width() -> usize {
    use std::process::Command;
    if let Ok(out) = Command::new("tput").arg("cols").output() {
        if let Ok(s) = std::str::from_utf8(&out.stdout) {
            if let Ok(n) = s.trim().parse::<usize>() {
                return n;
            }
        }
    }
    80
}

fn separator() -> String {
    "─".repeat(60).dimmed().to_string()
}

fn format_recipient(r: &MessageFrom) -> String {
    if r.name.is_empty() {
        r.address.clone()
    } else {
        format!("{} <{}>", r.name, r.address)
    }
}

fn format_recipient_list(rs: &[MessageFrom]) -> String {
    rs.iter()
        .map(format_recipient)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_local_datetime(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn relative_time(then: DateTime<Utc>) -> String {
    let now = Utc::now();
    let delta = now - then;
    if delta.num_minutes() < 60 {
        format!("{}m ago", delta.num_minutes().max(0))
    } else if delta.num_hours() < 24 {
        format!("{}h ago", delta.num_hours())
    } else if delta.num_days() < 7 {
        format!("{}d ago", delta.num_days())
    } else {
        format_local_datetime(then)
    }
}

#[derive(Serialize)]
struct ShowOut<'a> {
    id: &'a str,
    graph_id: &'a str,
    account: &'a str,
    from: &'a MessageFrom,
    to: &'a [MessageFrom],
    cc: &'a [MessageFrom],
    bcc: &'a [MessageFrom],
    subject: &'a str,
    received_at: DateTime<Utc>,
    sent_at: DateTime<Utc>,
    is_read: bool,
    body: BodyOut<'a>,
    has_attachments: bool,
    attachments: &'a [Attachment],
}

#[derive(Serialize)]
struct BodyOut<'a> {
    content_type: BodyContentType,
    html: Option<&'a str>,
    text: String,
}

fn render_json(short_hash: &str, full: &FullMessage, attachments: &[Attachment]) -> Result<()> {
    let body_text = render_body(full);
    let body = BodyOut {
        content_type: full.body_content_type,
        html: if matches!(full.body_content_type, BodyContentType::Html) {
            Some(full.body_content.as_str())
        } else {
            None
        },
        text: body_text,
    };
    let out = ShowOut {
        id: short_hash,
        graph_id: &full.id,
        account: &full.account,
        from: &full.from,
        to: &full.to,
        cc: &full.cc,
        bcc: &full.bcc,
        subject: &full.subject,
        received_at: full.received_at,
        sent_at: full.sent_at,
        is_read: full.is_read,
        body,
        has_attachments: full.has_attachments,
        attachments,
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_content_type_recognizes_common_image_types() {
        assert!(is_image_content_type("image/png"));
        assert!(is_image_content_type("image/jpeg"));
        assert!(is_image_content_type("image/jpg"));
        assert!(is_image_content_type("image/webp"));
        assert!(is_image_content_type("image/gif"));
        assert!(is_image_content_type("IMAGE/PNG"));
    }

    #[test]
    fn is_image_content_type_rejects_non_image_types() {
        assert!(!is_image_content_type("application/pdf"));
        assert!(!is_image_content_type("image/svg+xml"));
        assert!(!is_image_content_type("text/html"));
        assert!(!is_image_content_type(""));
    }

    // --- HTML body rendering snapshot tests ----------------------------------
    //
    // The fixtures in `tests/fixtures/*.html` are anonymized captures of real
    // marketing-style emails (LinkedIn jobs digest, SpeedLedger newsletter)
    // that previously rendered badly. They live in the repo so we can iterate
    // on the renderer without hitting Microsoft Graph and without my keychain
    // weighing in. Every time a real-world email renders poorly, the workflow
    // is:
    //
    //   1. `pidge mail show <fragment> --raw-html > tests/fixtures/raw/<name>.html`
    //   2. Anonymize the raw file into `tests/fixtures/<name>.html` (Lorem
    //      Ipsum text, example.com URLs, structure preserved).
    //   3. Add a test case here that asserts the desired rendering.
    //   4. Iterate on `render_html_body` until the test passes.
    //
    // Snapshots are stored as `tests/fixtures/<name>.expected.txt`. OSC 8
    // hyperlink escapes are encoded as the human-readable form
    // `[link=URL]visible text[/link]` so snapshot diffs stay legible.
    //
    // To accept a new rendered output as the snapshot, run:
    //     UPDATE_SNAPSHOTS=1 cargo test -p pidge render_html_
    //
    // — make sure to review the resulting diff before committing.

    const LINKEDIN_HTML: &str = include_str!("../../tests/fixtures/linkedin_jobs_digest.html");
    const SPEEDLEDGER_HTML: &str = include_str!("../../tests/fixtures/speedledger_newsletter.html");

    const LINKEDIN_SNAPSHOT: &str = "tests/fixtures/linkedin_jobs_digest.expected.txt";
    const SPEEDLEDGER_SNAPSHOT: &str = "tests/fixtures/speedledger_newsletter.expected.txt";

    /// Translate OSC 8 escape sequences emitted by `render_html_body` into the
    /// snapshot-friendly form `[link=URL]text[/link]` and strip ANSI SGR codes
    /// (color / underline / etc.) so snapshots stay readable across `colored`
    /// crate version bumps.
    ///
    /// OSC 8 sequences are all-ASCII (`ESC ] 8 ; ; URL ESC \`); SGR sequences
    /// are `ESC [ params m`. Both are byte-safe to scan around — multi-byte
    /// UTF-8 chars like `ö` are preserved unchanged in the non-escape gaps.
    fn osc8_to_visible(input: &str) -> String {
        const OPEN: &str = "\x1b]8;;";
        const ST: &str = "\x1b\\";
        const CSI: &str = "\x1b[";

        let mut out = String::with_capacity(input.len());
        let mut rest = input;

        loop {
            // Find the next escape sequence start (OSC 8 or CSI).
            let next_osc = rest.find(OPEN);
            let next_csi = rest.find(CSI);
            let (pos, kind) = match (next_osc, next_csi) {
                (Some(o), Some(c)) if o < c => (o, EscKind::Osc8),
                (Some(_), Some(c)) => (c, EscKind::Csi),
                (Some(o), None) => (o, EscKind::Osc8),
                (None, Some(c)) => (c, EscKind::Csi),
                (None, None) => {
                    out.push_str(rest);
                    return out;
                }
            };
            out.push_str(&rest[..pos]);
            match kind {
                EscKind::Osc8 => {
                    let after_open = &rest[pos + OPEN.len()..];
                    let st_pos = match after_open.find(ST) {
                        Some(p) => p,
                        None => {
                            out.push_str(after_open);
                            return out;
                        }
                    };
                    let url = &after_open[..st_pos];
                    if url.is_empty() {
                        out.push_str("[/link]");
                    } else {
                        out.push_str("[link=");
                        out.push_str(url);
                        out.push(']');
                    }
                    rest = &after_open[st_pos + ST.len()..];
                }
                EscKind::Csi => {
                    // ESC [ digits-and-semicolons m → drop entirely. If the
                    // pattern doesn't terminate with 'm' (some other CSI),
                    // skip just the ESC [ and keep walking — we don't emit
                    // any of those today, so any unknown CSI is a bug worth
                    // showing in the snapshot.
                    let after = &rest[pos + CSI.len()..];
                    let end_byte = after.bytes().position(|b| b == b'm');
                    match end_byte {
                        Some(e) if after[..e].bytes().all(|b| b.is_ascii_digit() || b == b';') => {
                            rest = &after[e + 1..];
                        }
                        _ => {
                            // Unknown CSI — fall through, keep `\x1b[` visible
                            // so the snapshot fails loudly.
                            out.push_str(CSI);
                            rest = after;
                        }
                    }
                }
            }
        }
    }

    enum EscKind {
        Osc8,
        Csi,
    }

    /// Compare `actual` against the snapshot file at `relative_path`
    /// (relative to the crate root). If `UPDATE_SNAPSHOTS=1` is set, the
    /// snapshot is overwritten with `actual`. Otherwise mismatch panics with
    /// a short diff.
    fn assert_snapshot(actual: &str, relative_path: &str) {
        let crate_dir = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(crate_dir).join(relative_path);

        if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, actual).unwrap();
            return;
        }

        let expected = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => panic!(
                "snapshot {} missing ({e}). Create it with UPDATE_SNAPSHOTS=1 cargo test.",
                path.display()
            ),
        };

        if actual == expected {
            return;
        }

        // Build a compact line-by-line diff (cap output to keep panic readable).
        let mut diff = String::new();
        diff.push_str("snapshot mismatch — first differing lines:\n");
        let actual_lines: Vec<&str> = actual.lines().collect();
        let expected_lines: Vec<&str> = expected.lines().collect();
        let mut shown = 0;
        for (i, (a, e)) in actual_lines.iter().zip(expected_lines.iter()).enumerate() {
            if a != e {
                diff.push_str(&format!("  line {i} (expected): {e}\n"));
                diff.push_str(&format!("  line {i} (actual)  : {a}\n"));
                shown += 1;
                if shown >= 5 {
                    break;
                }
            }
        }
        if actual_lines.len() != expected_lines.len() {
            diff.push_str(&format!(
                "  length differs: expected={} actual={}\n",
                expected_lines.len(),
                actual_lines.len(),
            ));
        }
        diff.push_str(
            "Run `UPDATE_SNAPSHOTS=1 cargo test -p pidge render_html_` to accept the new output.\n",
        );
        panic!("{diff}");
    }

    #[test]
    fn render_html_linkedin_matches_snapshot() {
        let rendered = render_html_body(LINKEDIN_HTML, 100);
        let visible = osc8_to_visible(&rendered);
        assert_snapshot(&visible, LINKEDIN_SNAPSHOT);
    }

    #[test]
    fn render_html_speedledger_matches_snapshot() {
        let rendered = render_html_body(SPEEDLEDGER_HTML, 100);
        let visible = osc8_to_visible(&rendered);
        assert_snapshot(&visible, SPEEDLEDGER_SNAPSHOT);
    }

    // --- Structural invariants -----------------------------------------------
    //
    // These assert properties of the rendered output that should hold for every
    // input, independent of the exact snapshot. They serve as quick alarms when
    // a refactor breaks the renderer in a way the snapshot diff might obscure.

    #[test]
    fn render_html_emits_no_raw_html_tags() {
        for html in [LINKEDIN_HTML, SPEEDLEDGER_HTML] {
            let rendered = render_html_body(html, 100);
            let visible = osc8_to_visible(&rendered);
            assert!(
                !visible.contains("<table"),
                "raw <table tag leaked through render_html_body"
            );
            assert!(
                !visible.contains("<a href"),
                "raw <a href tag leaked through render_html_body"
            );
            assert!(
                !visible.contains("<img"),
                "raw <img tag leaked through render_html_body"
            );
        }
    }

    #[test]
    fn render_html_collapses_blank_runs() {
        for html in [LINKEDIN_HTML, SPEEDLEDGER_HTML] {
            let rendered = render_html_body(html, 100);
            assert!(
                !rendered.contains("\n\n\n\n"),
                "render_html_body left a run of 4+ consecutive newlines"
            );
        }
    }

    #[test]
    fn render_html_strips_tracking_pixel_chars() {
        for html in [LINKEDIN_HTML, SPEEDLEDGER_HTML] {
            let rendered = render_html_body(html, 100);
            for marker in ['\u{200C}', '\u{200B}', '\u{200A}', '\u{034F}'] {
                assert!(
                    !rendered.contains(marker),
                    "tracking-pixel char U+{:04X} survived render_html_body",
                    marker as u32
                );
            }
        }
    }

    #[test]
    fn render_html_wraps_anchors_with_osc8() {
        // The linkedin fixture has many `<a>` elements; at least one should
        // turn into an OSC 8 link in the rendered output.
        let rendered = render_html_body(LINKEDIN_HTML, 100);
        assert!(
            rendered.contains("\x1b]8;;"),
            "expected at least one OSC 8 link in linkedin output, found none"
        );
    }

    #[test]
    fn render_html_styles_link_text_with_underline_and_color() {
        // OSC 8 alone makes Ghostty/iTerm2 treat the text as clickable, but
        // the affordance is only obvious when the cursor is exactly over it.
        // We add ANSI underline + cyan so the user sees something is clickable
        // without hovering.
        //
        // Force colors on for this test — the production code path checks
        // SHOULD_COLORIZE (TTY detection / NO_COLOR / --no-color), which would
        // suppress them under `cargo test`.
        colored::control::set_override(true);
        let rendered = render_html_body(LINKEDIN_HTML, 100);
        colored::control::unset_override();

        // Locate the visible span between the OSC 8 open `\x1b]8;;URL\x1b\\` and
        // the OSC 8 close `\x1b]8;;\x1b\\`.
        let osc8_open = "\x1b]8;;";
        let st = "\x1b\\";
        let osc8_close = "\x1b]8;;\x1b\\";
        let open_idx = rendered
            .find(osc8_open)
            .expect("expected an OSC 8 sequence in the output");
        let after_open = &rendered[open_idx + osc8_open.len()..];
        let url_end = after_open.find(st).expect("malformed OSC 8 — no ST");
        let body = &after_open[url_end + st.len()..];
        let body_end = body
            .find(osc8_close)
            .expect("malformed OSC 8 — no closing sequence");
        let visible_span = &body[..body_end];

        assert!(
            visible_span.contains("\x1b["),
            "link text should contain ANSI SGR styling, got: {visible_span:?}"
        );
        // Underline parameter is '4'; cyan foreground is '36'. Order varies
        // depending on which trait method `colored` resolved first, so check
        // both arrangements.
        let has_underline = visible_span.contains("4m")
            || visible_span.contains("4;")
            || visible_span.contains(";4;");
        assert!(
            has_underline,
            "expected underline (parameter 4) in link styling, got: {visible_span:?}"
        );
        assert!(
            visible_span.contains("36"),
            "expected cyan (parameter 36) in link styling, got: {visible_span:?}"
        );
    }

    #[test]
    fn render_html_suppresses_image_alt_text() {
        // Image elements in the fixtures have no alt text in raw form, but the
        // renderer is also expected to drop alt text even if present. Verify
        // by checking that synthesized image-alt markers (`[…]` from html2text)
        // don't appear.
        for html in [LINKEDIN_HTML, SPEEDLEDGER_HTML] {
            let rendered = render_html_body(html, 100);
            let visible = osc8_to_visible(&rendered);
            assert!(
                !visible.contains("[Logo]"),
                "image alt text [Logo] leaked through render_html_body"
            );
            assert!(
                !visible.contains("[Image]"),
                "image alt text [Image] leaked through render_html_body"
            );
        }
    }

    // --- osc8_to_visible self-tests -----------------------------------------

    #[test]
    fn osc8_to_visible_unwraps_single_link() {
        let raw = "before \x1b]8;;https://example.com/x\x1b\\link text\x1b]8;;\x1b\\ after";
        assert_eq!(
            osc8_to_visible(raw),
            "before [link=https://example.com/x]link text[/link] after"
        );
    }

    #[test]
    fn osc8_to_visible_leaves_plain_text_alone() {
        assert_eq!(osc8_to_visible("hello world"), "hello world");
    }
}
