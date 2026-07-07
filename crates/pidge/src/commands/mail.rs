//! `pidge mail list` — list messages merged across signed-in accounts.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Datelike, Local, Utc};
use colored::Colorize;
use futures::future::join_all;
use serde::Serialize;

use pidge_client::{AuthClient, ClientError, GraphClient};
use pidge_core::{Config, Message, MessageCache, short_hash};

use crate::cli::MailCommands;
use crate::output::linkify_text;

/// Pair of message and its computed short hash, for rendering.
pub(crate) struct MessageRow {
    pub(crate) message: Message,
    pub(crate) short_hash: String,
}

pub async fn run(command: MailCommands, json: bool) -> Result<()> {
    match command {
        MailCommands::List {
            account,
            folder,
            limit,
            page,
            cursor,
            unread,
            compact,
            table,
            full,
        } => {
            list(
                account, folder, limit, page, unread, compact, table, full, json, cursor,
            )
            .await
        }
        MailCommands::Show {
            fragment,
            mark_read,
            show_images,
            raw_html,
        } => {
            crate::commands::mail_show::run(fragment, mark_read, show_images, raw_html, json).await
        }
        MailCommands::Delta {
            folder,
            cursor,
            account,
            full,
        } => {
            crate::commands::delta_cmd::mail(crate::commands::delta_cmd::MailDeltaArgs {
                folder,
                cursor,
                account,
                full,
            })
            .await
        }
        MailCommands::Search {
            query,
            account,
            limit,
            cursor,
            compact,
            table,
            full,
        } => {
            crate::commands::mail_search::run(
                query, account, limit, compact, table, full, json, cursor,
            )
            .await
        }
        MailCommands::MarkRead { fragment } => {
            crate::commands::mail_actions::mark_read(fragment).await
        }
        MailCommands::MarkUnread { fragment } => {
            crate::commands::mail_actions::mark_unread(fragment).await
        }
        MailCommands::Flag { fragment } => crate::commands::mail_actions::flag(fragment).await,
        MailCommands::Unflag { fragment } => crate::commands::mail_actions::unflag(fragment).await,
        MailCommands::Archive {
            fragment,
            from,
            older_than,
            account,
            yes,
        } => {
            crate::commands::mail_actions::archive_dispatch(
                fragment, from, older_than, account, yes,
            )
            .await
        }
        MailCommands::Move {
            fragment,
            to,
            from,
            older_than,
            account,
            yes,
        } => crate::commands::mail_move::run(fragment, from, older_than, account, to, yes).await,
        MailCommands::Folders { account } => {
            crate::commands::mail_folders::run(account, json).await
        }
        MailCommands::Mkdir { name, account } => {
            crate::commands::mail_folders::mkdir(name, account).await
        }
        MailCommands::Rmdir { name, account, yes } => {
            crate::commands::mail_folders::rmdir(name, account, yes).await
        }
        MailCommands::New(args) => crate::commands::mail_compose::send(args).await,
        MailCommands::Reply { fragment, compose } => {
            crate::commands::mail_compose::reply(fragment, compose, false).await
        }
        MailCommands::ReplyAll { fragment, compose } => {
            crate::commands::mail_compose::reply(fragment, compose, true).await
        }
        MailCommands::Forward { fragment, compose } => {
            crate::commands::mail_compose::forward(fragment, compose).await
        }
        MailCommands::Delete {
            fragment,
            from,
            older_than,
            account,
            yes,
        } => crate::commands::mail_delete::run(fragment, from, older_than, account, yes).await,
        MailCommands::Unsubscribe { fragment, yes } => {
            crate::commands::mail_unsubscribe::run(fragment, yes).await
        }
        MailCommands::Attachments { command } => {
            crate::commands::mail_attachments::run(command, json).await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn list(
    account_filter: Vec<String>,
    folder: Option<String>,
    limit: usize,
    page: usize,
    unread_only: bool,
    compact: bool,
    table: bool,
    full: bool,
    json: bool,
    cursor: Option<String>,
) -> Result<()> {
    // Continuation: fetch each account's next page at its stored URL.
    if let Some(token) = cursor {
        let cursor = pidge_client::Cursor::decode(&token, "mail")?;
        return list_at_cursor(cursor, compact, table, full, json).await;
    }
    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!(
            "No accounts signed in. Run `pidge account add` to add one."
        ));
    }

    let target_emails: Vec<String> = if account_filter.is_empty() {
        config.accounts.iter().map(|a| a.email.clone()).collect()
    } else {
        for f in &account_filter {
            if config.find(f).is_none() {
                return Err(anyhow!("not signed in to {f}"));
            }
        }
        account_filter
    };

    let per_account = compute_per_account_fetch(limit, target_emails.len());
    // 1-based page → 0-based skip. `compute_per_account_skip` keeps the merge
    // tidy: every account gets the same skip so received-time interleaving stays
    // intact across pages (yes, multi-account paging is imperfect — see comment
    // on the helper — but it's the right approximation given Graph's per-mailbox
    // ordering).
    let skip = compute_per_account_skip(per_account, page);
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    let futures = target_emails.iter().map(|email| {
        let graph = &graph;
        let e = email.clone();
        let folder = folder.clone();
        async move {
            let result = match folder {
                None => graph.list_inbox(&e, per_account, skip, unread_only).await,
                Some(path) => {
                    // Resolve the (possibly nested) folder path to an ID for
                    // this account, then list it. A path that doesn't exist
                    // in this account surfaces as an empty, explanatory error.
                    match crate::commands::mail_folders::resolve_folder_path(graph, &e, &path).await
                    {
                        Ok(Some(id)) => {
                            graph
                                .list_folder(&e, &id, per_account, skip, unread_only)
                                .await
                        }
                        Ok(None) => Err(pidge_client::ClientError::Graph {
                            status: 404,
                            message: format!("no folder '{path}' in {e}"),
                        }),
                        Err(err) => Err(pidge_client::ClientError::Graph {
                            status: 500,
                            message: err.to_string(),
                        }),
                    }
                }
            };
            (e, result)
        }
    });

    let results = join_all(futures).await;

    let mut all_messages: Vec<Message> = Vec::new();
    let mut had_success = false;
    let mut next_cursor = pidge_client::Cursor::new("mail");
    for (email, result) in results {
        match result {
            Ok(page) => {
                had_success = true;
                next_cursor
                    .per_account
                    .insert(email.clone(), page.next_link);
                all_messages.extend(page.messages);
            }
            Err(ClientError::SessionExpired { email: e }) => {
                eprintln!(
                    "{} {e}: session expired, run `pidge account add`",
                    "WARNING:".yellow().bold()
                );
            }
            Err(e) => {
                eprintln!("{} {email}: {e}", "WARNING:".yellow().bold());
            }
        }
    }

    if !had_success {
        return Err(anyhow!("All accounts failed."));
    }

    all_messages.sort_by_key(|b| std::cmp::Reverse(b.received_at));
    all_messages.truncate(limit);

    let rows: Vec<MessageRow> = all_messages
        .into_iter()
        .map(|m| {
            let h = short_hash(&m.id);
            MessageRow {
                message: m,
                short_hash: h,
            }
        })
        .collect();

    update_cache(&rows)?;

    let single_account = target_emails.len() == 1;
    let labels = account_labels(&target_emails);

    // Agents get a continuation token whenever more pages exist.
    if json && !next_cursor.exhausted() {
        update_cache(&rows)?;
        return render_json_with_cursor(&rows, Some(next_cursor.encode()));
    }
    render(&rows, single_account, &labels, compact, table, full, json)
}

/// Continue a cursor: pull one page per non-exhausted account, merge by
/// received time, and hand back a refreshed cursor. Exact continuation —
/// every account advances via its own Graph nextLink (no $skip drift).
pub(crate) async fn list_at_cursor(
    cursor: pidge_client::Cursor,
    compact: bool,
    table: bool,
    full: bool,
    json: bool,
) -> Result<()> {
    let graph = GraphClient::new(AuthClient::from_env()?)?;
    let mut all_messages: Vec<Message> = Vec::new();
    let mut next_cursor = pidge_client::Cursor::new("mail");
    let futures = cursor
        .per_account
        .iter()
        .filter_map(|(email, link)| link.as_ref().map(|l| (email.clone(), l.clone())))
        .map(|(email, link)| {
            let graph = &graph;
            async move {
                let result = graph.list_messages_at(&email, &link).await;
                (email, result)
            }
        });
    let mut had_success = false;
    for (email, result) in join_all(futures).await {
        match result {
            Ok(page) => {
                had_success = true;
                next_cursor
                    .per_account
                    .insert(email.clone(), page.next_link);
                all_messages.extend(page.messages);
            }
            Err(e) => {
                eprintln!("{} {email}: {e}", "WARNING:".yellow().bold());
                next_cursor.per_account.insert(email.clone(), None);
            }
        }
    }
    if !had_success && !cursor.exhausted() {
        return Err(anyhow!("All accounts failed."));
    }

    all_messages.sort_by_key(|b| std::cmp::Reverse(b.received_at));
    let rows: Vec<MessageRow> = all_messages
        .into_iter()
        .map(|m| {
            let h = short_hash(&m.id);
            MessageRow {
                message: m,
                short_hash: h,
            }
        })
        .collect();
    update_cache(&rows)?;

    let emails: Vec<String> = cursor.per_account.keys().cloned().collect();
    let labels = account_labels(&emails);
    if json {
        let token = (!next_cursor.exhausted()).then(|| next_cursor.encode());
        return render_json_with_cursor(&rows, token);
    }
    render(
        &rows,
        emails.len() == 1,
        &labels,
        compact,
        table,
        full,
        false,
    )
}

/// Pick a renderer based on the user's output flags. Centralised so
/// `pidge mail list` and `pidge mail search` produce the same shape.
///
/// `--json` always wins (so scripting paths are predictable). Then
/// `--table` for tabular, then `--full` for the entire body inlined,
/// `--compact` for a 1-line preview per card, otherwise the default
/// 8-line preview card layout.
pub(crate) fn render(
    rows: &[MessageRow],
    hide_account: bool,
    labels: &std::collections::HashMap<String, String>,
    compact: bool,
    table: bool,
    full: bool,
    json: bool,
) -> Result<()> {
    if json {
        return render_json(rows);
    }
    if table {
        return render_table(rows, hide_account, labels);
    }
    // 8 lines per card by default — comfortably inside the 5–10 sweet
    // spot regardless of terminal width. Short emails fill fewer lines
    // naturally; long emails show 8 lines with a trailing `…` to signal
    // there's more. `--compact` collapses to a single preview line, and
    // `--full` removes the cap entirely so the whole body is shown.
    let preview_lines = if compact {
        1
    } else if full {
        usize::MAX
    } else {
        DEFAULT_PREVIEW_LINES
    };
    render_cards(rows, hide_account, labels, preview_lines)
}

const DEFAULT_PREVIEW_LINES: usize = 8;

/// Map each signed-in e-mail to its display label for list views. If a
/// domain has only one account signed in, we shorten its label to just the
/// domain (`kristofer@mklab.se` → `mklab.se`) since the local-part is
/// redundant given the inbox is yours. If two accounts share a domain we
/// keep both as full addresses to preserve disambiguation.
pub(crate) fn account_labels(accounts: &[String]) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut domain_count: HashMap<&str, usize> = HashMap::new();
    for email in accounts {
        if let Some((_, domain)) = email.split_once('@') {
            *domain_count.entry(domain).or_insert(0) += 1;
        }
    }
    accounts
        .iter()
        .map(|email| {
            let label = email
                .split_once('@')
                .and_then(|(_, d)| {
                    if domain_count.get(d) == Some(&1) {
                        Some(d.to_string())
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| email.clone());
            (email.clone(), label)
        })
        .collect()
}

fn update_cache(rows: &[MessageRow]) -> Result<()> {
    let mut cache = MessageCache::load()?;
    let pairs: Vec<(String, String)> = rows
        .iter()
        .map(|r| (r.message.id.clone(), r.message.account.clone()))
        .collect();
    cache.insert_many(&pairs);
    cache.save()?;
    Ok(())
}

fn compute_per_account_fetch(limit: usize, num_accounts: usize) -> usize {
    if num_accounts == 0 {
        return limit;
    }
    let calc = (limit as f64 * 1.2 / num_accounts as f64).ceil() as usize;
    calc.max(10)
}

/// Compute the per-account `$skip` for a given page. Each account is paged
/// independently and the merged result is trimmed to `per_account * accounts`,
/// so page boundaries are approximate when accounts have very different
/// volumes — page 2 across two accounts may include some items from one
/// account that also appeared on page 1 of the other, sorted by received
/// time. This matches users' mental model of "I see the next batch" without
/// requiring a cross-account cursor we can't actually maintain (Graph has no
/// federated paging).
fn compute_per_account_skip(per_account: usize, page: usize) -> usize {
    page.saturating_sub(1) * per_account
}

fn from_display(from: &pidge_core::MessageFrom) -> &str {
    if from.name.is_empty() {
        &from.address
    } else {
        &from.name
    }
}

fn style_subject(subject: &str, is_read: bool) -> String {
    let linked = linkify_text(subject);
    if is_read {
        linked.cyan().to_string()
    } else {
        linked.bold().magenta().to_string()
    }
}

/// Visual prefix for a message's flag state. Empty string when not flagged
/// so unflagged rows stay aligned with the rest. Yellow `⚑` for an active
/// follow-up flag; green `✓` for a completed one — matches Outlook's
/// distinction between "flagged" and "complete".
pub fn flag_marker(status: pidge_core::FlagStatus) -> String {
    match status {
        pidge_core::FlagStatus::Flagged => format!("{} ", "⚑".yellow().bold()),
        pidge_core::FlagStatus::Complete => format!("{} ", "✓".green()),
        pidge_core::FlagStatus::NotFlagged => String::new(),
    }
}

/// Card-style multi-line rendering, with a dim grey horizontal rule between
/// cards to make entry boundaries unmistakable.
///
///   `<id> · Account: <acct> · From: <from> · 📎 · <received>`   ← labeled meta header
///   `   <flag> <subject>`                                        ← indented; bold magenta when unread
///   `   <preview line 1>`                                        ← indented, word-wrapped
///   `   <preview line 2>`                                          to terminal width
///   `   ...`                                                       up to `preview_lines` lines
///   `───────────────────`                                        ← dim grey rule
///
/// `preview_lines` controls how much of the body excerpt is shown:
/// `4` for the default rich view, `1` for `--compact`. If the wrapped
/// preview has more lines than `preview_lines`, the last shown line ends
/// in `…` to make the truncation visible.
fn render_cards(
    rows: &[MessageRow],
    hide_account: bool,
    labels: &std::collections::HashMap<String, String>,
    preview_lines: usize,
) -> Result<()> {
    const INDENT: &str = "   ";
    let width = terminal_width();
    let sep = format!(" {} ", "·".dimmed());
    let rule_str: String = "─".repeat(width);

    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            println!("{}", rule_str.dimmed());
        }
        let account_label = labels
            .get(&row.message.account)
            .cloned()
            .unwrap_or_else(|| row.message.account.clone());

        let mut header_parts: Vec<String> = Vec::with_capacity(5);
        header_parts.push(row.short_hash.dimmed().to_string());
        if !hide_account {
            header_parts.push(format!("{}{}", "Account: ".dimmed(), account_label.cyan()));
        }
        header_parts.push(format!(
            "{}{}",
            "From: ".dimmed(),
            from_display(&row.message.from).yellow().bold()
        ));
        if row.message.has_attachments {
            header_parts.push("📎".to_string());
        }
        header_parts.push(
            relative_received(row.message.received_at)
                .dimmed()
                .italic()
                .to_string(),
        );
        println!("{}", header_parts.join(&sep));

        let flag = flag_marker(row.message.flag_status);
        println!(
            "{INDENT}{flag}{}",
            style_subject(&row.message.subject, row.message.is_read)
        );

        if preview_lines > 0 {
            let max_chars = width.saturating_sub(INDENT.len());
            let lines = render_preview_lines(&row.message, max_chars, preview_lines);
            for line in lines {
                println!("{INDENT}{line}");
            }
        }
    }
    Ok(())
}

/// Produce up to `max_lines` styled preview lines for `message`, ready to
/// `println!` straight into a card.
///
/// HTML bodies go through `html2text` so each `<a href="…">text</a>` becomes
/// an OSC 8 hyperlink with the original anchor TEXT as the clickable surface
/// (matches `pidge mail show`). Plain-text bodies fall back to the legacy
/// wrap-and-bare-URL-linkify path. If no body is available, we still
/// render the 255-char `bodyPreview` snippet as plain text.
fn render_preview_lines(message: &Message, width: usize, max_lines: usize) -> Vec<String> {
    use pidge_core::BodyContentType;
    let has_body = !message.body.is_empty();
    match (has_body, message.body_content_type) {
        (true, BodyContentType::Html) => render_html_preview(&message.body, width, max_lines),
        (true, BodyContentType::Text) => wrap_preview(&message.body, width, max_lines)
            .into_iter()
            .map(|l| style_preview_line(&l))
            .collect(),
        (false, _) => {
            if message.preview.is_empty() {
                return Vec::new();
            }
            wrap_preview(&message.preview, width, max_lines)
                .into_iter()
                .map(|l| style_preview_line(&l))
                .collect()
        }
    }
}

/// HTML-aware preview renderer. Flattens HTML via `html2text` (raw_mode so
/// marketing-email layout tables read as ordinary paragraphs), then emits
/// each visible span — dim for prose, cyan+underline+OSC 8 for `<a>`
/// anchor text. Image alt text and `<img>` URLs are skipped entirely
/// (the user has no use for `[Logo]` markers or clickable tracking pixels).
fn render_html_preview(html: &str, width: usize, max_lines: usize) -> Vec<String> {
    use html2text::render::text_renderer::{RichAnnotation, TaggedLineElement};

    let no_color = crate::output::no_color();
    let raw_lines = match html2text::config::rich()
        .raw_mode(true)
        .lines_from_read(html.as_bytes(), width)
    {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };

    // First pass: assemble plain text per line; collapse blank runs to
    // avoid huge gaps from flattened layout tables.
    let mut staged: Vec<Vec<TaggedLinePiece>> = Vec::new();
    for line in raw_lines {
        let mut pieces: Vec<TaggedLinePiece> = Vec::new();
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
            // Strip the same tracking-padding chars `mail show` does, so
            // marketing previews don't waste lines on invisible joiners.
            let cleaned: String =
                ts.s.chars()
                    .filter(|c| !matches!(c, '\u{200C}' | '\u{200B}' | '\u{200A}' | '\u{034F}'))
                    .collect();
            if cleaned.is_empty() {
                continue;
            }
            pieces.push(TaggedLinePiece {
                text: cleaned,
                url: url.map(str::to_string),
            });
        }
        staged.push(pieces);
    }

    // Drop blank lines entirely — preview budget is 5-10 lines of CONTENT,
    // and HTML paragraph breaks would otherwise eat 30-50% of that budget
    // showing nothing useful. Trade visual breathing room for information
    // density; `mail show` still has the full paragraph structure.
    let mut compact: Vec<Vec<TaggedLinePiece>> = staged
        .into_iter()
        .filter(|pieces| !pieces.is_empty() && pieces.iter().any(|p| !p.text.trim().is_empty()))
        .collect();

    let total = compact.len();
    let truncated = total > max_lines;
    compact.truncate(max_lines);

    // Third pass: style.
    let mut out: Vec<String> = compact
        .into_iter()
        .map(|pieces| style_html_line(pieces, no_color))
        .collect();
    if truncated {
        if let Some(last) = out.last_mut() {
            if no_color {
                last.push('…');
            } else {
                last.push_str(&"…".dimmed().to_string());
            }
        }
    }
    out
}

struct TaggedLinePiece {
    text: String,
    url: Option<String>,
}

fn style_html_line(pieces: Vec<TaggedLinePiece>, no_color: bool) -> String {
    let mut out = String::new();
    for piece in pieces {
        if no_color {
            out.push_str(&piece.text);
            continue;
        }
        if let Some(url) = piece.url {
            // OSC 8 wrap with cyan + underline visible text — the same
            // treatment `mail show` uses so anchor text (not the URL) is
            // the click target.
            out.push_str("\x1b]8;;");
            out.push_str(&url);
            out.push_str("\x1b\\");
            out.push_str(&piece.text.cyan().underline().to_string());
            out.push_str("\x1b]8;;\x1b\\");
        } else {
            out.push_str(&piece.text.dimmed().to_string());
        }
    }
    out
}

/// Render one wrapped preview line so URLs are visually distinct from prose
/// and clickable in supporting terminals.
///
/// - Non-URL stretches are dimmed (preserves the "preview is secondary
///   context" feel of the card layout).
/// - URLs are wrapped with an OSC 8 escape so the terminal makes them
///   clickable, with the visible text styled cyan + underlined — the same
///   convention `pidge mail show` uses for HTML anchors.
fn style_preview_line(line: &str) -> String {
    use linkify::{LinkFinder, LinkKind};
    if crate::output::no_color() {
        return line.to_string();
    }
    let finder = LinkFinder::new();
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0;
    for link in finder.links(line) {
        if !matches!(link.kind(), LinkKind::Url) {
            continue;
        }
        let (start, end) = (link.start(), link.end());
        if cursor < start {
            out.push_str(&line[cursor..start].dimmed().to_string());
        }
        // OSC 8 wrap with cyan + underline visible text — same treatment as
        // mail show. The full URL is the click target; styling makes the
        // link visually obvious even before hovering.
        out.push_str("\x1b]8;;");
        out.push_str(link.as_str());
        out.push_str("\x1b\\");
        out.push_str(&link.as_str().cyan().underline().to_string());
        out.push_str("\x1b]8;;\x1b\\");
        cursor = end;
    }
    if cursor < line.len() {
        out.push_str(&line[cursor..].dimmed().to_string());
    }
    out
}

/// Word-wrap `text` to `width` columns, returning at most `max_lines` lines.
/// If the wrapped output would have been longer, the final returned line ends
/// in `…` so the caller doesn't need to know whether truncation happened.
///
/// Whitespace (including newlines) is collapsed: bodyPreview from Microsoft
/// Graph often has paragraph breaks that don't align nicely with the card
/// width, so we reflow the text as a single paragraph.
///
/// URLs are recognised via `linkify` and treated as atomic tokens — they are
/// never split across lines, even when longer than `width`. Splitting a URL
/// mid-string would prevent the link styler from recognising it and would
/// produce broken `…`-truncated hrefs.
fn wrap_preview(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let tokens = tokenize_with_urls(text);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut idx = 0;
    while idx < tokens.len() {
        let token = &tokens[idx];
        let tlen = token.text.chars().count();
        if current.is_empty() {
            // Long non-URL word: hard-split across lines (URLs stay whole even
            // when oversized — the terminal will visually wrap them, which is
            // fine because clicking still works on a soft-wrapped OSC8 link).
            if !token.is_url && tlen > width {
                let chars: Vec<char> = token.text.chars().collect();
                let mut start = 0;
                while start < chars.len() {
                    let end = (start + width).min(chars.len());
                    lines.push(chars[start..end].iter().collect());
                    if lines.len() == max_lines {
                        return finalize(lines, width, idx + 1 < tokens.len() || end < chars.len());
                    }
                    start = end;
                }
            } else {
                current.push_str(&token.text);
            }
            idx += 1;
        } else if current.chars().count() + 1 + tlen <= width {
            current.push(' ');
            current.push_str(&token.text);
            idx += 1;
        } else {
            lines.push(std::mem::take(&mut current));
            if lines.len() == max_lines {
                return finalize(lines, width, idx < tokens.len());
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    finalize(lines, width, false)
}

/// One whitespace-separated piece of text, flagged as either a URL or
/// regular prose. URL tokens are never split by the wrap algorithm; prose
/// tokens may be hard-split if longer than the line width.
struct PreviewToken {
    text: String,
    is_url: bool,
}

/// Tokenise `text` on whitespace, then split each word around any URLs it
/// contains so URL tokens come out as standalone atomic units (a URL embedded
/// in `Visit:https://example.com,then…` becomes three tokens).
fn tokenize_with_urls(text: &str) -> Vec<PreviewToken> {
    use linkify::{LinkFinder, LinkKind};
    let finder = LinkFinder::new();
    let mut tokens: Vec<PreviewToken> = Vec::new();
    for word in text.split_whitespace() {
        let urls: Vec<_> = finder
            .links(word)
            .filter(|l| matches!(l.kind(), LinkKind::Url))
            .collect();
        if urls.is_empty() {
            tokens.push(PreviewToken {
                text: word.to_string(),
                is_url: false,
            });
            continue;
        }
        let mut cursor = 0;
        for url in urls {
            if cursor < url.start() {
                tokens.push(PreviewToken {
                    text: word[cursor..url.start()].to_string(),
                    is_url: false,
                });
            }
            tokens.push(PreviewToken {
                text: url.as_str().to_string(),
                is_url: true,
            });
            cursor = url.end();
        }
        if cursor < word.len() {
            tokens.push(PreviewToken {
                text: word[cursor..].to_string(),
                is_url: false,
            });
        }
    }
    tokens
}

/// If the wrapped output was truncated, append `…` to the last shown line,
/// stealing the trailing character when needed to stay within `width`.
fn finalize(mut lines: Vec<String>, width: usize, truncated: bool) -> Vec<String> {
    if !truncated {
        return lines;
    }
    if let Some(last) = lines.last_mut() {
        if last.chars().count() < width {
            last.push('…');
        } else {
            let mut chars: Vec<char> = last.chars().collect();
            if !chars.is_empty() {
                chars.pop();
            }
            chars.push('…');
            *last = chars.into_iter().collect();
        }
    }
    lines
}

fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(80)
}

/// Tabular rendering — one row per message with `ID`, `ACCOUNT`, `FROM`,
/// `SUBJECT`, `RECEIVED` columns. Useful for piping into other tools or
/// when the user prefers a dense overview without preview text.
///
/// The `📎` marker is appended to `SUBJECT` (rather than getting its own
/// column) so the table stays compact and the indicator still scans next
/// to the subject — same place an attachment icon lives in most mail UIs.
fn render_table(
    rows: &[MessageRow],
    hide_account: bool,
    labels: &std::collections::HashMap<String, String>,
) -> Result<()> {
    use comfy_table::{ContentArrangement, Table};

    let mut table = Table::new();
    table.load_preset(comfy_table::presets::UTF8_HORIZONTAL_ONLY);
    table.set_content_arrangement(ContentArrangement::Dynamic);

    let mut header = vec!["ID", "ACCOUNT", "FROM", "SUBJECT", "RECEIVED"];
    if hide_account {
        header.remove(1);
    }
    table.set_header(header);

    for row in rows {
        let attach = if row.message.has_attachments {
            " 📎"
        } else {
            ""
        };
        let subject = format!(
            "{}{}{}",
            flag_marker(row.message.flag_status),
            style_subject(&row.message.subject, row.message.is_read),
            attach,
        );

        let account_label = labels
            .get(&row.message.account)
            .cloned()
            .unwrap_or_else(|| row.message.account.clone());

        let mut cells = vec![
            row.short_hash.dimmed().to_string(),
            account_label,
            from_display(&row.message.from).to_string(),
            subject,
            relative_received(row.message.received_at),
        ];
        if hide_account {
            cells.remove(1);
        }
        table.add_row(cells);
    }

    println!("{table}");
    Ok(())
}

#[derive(Serialize)]
struct MessageOut<'a> {
    id: &'a str,
    graph_id: &'a str,
    account: &'a str,
    from: &'a pidge_core::MessageFrom,
    subject: &'a str,
    received_at: chrono::DateTime<chrono::Utc>,
    is_read: bool,
    /// Short snippet — Graph's `bodyPreview`, capped at 255 chars. Cheap
    /// "summary" string suitable for scanning.
    preview: &'a str,
    /// Full body content as plain text. For HTML emails it's the
    /// html2text-converted version, for plain-text emails it's the
    /// original. Aimed at scripting / AI consumers who don't want to
    /// parse HTML themselves.
    body_text: String,
    // Renamed so jq filters can use the same casing as the underlying
    // FlagStatus enum variants ("flagged" / "notFlagged" / "complete").
    #[serde(rename = "flagStatus")]
    flag_status: pidge_core::FlagStatus,
    has_attachments: bool,
}

pub(crate) fn render_json_with_cursor(
    rows: &[MessageRow],
    next_cursor: Option<String>,
) -> Result<()> {
    let out: Vec<MessageOut<'_>> = rows
        .iter()
        .map(|r| MessageOut {
            id: &r.short_hash,
            graph_id: &r.message.id,
            account: &r.message.account,
            from: &r.message.from,
            subject: &r.message.subject,
            received_at: r.message.received_at,
            is_read: r.message.is_read,
            preview: &r.message.preview,
            body_text: body_as_plain_text(&r.message),
            flag_status: r.message.flag_status,
            has_attachments: r.message.has_attachments,
        })
        .collect();
    match next_cursor {
        // Cursor flows use an object envelope so agents can continue paging.
        Some(cursor) => println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "items": out,
                "next_cursor": cursor,
            }))?
        ),
        None => println!("{}", serde_json::to_string_pretty(&out)?),
    }
    Ok(())
}

fn render_json(rows: &[MessageRow]) -> Result<()> {
    let out: Vec<MessageOut<'_>> = rows
        .iter()
        .map(|r| MessageOut {
            id: &r.short_hash,
            graph_id: &r.message.id,
            account: &r.message.account,
            from: &r.message.from,
            subject: &r.message.subject,
            received_at: r.message.received_at,
            is_read: r.message.is_read,
            preview: &r.message.preview,
            body_text: body_as_plain_text(&r.message),
            flag_status: r.message.flag_status,
            has_attachments: r.message.has_attachments,
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Plain-text representation of the message body. For HTML emails we run
/// html2text locally (no fancy formatting — just text + structure); for
/// plain-text emails we return the body unchanged. Empty if the body is
/// missing — callers should fall back to `Message::preview`.
fn body_as_plain_text(m: &Message) -> String {
    use pidge_core::BodyContentType;
    if m.body.is_empty() {
        return String::new();
    }
    match m.body_content_type {
        BodyContentType::Text => m.body.clone(),
        BodyContentType::Html => {
            // 100 cols is a reasonable target width for downstream consumers;
            // they can re-wrap if they want. We don't care about styling
            // here — just structured plain text.
            html2text::from_read(m.body.as_bytes(), 100)
        }
    }
}

fn relative_received(then: DateTime<Utc>) -> String {
    let now = Local::now();
    let then_local: DateTime<Local> = then.with_timezone(&Local);
    let delta = now - then_local;

    if delta.num_seconds() < 60 {
        return "just now".to_string();
    }
    if delta.num_minutes() < 60 {
        return format!("{}m ago", delta.num_minutes());
    }
    if delta.num_hours() < 24 {
        return format!("{}h ago", delta.num_hours());
    }
    if now.date_naive().pred_opt() == Some(then_local.date_naive()) {
        return "yesterday".to_string();
    }
    if delta.num_days() < 7 {
        return then_local.format("%a").to_string();
    }
    if now.year() == then_local.year() {
        return then_local.format("%b %-d").to_string();
    }
    then_local.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_account_fetch_at_least_10() {
        assert_eq!(compute_per_account_fetch(5, 1), 10);
    }

    #[test]
    fn per_account_fetch_scales_with_limit_and_accounts() {
        // ceil(25 * 1.2 / 3) = ceil(10) = 10 → 10 (because max(10) wins)
        assert_eq!(compute_per_account_fetch(25, 3), 10);
        // ceil(100 * 1.2 / 3) = ceil(40) = 40
        assert_eq!(compute_per_account_fetch(100, 3), 40);
    }

    #[test]
    fn wrap_preview_fits_in_one_line() {
        assert_eq!(wrap_preview("hello world", 40, 4), vec!["hello world"]);
    }

    #[test]
    fn wrap_preview_wraps_at_word_boundary() {
        let out = wrap_preview("the quick brown fox jumps", 10, 4);
        assert_eq!(out, vec!["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn wrap_preview_collapses_newlines_and_extra_spaces() {
        let out = wrap_preview("Hello\n\nworld   here", 40, 4);
        assert_eq!(out, vec!["Hello world here"]);
    }

    #[test]
    fn wrap_preview_truncates_with_ellipsis_when_more_lines_remain() {
        let out = wrap_preview("aaa bbb ccc ddd eee fff ggg", 7, 2);
        // first two wrapped lines, last line gets trailing …
        assert_eq!(out.len(), 2);
        assert!(
            out[1].ends_with('…'),
            "expected truncation marker, got {out:?}"
        );
    }

    #[test]
    fn wrap_preview_returns_empty_when_max_lines_is_zero() {
        assert!(wrap_preview("anything", 40, 0).is_empty());
    }

    #[test]
    fn default_preview_lines_is_in_the_5_to_10_range() {
        assert!(
            (5..=10).contains(&DEFAULT_PREVIEW_LINES),
            "default preview line count should stay inside the 5-10 sweet spot"
        );
    }

    #[test]
    fn wrap_preview_keeps_long_url_whole() {
        // A URL longer than the line width must stay on a single line — even
        // though it exceeds `width` — so the link styler later finds it intact.
        let long_url = "https://example.com/path/with/many/segments?q=a&also=this-is-extra-padding";
        let text = format!("see {long_url} for details");
        let out = wrap_preview(&text, 20, 5);
        assert!(
            out.iter().any(|l| l == long_url),
            "expected the long URL to appear as its own line, got: {out:?}"
        );
        assert!(
            !out.iter()
                .any(|l| l.contains('…') && l.contains("https://")),
            "URL was truncated mid-string: {out:?}"
        );
    }

    #[test]
    fn wrap_preview_splits_around_url_inside_word() {
        // "Visit:https://example.com,thanks" should pop the URL into its own
        // token; otherwise the URL would not be wrappable as a unit.
        let out = wrap_preview("Visit:https://example.com,thanks for reading", 80, 5);
        // First line should contain the URL intact.
        assert!(out[0].contains("https://example.com"), "got: {out:?}");
    }

    #[test]
    fn wrap_preview_splits_oversized_word() {
        // A 30-char "word" must fit in 10-col lines.
        let out = wrap_preview("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 10, 4);
        assert!(out.iter().all(|l| l.chars().count() <= 10));
        assert_eq!(out.iter().map(|l| l.len()).sum::<usize>(), 30);
    }
}
