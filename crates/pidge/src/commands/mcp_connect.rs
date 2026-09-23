//! `pidge mcp connect <url>` — sign in to a hosted pidge MCP server and
//! migrate every locally signed-in Microsoft account onto it.
//!
//! The migration loop (skip already-connected mailboxes, call
//! `accounts_connect` for the rest, wait for each to finish) is factored out
//! as [`run_migration`] over a small [`McpCalls`] trait so it can be driven
//! against a fake in tests, with no network involved.

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
    match run_inner(&url, store, yes, json_output).await {
        Ok(()) => Ok(()),
        Err(e) => Err(mcp::remap_session_expired(&url, e)),
    }
}

async fn run_inner(url: &str, store: TokenStorage, yes: bool, json_output: bool) -> Result<()> {
    let http = reqwest::Client::new();
    let tokens = ensure_signed_in(&http, url, store).await?;

    let mut rpc = McpRpc::new(
        http.clone(),
        tokens.server.clone(),
        tokens.access_token.clone(),
    );
    rpc.initialize()
        .await
        .context("failed to initialize the MCP session")?;

    let local_accounts: Vec<String> = Config::load()?
        .accounts
        .into_iter()
        .map(|a| a.email)
        .collect();

    let list_text = rpc
        .call_tool("accounts_list", json!({}))
        .await
        .context("accounts_list failed")?
        .text;
    let already_connected = parse_connected_addresses(&list_text);

    if !json_output {
        println!();
        println!("Migrating local accounts to {}", tokens.server.cyan());
    }

    let calls = Mutex::new(rpc);
    let outcome = run_migration(
        &local_accounts,
        &already_connected,
        &calls,
        |connect_url, email| {
            let calls = &calls;
            async move { wait_for_connection(calls, &connect_url, &email, yes, json_output).await }
        },
    )
    .await?;
    let mut rpc = calls.into_inner();

    // Copy settings that only make sense once the account they refer to is
    // actually connected on the server.
    let config = Config::load()?;
    let now_connected = |addr: &str| {
        already_connected.contains(&addr.to_lowercase())
            || outcome
                .connected
                .iter()
                .any(|e| e.eq_ignore_ascii_case(addr))
    };
    if let Some(default_send) = config.defaults.send.as_deref()
        && now_connected(default_send)
    {
        rpc.call_tool("accounts_update", json!({ "default_sender": default_send }))
            .await
            .context("accounts_update (default_sender) failed")?;
    }
    for sender in &config.trusted_senders {
        rpc.call_tool("accounts_update", json!({ "trust": sender }))
            .await
            .context("accounts_update (trust) failed")?;
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
                "connected": outcome.connected,
                "skipped": outcome.skipped,
            })
        );
    } else {
        println!();
        println!("{final_text}");
    }

    Ok(())
}

/// Load stored tokens for `url` and reuse them if still valid (refreshing in
/// place if needed); otherwise run the full browser sign-in flow and store
/// the result. A refresh failure that specifically means the session is
/// gone (`ClientError::SessionExpired`, e.g. a revoked refresh token) falls
/// through to a fresh sign-in rather than failing outright — any other
/// error (network, server) is propagated.
async fn ensure_signed_in(
    http: &reqwest::Client,
    url: &str,
    store: TokenStorage,
) -> Result<McpTokens> {
    if let Some(mut tokens) = McpTokenStore::load(url, store)? {
        let server = tokens.server.clone();
        match valid_access_token(http, &server, &mut tokens).await {
            Ok(_) => {
                McpTokenStore::save(&tokens, store)?;
                return Ok(tokens);
            }
            Err(ClientError::SessionExpired { .. }) => {
                // Refresh token revoked or expired — sign in again below.
            }
            Err(e) => return Err(e.into()),
        }
    }

    println!();
    println!("Signing in to the hosted pidge MCP server.");
    println!();
    println!("{}", "A browser window will open for sign-in.".dimmed());
    println!();

    let tokens = sign_in(http, url, "pidge", |authorize_url| {
        println!("{} {}", "Sign in at:".bold(), authorize_url.cyan());
        println!(
            "{}",
            "(opening your browser…  Ctrl-C here to cancel)".dimmed()
        );
        let _ = account_add::open_browser(authorize_url);
    })
    .await
    .context("MCP sign-in failed")?;

    McpTokenStore::save(&tokens, store)?;
    println!();
    println!("{} Signed in to {}", "✔".green(), tokens.server);
    Ok(tokens)
}

/// Minimal tool-calling surface [`run_migration`] needs. Implemented for the
/// real [`McpRpc`] and for a `FakeRpc` in tests, so the migration loop's
/// decision logic (who to skip, who to call `accounts_connect` for) can be
/// exercised with no network call.
pub trait McpCalls {
    async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<ToolResult>;
}

impl McpCalls for McpRpc {
    async fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> Result<ToolResult> {
        Ok(McpRpc::call_tool(self, name, arguments).await?)
    }
}

/// The outcome of one [`run_migration`] run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// Local accounts that were newly connected during this run, in the
    /// order they were processed.
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
/// `calls` is shared behind a `tokio::sync::Mutex` rather than taken as `&mut` so that
/// `on_link` can also reach it (e.g. to poll `accounts_list` while waiting)
/// without fighting the borrow checker; every lock here is released
/// before the next `.await` point that might re-enter it. A plain
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

/// Print/open the connect link for `email`, then wait for the user to
/// finish signing in: with `yes`, poll `accounts_list` every 5 seconds for
/// up to 10 minutes until `email` shows as connected; otherwise, print a
/// prompt and block for Enter.
async fn wait_for_connection<C: McpCalls>(
    calls: &Mutex<C>,
    connect_url: &str,
    email: &str,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    if !json_output {
        println!();
        println!("Connect {}: {}", email.bold(), connect_url.cyan());
        println!("{}", "(opening your browser…)".dimmed());
    }
    let _ = account_add::open_browser(connect_url);

    if yes {
        poll_until_connected(calls, email).await
    } else {
        if !json_output {
            println!(
                "{}",
                "Press Enter once you've finished signing in.".dimmed()
            );
        }
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        Ok(())
    }
}

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const MAX_POLL_WAIT: Duration = Duration::from_secs(10 * 60);

async fn poll_until_connected<C: McpCalls>(calls: &Mutex<C>, email: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + MAX_POLL_WAIT;
    let target = email.to_lowercase();
    loop {
        let text = {
            let mut c = calls.lock().await;
            c.call_tool("accounts_list", json!({})).await?.text
        };
        if parse_connected_addresses(&text).contains(&target) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow::anyhow!(
                "timed out after 10 minutes waiting for {email} to connect"
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Extract the set of e-mail addresses appearing in `accounts_list`'s
/// mailbox lines (rendered by the server, one per line, as
/// `  - <addr>[ (sign-in)]: <health>` — see `pidge-mcp`'s
/// `tools::accounts::render_accounts`). Tolerant of the exact formatting: a
/// line only needs to start with `-` once trimmed, and any whitespace-
/// separated token on it containing `@` counts, stripped of surrounding
/// punctuation and lower-cased. Lines that don't start with `-` (the
/// sign-in/default-sender/trusted-senders lines, which may also contain
/// addresses) are ignored.
pub fn parse_connected_addresses(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix('-'))
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
        next: accounts_connect to add another mailbox";

    #[test]
    fn parse_connected_addresses_extracts_mailbox_lines_only() {
        let addrs = parse_connected_addresses(ACCOUNTS_LIST_TEXT);
        assert_eq!(
            addrs,
            HashSet::from([
                "jane@example.com".to_string(),
                "work@example.com".to_string(),
            ])
        );
        // The trusted-senders line isn't a mailbox line — it must not
        // contribute an address, even though it contains an `@`.
        assert!(!addrs.contains("colleague@example.com"));
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
}
