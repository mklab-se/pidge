//! `pidge mcp connect <url>`: sign in to a hosted pidge MCP server and
//! migrate every locally signed-in Microsoft account onto it.
//!
//! The migration loop (skip already-connected mailboxes, call
//! `accounts_connect` for the rest, wait for each to finish) is factored out
//! as [`run_migration`] over a small [`McpCalls`] trait so it can be driven
//! against a fake in tests, with no network involved.
//!
//! Output contract: all progress/human text (sign-in prompts, the authorize
//! URL, each connect link, the Enter prompt, "already connected" notes, and
//! warnings) goes to stderr, unconditionally; `--json` does not suppress
//! it, since the connect link is often the only way to finish the flow.
//! Stdout carries only the one final payload: the `accounts_list` text, or
//! (with `--json`) the `{"server","connected","skipped"}` summary.

use std::collections::HashSet;
use std::future::Future;
use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::json;
use tokio::sync::Mutex;

use pidge_client::mcp::{McpRpc, McpTokenStore, McpTokens, sign_in};
use pidge_core::{Config, TokenStorage};

use crate::commands::mcp_session::{
    McpCalls, RefreshingRpc, SessionLookup, backend_name, find_stored_tokens, is_session_expired,
    lookup_session, remap_401_to_session_expired,
};
use crate::commands::{account_add, mcp};

pub async fn run(url: String, store: TokenStorage, yes: bool, json_output: bool) -> Result<()> {
    let result = if crate::guardrail::dry_run_active() {
        run_dry(&url, store, json_output).await
    } else {
        let (yes, note) = effective_yes(yes, std::io::stdin().is_terminal());
        if let Some(note) = note {
            eprintln!("{note}");
        }
        let result = run_inner(&url, store, yes, json_output).await;
        if result.is_ok() && !json_output {
            eprintln!();
            eprintln!("{}", ROUTING_HINT);
        }
        result
    };
    result.map_err(|e| mcp::remap_session_expired(&url, e))
}

/// Printed after a successful connect: a harness with several connectors
/// picks by name, and an idle Gmail or Google Calendar connector can win a
/// "send an e-mail" request. One line in the harness's instructions or
/// memory settles it; see docs/mcp.md "Make sure the agent picks pidge".
const ROUTING_HINT: &str = "Tip: so your AI harness always uses pidge for mail, add this line to its instructions \
or memory (CLAUDE.md, AGENTS.md, project instructions):\n\
  E-mail and calendar go through the pidge MCP tools (mail_*, calendar_*, accounts_*), \
never another mail connector.";

/// Whether to poll for each mailbox rather than wait for Enter. Without a
/// terminal on stdin (an agent, a pipe, `< /dev/null`), waiting for Enter
/// would read EOF at once and rush through every link, so `connect`
/// behaves as if `--yes` were given and says so in the returned note.
fn effective_yes(yes: bool, stdin_is_terminal: bool) -> (bool, Option<&'static str>) {
    if yes || stdin_is_terminal {
        (yes, None)
    } else {
        (
            true,
            Some(
                "stdin is not a terminal: acting as --yes (polling until each mailbox shows as connected).",
            ),
        )
    }
}

/// `--dry-run`: report what `connect` would do without touching the server,
/// refreshing anything, or running an interactive sign-in. If there's no
/// usable stored session, says sign-in would be required and stops there.
/// If the stored session is close enough to expiry that using it would
/// require a refresh, says so and stops rather than performing that
/// refresh. Otherwise makes exactly one `accounts_list` read (via
/// [`plan_dry_run`]) and reports which local accounts would be connected
/// and which settings would be copied.
async fn run_dry(url: &str, store: TokenStorage, json_output: bool) -> Result<()> {
    let Some((tokens, backend)) = find_stored_tokens(url, store)? else {
        if json_output {
            println!(
                "{}",
                json!({ "dry_run": true, "server": url, "signed_in": false })
            );
        } else {
            println!(
                "Dry run: not signed in to {url} yet; `pidge mcp connect {url}` would first open a browser to sign in."
            );
        }
        return Ok(());
    };

    if tokens.needs_refresh() {
        if json_output {
            println!(
                "{}",
                json!({
                    "dry_run": true,
                    "server": tokens.server,
                    "signed_in": true,
                    "store": backend_name(backend),
                    "token_near_expiry": true,
                })
            );
        } else {
            println!(
                "Dry run: signed in to {} ({} backend), but the access token is near expiry.",
                tokens.server.cyan(),
                backend_name(backend)
            );
            println!("  skipping the live accounts_list call; a dry run never refreshes tokens.");
            println!(
                "  run `pidge mcp connect {url}` (without --dry-run) to see the current plan."
            );
        }
        return Ok(());
    }

    let http = reqwest::Client::new();
    let mut raw = McpRpc::new(http, tokens.server.clone(), tokens.access_token.clone());
    raw.initialize()
        .await
        .map_err(|e| remap_401_to_session_expired(&tokens.server, e))
        .context("failed to initialize the MCP session")?;

    let config = Config::load()?;
    let local_accounts: Vec<String> = config.accounts.iter().map(|a| a.email.clone()).collect();
    let plan = plan_dry_run(
        &mut raw,
        &local_accounts,
        config.defaults.send.as_deref(),
        &config.trusted_senders,
    )
    .await?;

    if json_output {
        println!(
            "{}",
            json!({
                "dry_run": true,
                "server": tokens.server,
                "signed_in": true,
                "store": backend_name(backend),
                "would_connect": plan.would_connect,
                "would_copy": plan.would_copy,
            })
        );
    } else {
        println!(
            "Dry run against {} (signed in via the {} backend):",
            tokens.server.cyan(),
            backend_name(backend)
        );
        if plan.would_connect.is_empty() {
            println!("  every local account is already connected");
        } else {
            println!("  would connect: {}", plan.would_connect.join(", "));
        }
        if plan.would_copy.is_empty() {
            println!("  no settings to copy");
        } else {
            println!("  would copy: {}", plan.would_copy.join(", "));
        }
    }
    Ok(())
}

/// The `--dry-run` plan for an already-signed-in session: which local
/// accounts would be connected, and which settings would be copied. Takes
/// `calls` generically (over [`McpCalls`]) purely so it's unit-testable
/// with a recording fake: a test can assert this makes exactly one
/// `accounts_list` call and nothing else, since a dry run must never call
/// `accounts_connect`/`accounts_update`.
struct DryRunPlan {
    would_connect: Vec<String>,
    would_copy: Vec<String>,
}

async fn plan_dry_run<C: McpCalls>(
    calls: &mut C,
    local_accounts: &[String],
    default_send: Option<&str>,
    trusted_senders: &[String],
) -> Result<DryRunPlan> {
    let list_text = calls
        .call_tool("accounts_list", json!({}))
        .await
        .context("accounts_list failed")?
        .text;
    let already_connected = parse_connected_addresses(&list_text);

    let would_connect: Vec<String> = local_accounts
        .iter()
        .filter(|email| !already_connected.contains(&email.to_lowercase()))
        .cloned()
        .collect();

    let mut would_copy = Vec::new();
    if let Some(default_send) = default_send
        && (already_connected.contains(&default_send.to_lowercase())
            || would_connect
                .iter()
                .any(|e| e.eq_ignore_ascii_case(default_send)))
    {
        would_copy.push(format!("default_sender={default_send}"));
    }
    for sender in trusted_senders {
        would_copy.push(format!("trust={sender}"));
    }

    Ok(DryRunPlan {
        would_connect,
        would_copy,
    })
}

async fn run_inner(url: &str, store: TokenStorage, yes: bool, json_output: bool) -> Result<()> {
    let http = reqwest::Client::new();
    let (tokens, backend) = ensure_signed_in(&http, url, store).await?;

    let raw = McpRpc::new(
        http.clone(),
        tokens.server.clone(),
        tokens.access_token.clone(),
    );
    let mut rpc = RefreshingRpc::new(http, raw, tokens.clone(), backend);
    rpc.initialize()
        .await
        .context("failed to initialize the MCP session")?;

    let config = Config::load()?;
    let local_accounts: Vec<String> = config.accounts.iter().map(|a| a.email.clone()).collect();

    let list_text = rpc
        .call_tool("accounts_list", json!({}))
        .await
        .context("accounts_list failed")?
        .text;
    let already_connected = parse_connected_addresses(&list_text);

    eprintln!();
    eprintln!("Migrating local accounts to {}", tokens.server.cyan());

    let calls = Mutex::new(rpc);
    let outcome = run_migration(
        &local_accounts,
        &already_connected,
        &calls,
        |connect_url, email| {
            let calls = &calls;
            async move { wait_for_connection(calls, &connect_url, &email, yes).await }
        },
    )
    .await?;
    let mut rpc = calls.into_inner();

    if !json_output {
        for email in &outcome.skipped {
            eprintln!("{}", already_connected_line(email));
        }
    }

    let connected = finish_migration(
        &mut rpc,
        url,
        &outcome,
        config.defaults.send.as_deref(),
        &config.trusted_senders,
    )
    .await?;

    let final_text = rpc
        .call_tool("accounts_list", json!({}))
        .await
        .context("accounts_list failed")?
        .text;

    if json_output {
        println!(
            "{}",
            json!({
                "server": tokens.server,
                "connected": connected,
                "skipped": outcome.skipped,
            })
        );
    } else {
        println!();
        println!("{final_text}");
    }

    Ok(())
}

/// Reuse a stored session if one is valid (or refreshable), signing in
/// fresh otherwise. Returns the tokens and the backend they were actually
/// found in or saved to, which can differ from `requested_store` (see
/// [`try_existing_session`]).
async fn ensure_signed_in(
    http: &reqwest::Client,
    url: &str,
    requested_store: TokenStorage,
) -> Result<(McpTokens, TokenStorage)> {
    if let Some((tokens, backend)) = try_existing_session(http, url, requested_store).await? {
        if backend != requested_store {
            eprintln!(
                "Using the existing session from the {} backend (requested {}); pass --store {} to store new sign-ins there too.",
                backend_name(backend),
                backend_name(requested_store),
                backend_name(backend)
            );
        }
        return Ok((tokens, backend));
    }

    eprintln!();
    eprintln!("Signing in to the hosted pidge MCP server.");
    eprintln!();
    eprintln!("{}", "A browser window will open for sign-in.".dimmed());
    eprintln!();

    let tokens = sign_in(http, url, "pidge", |authorize_url| {
        eprintln!("{} {}", "Sign in at:".bold(), authorize_url.cyan());
        eprintln!(
            "{}",
            "(opening your browser…  Ctrl-C here to cancel)".dimmed()
        );
        eprintln!(
            "{}",
            "If the browser shows an error page, press Ctrl-C: the server does not redirect refusals back here."
                .dimmed()
        );
        let _ = account_add::open_browser(authorize_url);
    })
    .await
    .context("MCP sign-in failed")?;

    McpTokenStore::save(&tokens, requested_store)?;
    eprintln!();
    eprintln!("{} Signed in to {}", "✔".green(), tokens.server);
    Ok((tokens, requested_store))
}

/// Reuse a stored session if one is valid (or refreshable), reporting
/// `None` (never triggering an interactive sign-in itself) otherwise.
/// Thin adapter over the shared [`lookup_session`], which `ensure_signed_in`
/// then falls back to an interactive sign-in for.
async fn try_existing_session(
    http: &reqwest::Client,
    url: &str,
    preferred: TokenStorage,
) -> Result<Option<(McpTokens, TokenStorage)>> {
    match lookup_session(http, url, preferred).await? {
        SessionLookup::Found(tokens, backend) => Ok(Some((tokens, backend))),
        SessionLookup::Expired | SessionLookup::Absent => Ok(None),
    }
}

/// The outcome of one [`run_migration`] run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// Local accounts `accounts_connect`+wait completed for, in the order
    /// they were processed. Not yet verified against the server; the
    /// caller re-checks `accounts_list` afterwards (see `run_inner`).
    pub connected: Vec<String>,
    /// Local accounts that were already connected and so were skipped.
    pub skipped: Vec<String>,
}

/// For every address in `local_accounts` not already present in
/// `already_connected`, call `accounts_connect`, extract its connect link,
/// and hand it to `on_link(url, email)`; the caller's job is to show the
/// link (and, for the real CLI, wait for the user to finish, interactively
/// or by polling). Already-connected addresses are skipped without calling
/// `accounts_connect` at all.
///
/// `calls` is shared behind a `tokio::sync::Mutex` rather than taken as
/// `&mut` so that `on_link` can also reach it (e.g. to poll `accounts_list`
/// while waiting) without fighting the borrow checker; every lock here is
/// released before the next `.await` point that might re-enter it. A plain
/// `std::cell::RefCell` cannot be used here: clippy (rightly) flags a
/// borrow held across an `.await`, since a real concurrent poll of the
/// same cell would panic; a `tokio::sync::Mutex` is built for exactly this.
pub async fn run_migration<C, F, Fut>(
    local_accounts: &[String],
    already_connected: &HashSet<String>,
    calls: &Mutex<C>,
    mut on_link: F,
) -> Result<MigrationOutcome>
where
    C: McpCalls,
    F: FnMut(String, String) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let mut connected = Vec::new();
    let mut skipped = Vec::new();

    for email in local_accounts {
        if already_connected.contains(&email.to_lowercase()) {
            skipped.push(email.clone());
            continue;
        }

        let result = {
            let mut c = calls.lock().await;
            c.call_tool("accounts_connect", json!({ "email": email }))
                .await?
        };
        let url = extract_connect_url(&result.text).ok_or_else(|| {
            anyhow::anyhow!(
                "accounts_connect returned no connect link for {email}: {}",
                result.text
            )
        })?;

        on_link(url, email.clone()).await?;
        connected.push(email.clone());
    }

    Ok(MigrationOutcome { connected, skipped })
}

/// After the migration loop: re-verify who's actually connected (never
/// trust an Enter keypress, or even a completed poll, at face value), then
/// copy `default_send` (only for a *verified* address) and every trusted
/// sender onto the server. Each `accounts_update` failure is a warning on
/// stderr, never fatal, so one rejected update doesn't stop the next or
/// swallow the summary. Returns the verified subset of `outcome.connected`
/// to report as the run's `connected` list; an account the loop thought it
/// connected but that doesn't verify gets a stderr warning instead and is
/// dropped from that list. Takes `calls` generically (over [`McpCalls`]) so
/// it's unit-testable with a recording fake, with no network involved.
async fn finish_migration<C: McpCalls>(
    calls: &mut C,
    url: &str,
    outcome: &MigrationOutcome,
    default_send: Option<&str>,
    trusted_senders: &[String],
) -> Result<Vec<String>> {
    let verify_text = calls
        .call_tool("accounts_list", json!({}))
        .await
        .context("accounts_list failed")?
        .text;
    let verified = parse_connected_addresses(&verify_text);

    let mut connected = Vec::new();
    for email in &outcome.connected {
        if verified.contains(&email.to_lowercase()) {
            connected.push(email.clone());
        } else {
            eprintln!(
                "warning: {email} did not show as connected after signing in; run `pidge mcp connect {url}` again to finish."
            );
        }
    }

    if let Some(default_send) = default_send
        && verified.contains(&default_send.to_lowercase())
        && let Err(e) = calls
            .call_tool("accounts_update", json!({ "default_sender": default_send }))
            .await
    {
        eprintln!("warning: could not set default sender to {default_send}: {e:#}");
    }
    for sender in trusted_senders {
        if let Err(e) = calls
            .call_tool("accounts_update", json!({ "trust": sender }))
            .await
        {
            eprintln!("warning: could not add trusted sender {sender}: {e:#}");
        }
    }

    Ok(connected)
}

/// The line printed for each account that was already connected and so
/// skipped. Factored out (pure, no I/O) so its wording is directly
/// unit-testable.
fn already_connected_line(addr: &str) -> String {
    format!("{} {} already connected", "✔".green(), addr.bold())
}

/// Print/open the connect link for `email`, then wait for the user to
/// finish signing in: with `yes`, poll `accounts_list` until `email` shows
/// as connected, the deadline passes, or the session turns out to be dead;
/// otherwise, print a prompt and block for Enter. Returns `Err` only for a
/// genuine I/O error reading stdin, or (via [`poll_until_connected`]) when
/// the hosted session itself has expired; anything else (a timed-out poll,
/// a transient `accounts_list` failure) is a warning; the caller
/// re-verifies who's actually connected once the whole migration is done.
async fn wait_for_connection<C: McpCalls>(
    calls: &Mutex<C>,
    connect_url: &str,
    email: &str,
    yes: bool,
) -> Result<()> {
    eprintln!();
    eprintln!("Connect {}: {}", email.bold(), connect_url.cyan());
    eprintln!("{}", "(opening your browser…)".dimmed());
    let _ = account_add::open_browser(connect_url);

    if yes {
        poll_until_connected(calls, email).await?;
    } else {
        eprintln!(
            "{}",
            "Press Enter once you've finished signing in.".dimmed()
        );
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
    }
    Ok(())
}

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_POLL_WAIT: Duration = Duration::from_secs(10 * 60);

async fn poll_until_connected<C: McpCalls>(calls: &Mutex<C>, email: &str) -> Result<()> {
    poll_until_connected_with(calls, email, POLL_INTERVAL, MAX_POLL_WAIT).await
}

/// `poll_until_connected`, parameterized on interval/deadline so tests can
/// drive it with tiny durations under `tokio::test(start_paused = true)`
/// instead of waiting for real minutes.
///
/// A dead session ([`is_session_expired`]) bails immediately; otherwise
/// this would spend its whole deadline re-POSTing a revoked refresh token
/// every interval, only for the *next* account's `accounts_connect` to
/// surface the reconnect hint. Every other `accounts_list` failure is
/// transient and just a warning, retried until the deadline; a timeout is
/// also just a warning; the caller moves on to the next account and
/// re-verifies who actually connected once the whole migration is done.
async fn poll_until_connected_with<C: McpCalls>(
    calls: &Mutex<C>,
    email: &str,
    interval: Duration,
    max_wait: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + max_wait;
    let target = email.to_lowercase();
    loop {
        let outcome = {
            let mut c = calls.lock().await;
            c.call_tool("accounts_list", json!({})).await
        };
        match outcome {
            Ok(result) if parse_connected_addresses(&result.text).contains(&target) => {
                return Ok(());
            }
            Ok(_) => {}
            Err(e) if is_session_expired(&e) => return Err(e),
            Err(e) => {
                eprintln!(
                    "warning: accounts_list failed while waiting for {email} to connect: {e:#} (retrying)"
                );
            }
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!(
                "warning: timed out waiting for {email} to connect; continuing with the rest"
            );
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
}

/// Extract the set of e-mail addresses appearing in `accounts_list`'s
/// *healthy* mailbox lines (rendered by the server, one per line, as
/// `  - <addr>[ (sign-in)]: <health>`; see `pidge-mcp`'s
/// `tools::accounts::render_accounts`). Tolerant of the exact formatting: a
/// line only needs to start with `-` once trimmed, and any whitespace-
/// separated token on it containing `@` counts, stripped of surrounding
/// punctuation and lower-cased. Lines that don't start with `-` (the
/// sign-in/default-sender/trusted-senders lines, which may also contain
/// addresses) are ignored, and so are mailbox lines whose health reads
/// "needs reconnect"; the server is telling us to run `accounts_connect`
/// for that address again, not that it's already connected.
pub fn parse_connected_addresses(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix('-'))
        .filter(|rest| !rest.to_lowercase().contains("needs reconnect"))
        .flat_map(str::split_whitespace)
        .filter(|token| token.contains('@'))
        .map(|token| {
            token
                .trim_matches(|c: char| !(c.is_alphanumeric() || "@.+-_".contains(c)))
                .to_lowercase()
        })
        .filter(|addr| !addr.is_empty())
        .collect()
}

/// Pull the `https://…/connect?state=…` URL out of an `accounts_connect`
/// response's text (see `pidge-mcp`'s `tools::accounts::accounts_connect`).
/// Tolerant of surrounding prose: it looks for any whitespace-separated
/// token that starts with `http` and contains `/connect?state=`.
pub fn extract_connect_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|token| token.starts_with("http") && token.contains("/connect?state="))
        .map(|token| token.trim_end_matches([')', '.', ',', ';']).to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pidge_client::ClientError;
    use pidge_client::mcp::ToolResult;

    use super::*;

    #[test]
    fn effective_yes_polls_when_stdin_is_not_a_terminal() {
        let (yes, note) = effective_yes(false, false);
        assert!(yes);
        assert!(note.unwrap().contains("--yes"));
    }

    #[test]
    fn effective_yes_keeps_the_flag_on_a_terminal_or_when_given() {
        assert_eq!(effective_yes(false, true), (false, None));
        assert_eq!(effective_yes(true, true), (true, None));
        assert_eq!(effective_yes(true, false), (true, None));
    }

    // --- parse_connected_addresses -----------------------------------

    const ACCOUNTS_LIST_TEXT: &str = "signed in as: jane@example.com\n\
        default sender: jane@example.com\n\
        timezone: Europe/Stockholm\n\
        mailboxes:\n\
        \x20 - jane@example.com (sign-in): ok\n\
        \x20 - work@example.com: needs reconnect\n\
        trusted senders: colleague@example.com\n\
        next: accounts_connect email=<mailbox> to reconnect";

    #[test]
    fn parse_connected_addresses_extracts_healthy_mailbox_lines_only() {
        let addrs = parse_connected_addresses(ACCOUNTS_LIST_TEXT);
        assert_eq!(addrs, HashSet::from(["jane@example.com".to_string()]));
        // The trusted-senders line isn't a mailbox line; it must not
        // contribute an address, even though it contains an `@`.
        assert!(!addrs.contains("colleague@example.com"));
    }

    #[test]
    fn parse_connected_addresses_excludes_needs_reconnect_mailboxes() {
        let text = "mailboxes:\n  - work@example.com: needs reconnect\n";
        assert!(
            parse_connected_addresses(text).is_empty(),
            "a mailbox needing reconnect must not count as connected"
        );
    }

    #[test]
    fn parse_connected_addresses_is_case_insensitive() {
        let text = "mailboxes:\n  - Jane@Example.com (sign-in): ok\n";
        let addrs = parse_connected_addresses(text);
        assert!(addrs.contains("jane@example.com"));
    }

    #[test]
    fn parse_connected_addresses_returns_empty_set_for_no_mailboxes() {
        assert!(parse_connected_addresses("mailboxes:\n").is_empty());
    }

    // --- extract_connect_url -------------------------------------------

    const ACCOUNTS_CONNECT_TEXT: &str = "Connect work@example.com by opening: \
        https://mcp.example.com/connect?state=abc123  (valid 10 minutes)\n\
        The user signs in at Microsoft with that mailbox's account.\n\
        next: accounts_list once they have finished";

    #[test]
    fn extract_connect_url_pulls_the_link_out_of_accounts_connect_text() {
        assert_eq!(
            extract_connect_url(ACCOUNTS_CONNECT_TEXT).as_deref(),
            Some("https://mcp.example.com/connect?state=abc123")
        );
    }

    #[test]
    fn extract_connect_url_returns_none_when_absent() {
        assert_eq!(extract_connect_url("no link here"), None);
    }

    // --- already_connected_line ------------------------------------------

    #[test]
    fn already_connected_line_names_the_address_and_says_already_connected() {
        let line = already_connected_line("jane@example.com");
        assert!(line.contains("jane@example.com"), "{line}");
        assert!(line.to_lowercase().contains("already connected"), "{line}");
    }

    // --- run_migration ---------------------------------------------------

    #[derive(Default)]
    struct FakeRpc {
        connect_calls: Vec<String>,
    }

    impl McpCalls for FakeRpc {
        async fn call_tool(
            &mut self,
            name: &str,
            arguments: serde_json::Value,
        ) -> Result<ToolResult> {
            match name {
                "accounts_connect" => {
                    let email = arguments["email"].as_str().unwrap_or_default().to_string();
                    self.connect_calls.push(email.clone());
                    Ok(ToolResult {
                        text: format!(
                            "Connect {email} by opening: https://mcp.example.com/connect?state=fake-{email}  (valid 10 minutes)"
                        ),
                        is_error: false,
                    })
                }
                other => panic!("unexpected tool call in test: {other}"),
            }
        }
    }

    #[tokio::test]
    async fn run_migration_skips_already_connected_accounts_and_connects_only_the_rest() {
        let local = vec![
            "a@x.com".to_string(),
            "b@x.com".to_string(),
            "c@x.com".to_string(),
        ];
        let connected_already: HashSet<String> = ["a@x.com".to_string()].into_iter().collect();
        let calls = Mutex::new(FakeRpc::default());
        let waited: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let outcome = run_migration(&local, &connected_already, &calls, |url, email| {
            let waited = waited.clone();
            async move {
                assert!(url.contains(&email), "link should mention {email}: {url}");
                waited.lock().unwrap().push(email);
                Ok(())
            }
        })
        .await
        .unwrap();

        assert_eq!(outcome.skipped, vec!["a@x.com".to_string()]);
        assert_eq!(
            outcome.connected,
            vec!["b@x.com".to_string(), "c@x.com".to_string()]
        );
        assert_eq!(
            calls.lock().await.connect_calls,
            vec!["b@x.com".to_string(), "c@x.com".to_string()]
        );
        assert_eq!(
            *waited.lock().unwrap(),
            vec!["b@x.com".to_string(), "c@x.com".to_string()]
        );
    }

    #[tokio::test]
    async fn run_migration_skips_every_account_when_all_are_already_connected() {
        let local = vec!["a@x.com".to_string(), "b@x.com".to_string()];
        let connected_already: HashSet<String> = local.iter().cloned().collect();
        let calls = Mutex::new(FakeRpc::default());

        let outcome = run_migration(&local, &connected_already, &calls, |_url, _email| async {
            panic!("on_link should not be called when every account is already connected");
        })
        .await
        .unwrap();

        assert!(outcome.connected.is_empty());
        assert_eq!(outcome.skipped, local);
        assert!(calls.lock().await.connect_calls.is_empty());
    }

    #[tokio::test]
    async fn run_migration_errors_when_accounts_connect_response_has_no_link() {
        struct NoLinkRpc;
        impl McpCalls for NoLinkRpc {
            async fn call_tool(
                &mut self,
                _name: &str,
                _arguments: serde_json::Value,
            ) -> Result<ToolResult> {
                Ok(ToolResult {
                    text: "no link here".into(),
                    is_error: false,
                })
            }
        }
        let local = vec!["a@x.com".to_string()];
        let connected_already = HashSet::new();
        let calls = Mutex::new(NoLinkRpc);

        let err = run_migration(&local, &connected_already, &calls, |_url, _email| async {
            Ok(())
        })
        .await
        .unwrap_err();

        assert!(err.to_string().contains("a@x.com"), "{err}");
    }

    // --- poll_until_connected_with ---------------------------------------

    #[tokio::test(start_paused = true)]
    async fn poll_until_connected_returns_once_the_address_appears() {
        struct EventuallyRpc {
            calls: usize,
        }
        impl McpCalls for EventuallyRpc {
            async fn call_tool(
                &mut self,
                _name: &str,
                _arguments: serde_json::Value,
            ) -> Result<ToolResult> {
                self.calls += 1;
                let text = if self.calls >= 3 {
                    "mailboxes:\n  - a@x.com: ok\n"
                } else {
                    "mailboxes:\n"
                };
                Ok(ToolResult {
                    text: text.to_string(),
                    is_error: false,
                })
            }
        }
        let calls = Mutex::new(EventuallyRpc { calls: 0 });

        poll_until_connected_with(
            &calls,
            "a@x.com",
            Duration::from_millis(1),
            Duration::from_secs(60),
        )
        .await
        .unwrap();

        assert!(calls.lock().await.calls >= 3);
    }

    #[tokio::test(start_paused = true)]
    async fn poll_until_connected_retries_transient_errors_and_warns_on_timeout() {
        struct FlakyRpc {
            calls: usize,
        }
        impl McpCalls for FlakyRpc {
            async fn call_tool(
                &mut self,
                _name: &str,
                _arguments: serde_json::Value,
            ) -> Result<ToolResult> {
                self.calls += 1;
                Err(anyhow::anyhow!("transient network blip"))
            }
        }
        let calls = Mutex::new(FlakyRpc { calls: 0 });

        // Never succeeds; this must return Ok (with a timeout warning)
        // rather than propagate the transient error or hang.
        poll_until_connected_with(
            &calls,
            "a@x.com",
            Duration::from_millis(1),
            Duration::from_millis(5),
        )
        .await
        .unwrap();

        assert!(
            calls.lock().await.calls >= 2,
            "should have retried at least once before the deadline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn poll_until_connected_bails_immediately_on_a_dead_session() {
        struct DeadSessionRpc {
            calls: usize,
        }
        impl McpCalls for DeadSessionRpc {
            async fn call_tool(
                &mut self,
                _name: &str,
                _arguments: serde_json::Value,
            ) -> Result<ToolResult> {
                self.calls += 1;
                Err(ClientError::SessionExpired {
                    email: "https://mcp.example.com/mcp".into(),
                }
                .into())
            }
        }
        let calls = Mutex::new(DeadSessionRpc { calls: 0 });

        // A generous deadline; if this retried instead of bailing, the
        // (paused) clock would need to advance the full 60s for the loop
        // to time out, and the test would still pass for the wrong reason,
        // so the real assertion is the call count below.
        let err = poll_until_connected_with(
            &calls,
            "a@x.com",
            Duration::from_millis(1),
            Duration::from_secs(60),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err.downcast_ref::<ClientError>(),
            Some(ClientError::SessionExpired { .. })
        ));
        assert_eq!(
            calls.lock().await.calls,
            1,
            "must bail on the first session-expired error, not retry until the deadline"
        );
    }

    // --- finish_migration / plan_dry_run --------------------------------

    /// Records every call made to it and answers `accounts_list` with a
    /// fixed, settable text; an `accounts_update {"trust": <addr>}` for an
    /// address in `fail_trust` errors, everything else succeeds. Lets N3's
    /// tests drive `finish_migration`/`plan_dry_run` without any network.
    #[derive(Default)]
    struct RecordingRpc {
        calls: Vec<(String, serde_json::Value)>,
        accounts_list_text: String,
        fail_trust: HashSet<String>,
    }

    impl McpCalls for RecordingRpc {
        async fn call_tool(
            &mut self,
            name: &str,
            arguments: serde_json::Value,
        ) -> Result<ToolResult> {
            self.calls.push((name.to_string(), arguments.clone()));
            match name {
                "accounts_list" => Ok(ToolResult {
                    text: self.accounts_list_text.clone(),
                    is_error: false,
                }),
                "accounts_update" => {
                    if let Some(trust) = arguments.get("trust").and_then(|v| v.as_str())
                        && self.fail_trust.contains(trust)
                    {
                        return Err(anyhow::anyhow!("simulated failure for {trust}"));
                    }
                    Ok(ToolResult {
                        text: "Saved.".into(),
                        is_error: false,
                    })
                }
                other => panic!("unexpected tool call in test: {other}"),
            }
        }
    }

    impl RecordingRpc {
        fn with_list_text(text: &str) -> Self {
            Self {
                accounts_list_text: text.to_string(),
                ..Default::default()
            }
        }

        fn update_calls_with(&self, key: &str) -> Vec<&str> {
            self.calls
                .iter()
                .filter(|(name, _)| name == "accounts_update")
                .filter_map(|(_, args)| args.get(key).and_then(|v| v.as_str()))
                .collect()
        }
    }

    #[tokio::test]
    async fn finish_migration_does_not_set_default_sender_for_an_unverified_account() {
        // `work@example.com` was Enter-confirmed (in `outcome.connected`)
        // but the verify pass shows only `jane@example.com` as connected;
        // the default-sender update must not be sent for it.
        let mut rpc = RecordingRpc::with_list_text("mailboxes:\n  - jane@example.com: ok\n");
        let outcome = MigrationOutcome {
            connected: vec!["work@example.com".to_string()],
            skipped: vec![],
        };

        let connected = finish_migration(
            &mut rpc,
            "https://mcp.example.com",
            &outcome,
            Some("work@example.com"),
            &[],
        )
        .await
        .unwrap();

        assert!(
            connected.is_empty(),
            "an unverified account must not be reported as connected: {connected:?}"
        );
        assert!(
            rpc.update_calls_with("default_sender").is_empty(),
            "default_sender must not be sent for an account that never verified: {:?}",
            rpc.calls
        );
    }

    #[tokio::test]
    async fn finish_migration_sets_default_sender_and_reports_a_verified_account() {
        let mut rpc = RecordingRpc::with_list_text(
            "mailboxes:\n  - jane@example.com: ok\n  - work@example.com: ok\n",
        );
        let outcome = MigrationOutcome {
            connected: vec!["work@example.com".to_string()],
            skipped: vec!["jane@example.com".to_string()],
        };

        let connected = finish_migration(
            &mut rpc,
            "https://mcp.example.com",
            &outcome,
            Some("work@example.com"),
            &[],
        )
        .await
        .unwrap();

        assert_eq!(connected, vec!["work@example.com".to_string()]);
        assert_eq!(
            rpc.update_calls_with("default_sender"),
            vec!["work@example.com"]
        );
    }

    #[tokio::test]
    async fn finish_migration_continues_past_a_failing_trust_update() {
        let mut rpc = RecordingRpc::with_list_text("mailboxes:\n  - jane@example.com: ok\n");
        rpc.fail_trust.insert("bad@example.com".to_string());
        let trusted = vec![
            "bad@example.com".to_string(),
            "good@example.com".to_string(),
        ];

        let connected = finish_migration(
            &mut rpc,
            "https://mcp.example.com",
            &MigrationOutcome::default(),
            None,
            &trusted,
        )
        .await
        .unwrap();

        assert!(connected.is_empty());
        assert_eq!(
            rpc.update_calls_with("trust"),
            vec!["bad@example.com", "good@example.com"],
            "a failing trust update must not stop the next one from being attempted"
        );
    }

    #[tokio::test]
    async fn plan_dry_run_only_calls_accounts_list() {
        let mut rpc = RecordingRpc::with_list_text("mailboxes:\n  - jane@example.com: ok\n");
        let local = vec![
            "jane@example.com".to_string(),
            "work@example.com".to_string(),
        ];

        let plan = plan_dry_run(
            &mut rpc,
            &local,
            Some("jane@example.com"),
            &["trusted@example.com".to_string()],
        )
        .await
        .unwrap();

        assert_eq!(plan.would_connect, vec!["work@example.com".to_string()]);
        assert_eq!(
            plan.would_copy,
            vec![
                "default_sender=jane@example.com".to_string(),
                "trust=trusted@example.com".to_string(),
            ]
        );
        assert_eq!(
            rpc.calls
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["accounts_list"],
            "a dry run must make exactly one accounts_list call and nothing else: {:?}",
            rpc.calls
        );
    }
}
