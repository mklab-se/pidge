//! `pidge mcp connect <url>` — sign in to a hosted pidge MCP server and
//! migrate every locally signed-in Microsoft account onto it.
//!
//! The migration loop (skip already-connected mailboxes, call
//! `accounts_connect` for the rest, wait for each to finish) is factored out
//! as [`run_migration`] over a small [`McpCalls`] trait so it can be driven
//! against a fake in tests, with no network involved.
//!
//! Output contract: all progress/human text (sign-in prompts, the authorize
//! URL, each connect link, the Enter prompt, "already connected" notes, and
//! warnings) goes to stderr, unconditionally — `--json` does not suppress
//! it, since the connect link is often the only way to finish the flow.
//! Stdout carries only the one final payload: the `accounts_list` text, or
//! (with `--json`) the `{"server","connected","skipped"}` summary.

use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::json;
use tokio::sync::Mutex;

use pidge_client::ClientError;
use pidge_client::mcp::{
    McpRpc, McpTokenStore, McpTokens, ToolResult, sign_in, valid_access_token,
};
use pidge_core::{Config, TokenStorage};

use crate::commands::{account_add, mcp};

pub async fn run(url: String, store: TokenStorage, yes: bool, json_output: bool) -> Result<()> {
    let result = if crate::guardrail::dry_run_active() {
        run_dry(&url, store, json_output).await
    } else {
        run_inner(&url, store, yes, json_output).await
    };
    result.map_err(|e| mcp::remap_session_expired(&url, e))
}

/// `--dry-run`: report what `connect` would do without touching the server
/// or running an interactive sign-in. If there's no usable stored session,
/// says sign-in would be required and stops there (a dry run must not open
/// a browser or block on anything). If there is one, makes exactly one
/// `accounts_list` read and reports which local accounts would be
/// connected and which settings would be copied.
async fn run_dry(url: &str, store: TokenStorage, json_output: bool) -> Result<()> {
    let http = reqwest::Client::new();
    let Some((tokens, backend)) = try_existing_session(&http, url, store).await? else {
        if json_output {
            println!(
                "{}",
                json!({ "dry_run": true, "server": url, "signed_in": false })
            );
        } else {
            println!(
                "Dry run: not signed in to {url} yet — `pidge mcp connect {url}` would first open a browser to sign in."
            );
        }
        return Ok(());
    };

    let mut raw = McpRpc::new(http, tokens.server.clone(), tokens.access_token.clone());
    raw.initialize()
        .await
        .context("failed to initialize the MCP session")?;
    let list_text = McpCalls::call_tool(&mut raw, "accounts_list", json!({}))
        .await
        .context("accounts_list failed")?
        .text;
    let already_connected = parse_connected_addresses(&list_text);

    let config = Config::load()?;
    let local_accounts: Vec<String> = config.accounts.iter().map(|a| a.email.clone()).collect();
    let would_connect: Vec<String> = local_accounts
        .iter()
        .filter(|email| !already_connected.contains(&email.to_lowercase()))
        .cloned()
        .collect();

    let mut would_copy = Vec::new();
    if let Some(default_send) = config.defaults.send.as_deref()
        && (already_connected.contains(&default_send.to_lowercase())
            || would_connect
                .iter()
                .any(|e| e.eq_ignore_ascii_case(default_send)))
    {
        would_copy.push(format!("default_sender={default_send}"));
    }
    for sender in &config.trusted_senders {
        would_copy.push(format!("trust={sender}"));
    }

    if json_output {
        println!(
            "{}",
            json!({
                "dry_run": true,
                "server": tokens.server,
                "signed_in": true,
                "store": backend_name(backend),
                "would_connect": would_connect,
                "would_copy": would_copy,
            })
        );
    } else {
        println!(
            "Dry run against {} (signed in via the {} backend):",
            tokens.server.cyan(),
            backend_name(backend)
        );
        if would_connect.is_empty() {
            println!("  every local account is already connected");
        } else {
            println!("  would connect: {}", would_connect.join(", "));
        }
        if would_copy.is_empty() {
            println!("  no settings to copy");
        } else {
            println!("  would copy: {}", would_copy.join(", "));
        }
    }
    Ok(())
}

async fn run_inner(url: &str, store: TokenStorage, yes: bool, json_output: bool) -> Result<()> {
    let http = reqwest::Client::new();
    let (tokens, backend) = ensure_signed_in(&http, url, store).await?;

    let mut raw = McpRpc::new(
        http.clone(),
        tokens.server.clone(),
        tokens.access_token.clone(),
    );
    raw.initialize()
        .await
        .context("failed to initialize the MCP session")?;
    let mut rpc = RefreshingRpc::new(http, raw, tokens.clone(), backend);

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

    // Don't trust an Enter keypress (or even a completed poll) at face
    // value: re-read accounts_list once, and only report/act on what the
    // server now actually shows as connected.
    let verify_text = rpc
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

    // Copy settings that only make sense once the account they refer to is
    // actually connected. A rejected or failing update is a warning, never
    // fatal — the rest of the summary still gets reported.
    if let Some(default_send) = config.defaults.send.as_deref()
        && verified.contains(&default_send.to_lowercase())
        && let Err(e) = rpc
            .call_tool("accounts_update", json!({ "default_sender": default_send }))
            .await
    {
        eprintln!("warning: could not set default sender to {default_send}: {e:#}");
    }
    for sender in &config.trusted_senders {
        if let Err(e) = rpc
            .call_tool("accounts_update", json!({ "trust": sender }))
            .await
        {
            eprintln!("warning: could not add trusted sender {sender}: {e:#}");
        }
    }

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
        let _ = account_add::open_browser(authorize_url);
    })
    .await
    .context("MCP sign-in failed")?;

    McpTokenStore::save(&tokens, requested_store)?;
    eprintln!();
    eprintln!("{} Signed in to {}", "✔".green(), tokens.server);
    Ok((tokens, requested_store))
}

/// Look for a usable stored session for `url`, preferring `preferred` but
/// falling back to the other backend on a miss — so `connect --store=file`
/// after an earlier `connect --store=keychain` reuses that session instead
/// of silently signing in again and leaving the keychain entry behind.
/// Only tries the second backend when the first has no entry at all; an
/// entry that exists but is irrecoverably expired (`SessionExpired`) is
/// reported as `None` without trying the other one. Never triggers an
/// interactive sign-in itself.
async fn try_existing_session(
    http: &reqwest::Client,
    url: &str,
    preferred: TokenStorage,
) -> Result<Option<(McpTokens, TokenStorage)>> {
    for backend in candidate_backends(preferred) {
        let Some(mut tokens) = McpTokenStore::load(url, backend)? else {
            continue;
        };
        let server = tokens.server.clone();
        return match valid_access_token(http, &server, &mut tokens).await {
            Ok(_) => {
                McpTokenStore::save(&tokens, backend)?;
                Ok(Some((tokens, backend)))
            }
            Err(ClientError::SessionExpired { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        };
    }
    Ok(None)
}

fn candidate_backends(preferred: TokenStorage) -> [TokenStorage; 2] {
    match preferred {
        TokenStorage::Keychain => [TokenStorage::Keychain, TokenStorage::File],
        TokenStorage::File => [TokenStorage::File, TokenStorage::Keychain],
    }
}

fn backend_name(store: TokenStorage) -> &'static str {
    match store {
        TokenStorage::Keychain => "keychain",
        TokenStorage::File => "file",
    }
}

/// Minimal tool-calling surface [`run_migration`] needs. Implemented for the
/// real [`McpRpc`] (bails on `ToolResult::is_error` via [`check_tool_result`])
/// and for [`RefreshingRpc`], and for fakes in tests — so the migration
/// loop's decision logic (who to skip, who to call `accounts_connect` for)
/// can be exercised with no network call.
pub trait McpCalls {
    async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<ToolResult>;
}

impl McpCalls for McpRpc {
    async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<ToolResult> {
        let result = McpRpc::call_tool(self, name, arguments).await?;
        check_tool_result(name, result)
    }
}

/// Turn a tool-level failure (`ToolResult::is_error`) into an error instead
/// of letting its text be misparsed as if it were a normal list/link. The
/// server reports failures as JSON-RPC errors today, so this only guards
/// against a tool that starts using `isError` for that in the future.
/// Factored out (pure, no network) so it's directly unit-testable.
fn check_tool_result(name: &str, result: ToolResult) -> Result<ToolResult> {
    if result.is_error {
        Err(anyhow::anyhow!("{name} reported an error: {}", result.text))
    } else {
        Ok(result)
    }
}

/// Wraps [`McpRpc`] so every [`McpCalls::call_tool`] first makes sure the
/// access token has more than [`McpTokens::needs_refresh`]'s margin left —
/// refreshing (and persisting the refresh through `store`) as needed. An
/// interactive migration can spend up to ten minutes per mailbox waiting on
/// the user, comfortably long enough to run past a token minted at the
/// start of the run, so this check happens immediately before every single
/// call rather than once at the start. A `401` that gets through anyway
/// (e.g. the server revoked the session between calls) is remapped to
/// [`ClientError::SessionExpired`] so it surfaces through
/// `commands::mcp::remap_session_expired`'s `pidge mcp connect <url>` hint
/// instead of a bare Graph error.
struct RefreshingRpc {
    http: reqwest::Client,
    inner: McpRpc,
    tokens: McpTokens,
    store: TokenStorage,
}

impl RefreshingRpc {
    fn new(http: reqwest::Client, inner: McpRpc, tokens: McpTokens, store: TokenStorage) -> Self {
        Self {
            http,
            inner,
            tokens,
            store,
        }
    }
}

impl McpCalls for RefreshingRpc {
    async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<ToolResult> {
        let server = self.tokens.server.clone();
        let previous_access_token = self.tokens.access_token.clone();
        let access_token = valid_access_token(&self.http, &server, &mut self.tokens).await?;
        if access_token != previous_access_token {
            self.inner.set_access_token(access_token);
            McpTokenStore::save(&self.tokens, self.store)?;
        }
        match <McpRpc as McpCalls>::call_tool(&mut self.inner, name, arguments).await {
            Ok(result) => Ok(result),
            Err(e) if is_unauthorized(&e) => {
                Err(ClientError::SessionExpired { email: server }.into())
            }
            Err(e) => Err(e),
        }
    }
}

fn is_unauthorized(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<ClientError>(),
        Some(ClientError::Graph { status: 401, .. })
    )
}

/// The outcome of one [`run_migration`] run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// Local accounts `accounts_connect`+wait completed for, in the order
    /// they were processed. Not yet verified against the server — the
    /// caller re-checks `accounts_list` afterwards (see `run_inner`).
    pub connected: Vec<String>,
    /// Local accounts that were already connected and so were skipped.
    pub skipped: Vec<String>,
}

/// For every address in `local_accounts` not already present in
/// `already_connected`, call `accounts_connect`, extract its connect link,
/// and hand it to `on_link(url, email)` — the caller's job is to show the
/// link (and, for the real CLI, wait for the user to finish, interactively
/// or by polling). Already-connected addresses are skipped without calling
/// `accounts_connect` at all.
///
/// `calls` is shared behind a `tokio::sync::Mutex` rather than taken as
/// `&mut` so that `on_link` can also reach it (e.g. to poll `accounts_list`
/// while waiting) without fighting the borrow checker; every lock here is
/// released before the next `.await` point that might re-enter it. A plain
/// `std::cell::RefCell` cannot be used here — clippy (rightly) flags a
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

/// The line printed for each account that was already connected and so
/// skipped. Factored out (pure, no I/O) so its wording is directly
/// unit-testable.
fn already_connected_line(addr: &str) -> String {
    format!("{} {} already connected", "✔".green(), addr.bold())
}

/// Print/open the connect link for `email`, then wait for the user to
/// finish signing in: with `yes`, poll `accounts_list` until `email` shows
/// as connected or the deadline passes; otherwise, print a prompt and block
/// for Enter. Always returns `Ok(())` except for a genuine I/O error
/// reading stdin — a timed-out poll warns and moves on rather than aborting
/// the rest of the migration (the caller re-verifies who's actually
/// connected afterwards).
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
        poll_until_connected(calls, email).await;
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

async fn poll_until_connected<C: McpCalls>(calls: &Mutex<C>, email: &str) {
    poll_until_connected_with(calls, email, POLL_INTERVAL, MAX_POLL_WAIT).await;
}

/// `poll_until_connected`, parameterized on interval/deadline so tests can
/// drive it with tiny durations under `tokio::test(start_paused = true)`
/// instead of waiting for real minutes. A transient `accounts_list` failure
/// is a warning, retried until the deadline rather than aborted; a timeout
/// is also just a warning — the caller moves on to the next account and
/// re-verifies who actually connected once the whole migration is done.
async fn poll_until_connected_with<C: McpCalls>(
    calls: &Mutex<C>,
    email: &str,
    interval: Duration,
    max_wait: Duration,
) {
    let deadline = tokio::time::Instant::now() + max_wait;
    let target = email.to_lowercase();
    loop {
        let outcome = {
            let mut c = calls.lock().await;
            c.call_tool("accounts_list", json!({})).await
        };
        match outcome {
            Ok(result) if parse_connected_addresses(&result.text).contains(&target) => return,
            Ok(_) => {}
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
            return;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Extract the set of e-mail addresses appearing in `accounts_list`'s
/// *healthy* mailbox lines (rendered by the server, one per line, as
/// `  - <addr>[ (sign-in)]: <health>` — see `pidge-mcp`'s
/// `tools::accounts::render_accounts`). Tolerant of the exact formatting: a
/// line only needs to start with `-` once trimmed, and any whitespace-
/// separated token on it containing `@` counts, stripped of surrounding
/// punctuation and lower-cased. Lines that don't start with `-` (the
/// sign-in/default-sender/trusted-senders lines, which may also contain
/// addresses) are ignored, and so are mailbox lines whose health reads
/// "needs reconnect" — the server is telling us to run `accounts_connect`
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

    use super::*;

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
        // The trusted-senders line isn't a mailbox line — it must not
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

    // --- check_tool_result -------------------------------------------------

    #[test]
    fn check_tool_result_bails_on_is_error() {
        let result = ToolResult {
            text: "boom".into(),
            is_error: true,
        };
        let err = check_tool_result("accounts_list", result).unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn check_tool_result_passes_through_ok_results() {
        let result = ToolResult {
            text: "fine".into(),
            is_error: false,
        };
        assert_eq!(
            check_tool_result("accounts_list", result).unwrap().text,
            "fine"
        );
    }

    // --- candidate_backends -----------------------------------------------

    #[test]
    fn candidate_backends_tries_the_preferred_backend_first() {
        assert_eq!(
            candidate_backends(TokenStorage::Keychain),
            [TokenStorage::Keychain, TokenStorage::File]
        );
        assert_eq!(
            candidate_backends(TokenStorage::File),
            [TokenStorage::File, TokenStorage::Keychain]
        );
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
        .await;

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

        // Never succeeds — this must return (with a timeout warning) rather
        // than propagate the transient error or hang.
        poll_until_connected_with(
            &calls,
            "a@x.com",
            Duration::from_millis(1),
            Duration::from_millis(5),
        )
        .await;

        assert!(
            calls.lock().await.calls >= 2,
            "should have retried at least once before the deadline"
        );
    }
}
