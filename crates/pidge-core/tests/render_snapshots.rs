//! Snapshot tests of the HTML renderer against anonymized real-world
//! e-mails (see "Workflow: when a real e-mail renders badly" in CLAUDE.md).
//! Accept new output with `UPDATE_SNAPSHOTS=1 cargo test -p pidge-core --test render_snapshots`.

use pidge_core::render::{LinkStyle, render_html};

const LINKEDIN_HTML: &str = include_str!("fixtures/linkedin_jobs_digest.html");
const SPEEDLEDGER_HTML: &str = include_str!("fixtures/speedledger_newsletter.html");
const LINKEDIN_SNAPSHOT: &str = "tests/fixtures/linkedin_jobs_digest.expected.txt";
const SPEEDLEDGER_SNAPSHOT: &str = "tests/fixtures/speedledger_newsletter.expected.txt";

/// Every fixture, for the structural invariants.
const ALL: &[&str] = &[LINKEDIN_HTML, SPEEDLEDGER_HTML];

fn render_html_body(html: &str, width: usize) -> String {
    render_html(html, width, LinkStyle::Osc8)
}

/// Translate OSC 8 escape sequences emitted by `render_html_body` into the
/// snapshot-friendly form `[link=URL]text[/link]` and strip ANSI SGR codes
/// (color / underline / etc.) so snapshots stay readable across `colored`
/// crate version bumps.
///
/// OSC 8 sequences are all-ASCII (`ESC ] 8 ; ; URL ESC \`); SGR sequences
/// are `ESC [ params m`. Both are byte-safe to scan around: multi-byte
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
                // skip just the ESC [ and keep walking; we don't emit
                // any of those today, so any unknown CSI is a bug worth
                // showing in the snapshot.
                let after = &rest[pos + CSI.len()..];
                let end_byte = after.bytes().position(|b| b == b'm');
                match end_byte {
                    Some(e) if after[..e].bytes().all(|b| b.is_ascii_digit() || b == b';') => {
                        rest = &after[e + 1..];
                    }
                    _ => {
                        // Unknown CSI: fall through, keep `\x1b[` visible
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
    diff.push_str("snapshot mismatch, first differing lines:\n");
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
        "Run `UPDATE_SNAPSHOTS=1 cargo test -p pidge-core --test render_snapshots` to accept the new output.\n",
    );
    panic!("{diff}");
}

#[test]
fn render_html_linkedin_matches_snapshot() {
    assert_snapshot(
        &osc8_to_visible(&render_html_body(LINKEDIN_HTML, 100)),
        LINKEDIN_SNAPSHOT,
    );
}

#[test]
fn render_html_speedledger_matches_snapshot() {
    assert_snapshot(
        &osc8_to_visible(&render_html_body(SPEEDLEDGER_HTML, 100)),
        SPEEDLEDGER_SNAPSHOT,
    );
}

#[test]
fn render_html_emits_no_raw_html_tags() {
    for html in ALL {
        let visible = osc8_to_visible(&render_html_body(html, 100));
        for tag in ["<table", "<a href", "<img"] {
            assert!(
                !visible.contains(tag),
                "raw {tag} leaked through the renderer"
            );
        }
    }
}

#[test]
fn render_html_collapses_blank_runs() {
    for html in ALL {
        assert!(!render_html_body(html, 100).contains("\n\n\n\n"));
    }
}

#[test]
fn render_html_strips_tracking_pixel_chars() {
    for html in ALL {
        let rendered = render_html_body(html, 100);
        for marker in ['\u{200C}', '\u{200B}', '\u{200A}', '\u{034F}'] {
            assert!(
                !rendered.contains(marker),
                "U+{:04X} survived",
                marker as u32
            );
        }
    }
}

#[test]
fn render_html_wraps_anchors_with_osc8() {
    assert!(render_html_body(LINKEDIN_HTML, 100).contains("\x1b]8;;"));
}

#[test]
fn render_html_suppresses_image_alt_text() {
    for html in ALL {
        let visible = osc8_to_visible(&render_html_body(html, 100));
        assert!(!visible.contains("[Logo]") && !visible.contains("[Image]"));
    }
}

#[test]
fn osc8_to_visible_unwraps_single_link() {
    let raw = "before \x1b]8;;https://example.com/x\x1b\\link text\x1b]8;;\x1b\\ after";
    assert_eq!(
        osc8_to_visible(raw),
        "before [link=https://example.com/x]link text[/link] after"
    );
}
