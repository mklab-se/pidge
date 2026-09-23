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
use pidge_client::mcp::{McpRpc, McpTokenStore, McpTokens, StoredServer, valid_access_token};
use pidge_core::TokenStorage;

use crate::commands::mcp::McpUsageError;
use crate::commands::mcp_connect::{McpCalls, RefreshingRpc, backend_name, candidate_backends};

pub async fn run(
    url: Option<String>,
    store: Option<TokenStorage>,
    json_output: bool,
) -> Result<()> {
    let servers = McpTokenStore::list()?;
    let (server_url, preferred) = match url {
        Some(u) => (u, TokenStorage::Keychain),
        None => match resolve_default_server(&servers) {
            ServerResolution::Only(s) => (s.server.clone(), s.storage),
            ServerResolution::None => return Err(McpUsageError::NoServerConnected.into()),
            ServerResolution::Many(list) => {
                let servers = list
                    .iter()
                    .map(|s| s.server.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let example = list[0].server.clone();
                return Err(McpUsageError::AmbiguousServer { servers, example }.into());
            }
        },
    };

    let http = reqwest::Client::new();
    let (tokens, backend) = load_session(&http, &server_url, store, preferred).await?;

    let mut raw = McpRpc::new(
        http.clone(),
        tokens.server.clone(),
        tokens.access_token.clone(),
    );
    raw.initialize().await?;
    let mut rpc = RefreshingRpc::new(http, raw, tokens.clone(), backend);
    let list_text = rpc
        .call_tool("accounts_list", json!({}))
        .await
        .map_err(|e| crate::commands::mcp::remap_session_expired(&tokens.server, e))?
        .text;

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

/// Load the stored session for `server_url`. If `override_store` is set,
/// only that backend is consulted (per its `--store` flag doc: it skips the
/// index entirely); otherwise `preferred` is tried first, falling back to
/// the other backend, mirroring `mcp_connect`'s own session lookup. A
/// session that exists but can't be refreshed comes back as
/// [`ClientError::McpSessionExpired`]; no session in any consulted backend
/// comes back as [`McpUsageError::NotConnected`].
async fn load_session(
    http: &reqwest::Client,
    server_url: &str,
    override_store: Option<TokenStorage>,
    preferred: TokenStorage,
) -> Result<(McpTokens, TokenStorage)> {
    let to_try: Vec<TokenStorage> = match override_store {
        Some(s) => vec![s],
        None => candidate_backends(preferred).to_vec(),
    };

    for backend in to_try {
        let Some(mut tokens) = McpTokenStore::load(server_url, backend)? else {
            continue;
        };
        let server = tokens.server.clone();
        return match valid_access_token(http, &server, &mut tokens).await {
            Ok(_) => {
                McpTokenStore::save(&tokens, backend)?;
                Ok((tokens, backend))
            }
            Err(ClientError::SessionExpired { .. }) => Err(ClientError::McpSessionExpired {
                server: server_url.to_string(),
            }
            .into()),
            Err(e) => Err(e.into()),
        };
    }

    Err(McpUsageError::NotConnected {
        server: server_url.to_string(),
    }
    .into())
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
