//! `pidge-mcp` — the remote MCP server for pidge.
//!
//! One binary: an OAuth 2.1 authorization server that delegates sign-in to
//! Microsoft (`oauth`), a bearer-guarded streamable-HTTP MCP endpoint at
//! `/mcp` (`mcp`), and a secret store for the signing key and per-mailbox
//! refresh tokens (`secrets`). See `deploy/azure/` for hosting.

mod app;
mod config;
mod mailbox;
mod mcp;
mod oauth;
mod secrets;
mod state;
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
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

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

    let auth =
        AuthClient::from_env_with_backend(Arc::new(SecretTokenBackend::new(secrets.clone())))?;
    let graph = GraphClient::new(auth)?;

    let state: SharedState = Arc::new(AppState::new(config, signer, graph, secrets));

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
