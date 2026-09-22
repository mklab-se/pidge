//! Process configuration, read once from the environment at startup.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use url::Url;

/// Where the server keeps its secrets (signing key, per-mailbox tokens).
#[derive(Debug, Clone)]
pub enum SecretsBackend {
    /// Azure Key Vault, authenticated with the ambient Azure identity
    /// (managed identity on Azure, `az login` on a workstation).
    KeyVault { vault_url: Url },
    /// One file per secret under a directory. Development only.
    File { dir: PathBuf },
}

#[derive(Debug, Clone)]
pub struct Config {
    /// TCP port to listen on. `PORT`, default 8080.
    pub port: u16,
    /// The externally visible origin, e.g. `https://ca-pidge-mcp.example.azurecontainerapps.io`.
    /// Used as the OAuth issuer, the protected-resource identifier and the
    /// Microsoft redirect URI. `PIDGE_MCP_PUBLIC_URL`.
    pub public_url: Url,
    /// Lower-cased e-mail addresses allowed to sign in. `PIDGE_MCP_ALLOWED_EMAILS`, comma-separated.
    pub allowed_emails: HashSet<String>,
    pub secrets: SecretsBackend,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let port = match std::env::var("PORT") {
            Ok(v) => v.parse().context("PORT must be a number")?,
            Err(_) => 8080,
        };

        let public_url = std::env::var("PIDGE_MCP_PUBLIC_URL").context(
            "PIDGE_MCP_PUBLIC_URL is required (e.g. https://host or http://localhost:8080)",
        )?;
        let public_url: Url = public_url
            .trim_end_matches('/')
            .parse()
            .context("PIDGE_MCP_PUBLIC_URL must be an absolute URL")?;
        if public_url.scheme() != "https" && public_url.host_str() != Some("localhost") {
            bail!("PIDGE_MCP_PUBLIC_URL must use https (http is only allowed for localhost)");
        }

        let allowed_emails: HashSet<String> = std::env::var("PIDGE_MCP_ALLOWED_EMAILS")
            .context("PIDGE_MCP_ALLOWED_EMAILS is required (comma-separated)")?
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if allowed_emails.is_empty() {
            bail!("PIDGE_MCP_ALLOWED_EMAILS must list at least one address");
        }

        let secrets = match (
            std::env::var("PIDGE_MCP_KEYVAULT_URL").ok(),
            std::env::var("PIDGE_MCP_SECRETS_DIR").ok(),
        ) {
            (Some(vault), None) => SecretsBackend::KeyVault {
                vault_url: vault
                    .parse()
                    .context("PIDGE_MCP_KEYVAULT_URL must be a URL")?,
            },
            (None, Some(dir)) => SecretsBackend::File { dir: dir.into() },
            (Some(_), Some(_)) => {
                bail!("set only one of PIDGE_MCP_KEYVAULT_URL and PIDGE_MCP_SECRETS_DIR")
            }
            (None, None) => {
                bail!("set PIDGE_MCP_KEYVAULT_URL (Azure) or PIDGE_MCP_SECRETS_DIR (dev)")
            }
        };

        Ok(Self {
            port,
            public_url,
            allowed_emails,
            secrets,
        })
    }

    /// The public origin without a trailing slash, for building URLs.
    pub fn base_url(&self) -> &str {
        self.public_url.as_str().trim_end_matches('/')
    }

    /// The OAuth protected resource identifier: the MCP endpoint URL.
    pub fn resource_url(&self) -> String {
        format!("{}/mcp", self.base_url())
    }

    /// Where Microsoft sends the user back after sign-in.
    pub fn microsoft_callback_url(&self) -> String {
        format!("{}/callback", self.base_url())
    }

    pub fn is_allowed(&self, email: &str) -> bool {
        self.allowed_emails.contains(&email.to_ascii_lowercase())
    }
}
