//! `pidge-mcp` — the remote MCP server for pidge.
//!
//! One binary: an OAuth 2.1 authorization server that delegates sign-in to
//! Microsoft (`oauth`), a bearer-guarded streamable-HTTP MCP endpoint at
//! `/mcp` (`tools`), and a secret store for the signing key and per-mailbox
//! refresh tokens (`secrets`). See `deploy/azure/` for hosting.

mod app;
mod cache;
mod config;
mod contacts;
mod context;
mod download;
mod mailbox;
mod markitdown;
mod oauth;
mod render;
mod secrets;
mod state;
#[cfg(test)]
mod test_support;
mod tools;
mod users;

use std::sync::Arc;

use anyhow::{Context, Result};
use pidge_client::{AuthClient, GraphClient};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::config::{Config, SecretsBackend};
use crate::mailbox::SecretTokenBackend;
use crate::oauth::jwt::Signer;
use crate::secrets::{FileSecrets, KeyVaultSecrets, SIGNING_KEY_SECRET, SharedSecrets};
use crate::state::{AppState, SharedState};

#[tokio::main]
async fn main() -> Result<()> {
    // Diagnostic: `pidge-mcp --convert-check <path>` converts a local file
    // through the production markitdown path and exits; no server config.
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|a| a == "--convert-check") {
        let code = match args.next() {
            Some(path) => {
                let bin = config::markitdown_from_env();
                match markitdown::convert_check(&bin, std::path::Path::new(&path)).await {
                    Ok(chars) => {
                        println!("converted {chars} chars");
                        0
                    }
                    Err(e) => {
                        println!("conversion failed: {e}");
                        1
                    }
                }
            }
            None => {
                eprintln!("usage: pidge-mcp --convert-check <path>");
                1
            }
        };
        std::process::exit(code);
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    forbid_core_dumps_and_ptrace();

    // Conversions a killed process never cleaned up; see `markitdown`.
    let swept = markitdown::sweep_stale_temp_files(std::time::Duration::from_secs(60 * 60));
    if swept > 0 {
        tracing::info!(count = swept, "removed stale attachment temp files");
    }

    let config = Config::from_env()?;
    tracing::info!(
        public_url = %config.public_url,
        allowed = config.allowed_emails.len(),
        "starting pidge-mcp {}",
        env!("CARGO_PKG_VERSION")
    );

    let secrets: SharedSecrets = match &config.secrets {
        SecretsBackend::KeyVault { vault_url } => {
            tracing::info!(vault = %vault_url, "secrets: Azure Key Vault");
            Arc::new(KeyVaultSecrets::new(vault_url)?)
        }
        SecretsBackend::File { dir } => {
            tracing::warn!(dir = %dir.display(), "secrets: plaintext files (development only)");
            Arc::new(FileSecrets::new(dir)?)
        }
    };

    let signer = Signer::new(
        &load_or_create_signing_key(&secrets).await?,
        config.base_url(),
        config.resource_url(),
    );

    let token_backend = Arc::new(SecretTokenBackend::new(secrets.clone()));
    let auth = AuthClient::from_env_with_backend(token_backend.clone())?;
    let graph = GraphClient::new(auth)?;

    let state: SharedState = Arc::new(AppState::new(config, signer, graph, token_backend, secrets));

    let cancel = CancellationToken::new();
    let app = app::build_router(state.clone(), cancel.child_token());

    let addr = format!("0.0.0.0:{}", state.config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            cancel.cancel();
        })
        .await?;
    Ok(())
}

/// markitdown runs as our uid, so without this a child exploited by a
/// hostile document could read `/proc/<our pid>/environ` (the managed
/// identity endpoint and secret on Container Apps) or ptrace us. Marking the
/// process non-dumpable makes those files root-only. The child's own
/// environment is scrubbed separately (see `markitdown`).
#[cfg(target_os = "linux")]
fn forbid_core_dumps_and_ptrace() {
    // SAFETY: PR_SET_DUMPABLE takes plain integer arguments and touches no
    // memory of ours.
    let rc = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    if rc == 0 {
        tracing::info!("process marked non-dumpable");
    } else {
        tracing::info!("could not mark process non-dumpable");
    }
}

#[cfg(not(target_os = "linux"))]
fn forbid_core_dumps_and_ptrace() {}

async fn load_or_create_signing_key(secrets: &SharedSecrets) -> Result<Vec<u8>> {
    if let Some(stored) = secrets.get(SIGNING_KEY_SECRET).await? {
        return Signer::decode_key(&stored);
    }
    tracing::info!("no signing key found; generating one");
    let fresh = Signer::generate_key();
    secrets.set(SIGNING_KEY_SECRET, &fresh).await?;
    Signer::decode_key(&fresh)
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
    tracing::info!("shutting down");
}
