//! `pidge mcp status [url]` — show the current hosted MCP session: which
//! server, when its access token expires, which backend it's stored under,
//! then the live `accounts_list` text for that session.
//!
//! With no `url`, the server is resolved from the on-disk index the
//! keychain itself can't provide (see [`pidge_client::mcp::McpTokenStore::list`]):
//! exactly one stored server is used automatically; zero or several are
//! reported via [`McpUsageError`] rather than guessed at.

use anyhow::Result;
use chrono::{DateTime, Duration, Local, Utc};
use colored::Colorize;
use serde_json::json;

use pidge_client::ClientError;
use pidge_client::mcp::{McpRpc, McpTokenStore, McpTokens, StoredServer};
use pidge_core::TokenStorage;

use crate::commands::mcp::McpUsageError;
use crate::commands::mcp_session::{
    McpCalls, RefreshingRpc, SessionLookup, backend_name, lookup_session, preferred_backend_for,
};

pub async fn run(
    url: Option<String>,
    store: Option<TokenStorage>,
    json_output: bool,
) -> Result<()> {
    // `McpTokenStore::list` is only consulted when actually needed: to
    // resolve which server a url-less call means, or (when a url is given)
    // to seed the `--store` preference from what the index already knows —
    // never for its own sake, and never twice for the same run.
    let (server_url, preferred) = match url {
        Some(u) => {
            let preferred = match store {
                Some(s) => s,
                None => preferred_backend_for(&u, &McpTokenStore::list()?),
            };
            (u, preferred)
        }
        None => {
            let servers = McpTokenStore::list()?;
            let server_url = match resolve_default_server(&servers) {
                ServerResolution::Only(s) => s.server,
                ServerResolution::None => return Err(McpUsageError::NoServerConnected.into()),
                ServerResolution::Many(list) => {
                    return Err(ambiguous_server_error(&list, "pidge mcp status"));
                }
            };
            let preferred = store.unwrap_or_else(|| preferred_backend_for(&server_url, &servers));
            (server_url, preferred)
        }
    };

    let http = reqwest::Client::new();
    let lookup = lookup_session(&http, &server_url, preferred).await?;
    let (tokens, backend) = session_or_error(lookup, &server_url)
        .map_err(|e| crate::commands::mcp::remap_session_expired(&server_url, e))?;

    let raw = McpRpc::new(
        http.clone(),
        tokens.server.clone(),
        tokens.access_token.clone(),
    );
    let mut rpc = RefreshingRpc::new(http, raw, tokens.clone(), backend);
    let list_text = accounts_list(&mut rpc)
        .await
        .map_err(|e| crate::commands::mcp::remap_session_expired(&tokens.server, e))?;

    if json_output {
        println!(
            "{}",
            json!({
                "server": tokens.server,
                "storage": backend,
                "expires_at": tokens.expires_at,
                "accounts": list_text,
            })
        );
    } else {
        println!("Server: {}", tokens.server.cyan());
        println!("Storage: {}", backend_name(backend));
        println!(
            "Token expires: {} ({})",
            format_relative(Utc::now(), tokens.expires_at),
            tokens
                .expires_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M %Z")
        );
        println!();
        println!("{list_text}");
    }

    Ok(())
}

/// Initialize the session and fetch `accounts_list`'s text. A `401` on
/// either step (the locally-held token looked fresh, but the server had
/// already revoked it — see [`RefreshingRpc::initialize`]) surfaces as
/// [`ClientError::SessionExpired`], which the caller remaps the same way as
/// [`session_or_error`]'s `Expired` case.
async fn accounts_list(rpc: &mut RefreshingRpc) -> Result<String> {
    rpc.initialize().await?;
    Ok(rpc.call_tool("accounts_list", json!({})).await?.text)
}

/// Turn a [`SessionLookup`] into `status`'s outcome: the found session, or a
/// typed error for the other two cases — [`ClientError::SessionExpired`]
/// for `Expired` (so the shared `remap_session_expired` at the call site
/// turns it into the `pidge mcp connect <url>` hint and exit code 3) and
/// [`McpUsageError::NotConnected`] for `Absent` (exit code 4). Pure and
/// synchronous, so this mapping is directly unit-testable without touching
/// any real backend.
fn session_or_error(lookup: SessionLookup, server_url: &str) -> Result<(McpTokens, TokenStorage)> {
    match lookup {
        SessionLookup::Found(tokens, backend) => Ok((tokens, backend)),
        SessionLookup::Expired => Err(ClientError::SessionExpired {
            email: server_url.to_string(),
        }
        .into()),
        SessionLookup::Absent => Err(McpUsageError::NotConnected {
            server: server_url.to_string(),
        }
        .into()),
    }
}

/// What `pidge mcp status` (with no `url`) should do about which server to
/// act on. A pure function over the stored index so the none/one/many
/// branching is directly unit-testable with no I/O.
enum ServerResolution {
    Only(StoredServer),
    None,
    Many(Vec<StoredServer>),
}

fn resolve_default_server(servers: &[StoredServer]) -> ServerResolution {
    match servers {
        [] => ServerResolution::None,
        [only] => ServerResolution::Only(only.clone()),
        many => ServerResolution::Many(many.to_vec()),
    }
}

/// Build [`McpUsageError::AmbiguousServer`] for `list`, with `example` a
/// complete, ready-to-run command using the first listed server — `command`
/// is the caller's own name (`"pidge mcp status"`) rather than hard-coded,
/// so the message stays correct if another url-less subcommand grows this
/// same resolution later.
fn ambiguous_server_error(list: &[StoredServer], command: &str) -> anyhow::Error {
    let servers = list
        .iter()
        .map(|s| s.server.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let example = format!("{command} {}", list[0].server);
    McpUsageError::AmbiguousServer { servers, example }.into()
}

/// "in 45m" for a future `target`, "3h ago" for a past one — used for token
/// expiry, which is usually future (the session was just validated/
/// refreshed) but can be in the past for an already-expired stored token
/// before that's discovered.
fn format_relative(now: DateTime<Utc>, target: DateTime<Utc>) -> String {
    let delta = target - now;
    if delta >= Duration::zero() {
        format!("in {}", humanize(delta))
    } else {
        format!("{} ago", humanize(-delta))
    }
}

fn humanize(d: Duration) -> String {
    if d.num_days() >= 1 {
        format!("{}d", d.num_days())
    } else if d.num_hours() >= 1 {
        format!("{}h", d.num_hours())
    } else if d.num_minutes() >= 1 {
        format!("{}m", d.num_minutes())
    } else {
        format!("{}s", d.num_seconds().max(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(url: &str, storage: TokenStorage) -> StoredServer {
        StoredServer {
            server: url.to_string(),
            storage,
        }
    }

    fn fake_tokens(server: &str) -> McpTokens {
        McpTokens {
            server: server.to_string(),
            access_token: "AT".into(),
            refresh_token: "RT".into(),
            expires_at: Utc::now() + Duration::seconds(3600),
            client_id: "client-jwt".into(),
        }
    }

    // --- resolve_default_server --------------------------------------

    #[test]
    fn resolve_default_server_reports_none_when_nothing_is_stored() {
        assert!(matches!(
            resolve_default_server(&[]),
            ServerResolution::None
        ));
    }

    #[test]
    fn resolve_default_server_picks_the_only_entry() {
        let entry = server("https://mcp.example.com", TokenStorage::Keychain);
        match resolve_default_server(std::slice::from_ref(&entry)) {
            ServerResolution::Only(s) => assert_eq!(s, entry),
            _ => panic!("expected Only, got a different variant"),
        }
    }

    #[test]
    fn resolve_default_server_reports_many_without_picking_one() {
        let entries = vec![
            server("https://a.example.com", TokenStorage::Keychain),
            server("https://b.example.com", TokenStorage::File),
        ];
        match resolve_default_server(&entries) {
            ServerResolution::Many(list) => assert_eq!(list, entries),
            _ => panic!("expected Many"),
        }
    }

    // --- preferred_backend_for (explicit-url path, N1) --------------------

    #[test]
    fn explicit_url_preference_comes_from_the_index_when_store_is_not_given() {
        let servers = vec![server("https://mcp.example.com", TokenStorage::File)];
        assert_eq!(
            preferred_backend_for("https://mcp.example.com", &servers),
            TokenStorage::File
        );
    }

    #[test]
    fn explicit_url_preference_defaults_to_keychain_when_unindexed() {
        let servers = vec![server("https://other.example.com", TokenStorage::File)];
        assert_eq!(
            preferred_backend_for("https://mcp.example.com", &servers),
            TokenStorage::Keychain
        );
    }

    // --- ambiguous_server_error ------------------------------------------

    #[test]
    fn ambiguous_server_error_names_the_calling_command_not_a_hardcoded_one() {
        let list = vec![
            server("https://a.example.com", TokenStorage::Keychain),
            server("https://b.example.com", TokenStorage::File),
        ];
        let err = ambiguous_server_error(&list, "pidge mcp logout");
        let message = err.to_string();
        assert!(
            message.contains("pidge mcp logout https://a.example.com"),
            "{message}"
        );
        assert!(message.contains("https://a.example.com"), "{message}");
        assert!(message.contains("https://b.example.com"), "{message}");
    }

    // --- session_or_error (M5: expired-vs-absent mapping) -----------------

    #[test]
    fn session_or_error_passes_through_a_found_session() {
        let tokens = fake_tokens("https://mcp.example.com");
        let (out_tokens, backend) = session_or_error(
            SessionLookup::Found(tokens.clone(), TokenStorage::File),
            "https://mcp.example.com",
        )
        .unwrap();
        assert_eq!(out_tokens, tokens);
        assert_eq!(backend, TokenStorage::File);
    }

    #[test]
    fn session_or_error_maps_expired_to_session_expired_for_the_remap_hint() {
        let err = session_or_error(SessionLookup::Expired, "https://mcp.example.com").unwrap_err();
        assert!(matches!(
            err.downcast_ref::<ClientError>(),
            Some(ClientError::SessionExpired { .. })
        ));
    }

    #[test]
    fn session_or_error_maps_absent_to_not_connected() {
        let err = session_or_error(SessionLookup::Absent, "https://mcp.example.com").unwrap_err();
        assert!(matches!(
            err.downcast_ref::<McpUsageError>(),
            Some(McpUsageError::NotConnected { .. })
        ));
    }

    // --- format_relative / humanize ------------------------------------

    #[test]
    fn format_relative_reports_future_expiry_as_in_x() {
        let now = Utc::now();
        let target = now + Duration::minutes(45);
        assert_eq!(format_relative(now, target), "in 45m");
    }

    #[test]
    fn format_relative_reports_past_expiry_as_x_ago() {
        let now = Utc::now();
        let target = now - Duration::hours(3);
        assert_eq!(format_relative(now, target), "3h ago");
    }
}
