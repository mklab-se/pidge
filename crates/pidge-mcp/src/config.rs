//! Process configuration, read once from the environment at startup.

use std::collections::{HashMap, HashSet};
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

/// How `main` configures the tracing subscriber. `PIDGE_MCP_LOG_FORMAT`,
/// default `json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line, for Container Apps / Log Analytics.
    Json,
    /// `tracing-subscriber`'s human-readable default, for local development.
    Text,
}

/// Shared by [`Config::from_map`] and [`log_format_from_env`]: `None` (the
/// variable unset) defaults to `Json`; any value other than `json`/`text` is
/// a startup error naming it.
fn parse_log_format(value: Option<&str>) -> Result<LogFormat> {
    match value {
        None => Ok(LogFormat::Json),
        Some("json") => Ok(LogFormat::Json),
        Some("text") => Ok(LogFormat::Text),
        Some(other) => {
            bail!("PIDGE_MCP_LOG_FORMAT must be \"json\" or \"text\", got {other:?}")
        }
    }
}

/// `PIDGE_MCP_LOG_FORMAT`, read directly from the process environment so
/// `main` can pick the tracing subscriber before [`Config`] (which may
/// itself fail to load, and needs a subscriber installed to log that
/// failure) exists.
pub fn log_format_from_env() -> Result<LogFormat> {
    parse_log_format(std::env::var("PIDGE_MCP_LOG_FORMAT").ok().as_deref())
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
    /// The markitdown executable `mail_attachment` converts documents with.
    /// `PIDGE_MCP_MARKITDOWN`, default `markitdown` (looked up on `PATH`).
    pub markitdown: PathBuf,
    /// Additional `Host` values rmcp accepts, alongside `public_url`'s host,
    /// for a domain cutover where both the old and new hostnames must keep
    /// working. `PIDGE_MCP_ALT_HOSTS`, comma-separated `host` or `host:port`,
    /// default empty.
    pub alt_hosts: Vec<String>,
    /// Origins whose previously issued access tokens must keep verifying
    /// after `public_url` (the issuer) changes. `PIDGE_MCP_LEGACY_ISSUERS`,
    /// comma-separated absolute `http`/`https` origins with no trailing
    /// slash, default empty. New access tokens are always issued under the
    /// current issuer only.
    pub legacy_issuers: Vec<String>,
    /// The tracing subscriber's output format. `PIDGE_MCP_LOG_FORMAT`,
    /// default `Json`.
    pub log_format: LogFormat,
}

/// `PIDGE_MCP_MARKITDOWN`, default `markitdown` (looked up on `PATH`).
/// Separate from [`Config::from_env`] so `--convert-check` needs no other
/// configuration.
pub fn markitdown_from_env() -> PathBuf {
    std::env::var_os("PIDGE_MCP_MARKITDOWN")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("markitdown"), PathBuf::from)
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let vars: HashMap<String, String> = std::env::vars().collect();
        Self::from_map(&vars)
    }

    /// All the parsing `from_env` does, over an explicit map instead of the
    /// process environment, so tests can exercise it without `set_var`.
    pub fn from_map(vars: &HashMap<String, String>) -> Result<Self> {
        let port = match vars.get("PORT") {
            Some(v) => v.parse().context("PORT must be a number")?,
            None => 8080,
        };

        let public_url = vars.get("PIDGE_MCP_PUBLIC_URL").context(
            "PIDGE_MCP_PUBLIC_URL is required (e.g. https://host or http://localhost:8080)",
        )?;
        let public_url: Url = public_url
            .trim_end_matches('/')
            .parse()
            .context("PIDGE_MCP_PUBLIC_URL must be an absolute URL")?;
        if public_url.scheme() != "https" && public_url.host_str() != Some("localhost") {
            bail!("PIDGE_MCP_PUBLIC_URL must use https (http is only allowed for localhost)");
        }

        let allowed_emails: HashSet<String> = vars
            .get("PIDGE_MCP_ALLOWED_EMAILS")
            .context("PIDGE_MCP_ALLOWED_EMAILS is required (comma-separated)")?
            .split(',')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if allowed_emails.is_empty() {
            bail!("PIDGE_MCP_ALLOWED_EMAILS must list at least one address");
        }

        let secrets = match (
            vars.get("PIDGE_MCP_KEYVAULT_URL"),
            vars.get("PIDGE_MCP_SECRETS_DIR"),
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

        let markitdown = vars
            .get("PIDGE_MCP_MARKITDOWN")
            .filter(|v| !v.is_empty())
            .map_or_else(|| PathBuf::from("markitdown"), PathBuf::from);

        let alt_hosts: Vec<String> = vars
            .get("PIDGE_MCP_ALT_HOSTS")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let legacy_issuers: Vec<String> = vars
            .get("PIDGE_MCP_LEGACY_ISSUERS")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.trim_end_matches('/').to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for issuer in &legacy_issuers {
            let parsed: Url = issuer.parse().with_context(|| {
                format!("PIDGE_MCP_LEGACY_ISSUERS: {issuer:?} is not an absolute URL")
            })?;
            if parsed.scheme() != "http" && parsed.scheme() != "https" {
                bail!("PIDGE_MCP_LEGACY_ISSUERS: {issuer:?} must be an http or https URL");
            }
        }

        let log_format = parse_log_format(vars.get("PIDGE_MCP_LOG_FORMAT").map(String::as_str))?;

        Ok(Self {
            markitdown,
            port,
            public_url,
            allowed_emails,
            secrets,
            alt_hosts,
            legacy_issuers,
            log_format,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn base_map() -> HashMap<String, String> {
        HashMap::from([
            (
                "PIDGE_MCP_PUBLIC_URL".to_string(),
                "http://localhost:8080".to_string(),
            ),
            (
                "PIDGE_MCP_ALLOWED_EMAILS".to_string(),
                "jane@example.com".to_string(),
            ),
            (
                "PIDGE_MCP_SECRETS_DIR".to_string(),
                "/tmp/pidge-mcp-secrets".to_string(),
            ),
        ])
    }

    #[test]
    fn from_map_parses_alt_hosts_and_legacy_issuers() {
        let mut vars = base_map();
        vars.insert(
            "PIDGE_MCP_ALT_HOSTS".to_string(),
            "ca-x.example.io, pidge.mklab.se:443".to_string(),
        );
        vars.insert(
            "PIDGE_MCP_LEGACY_ISSUERS".to_string(),
            "https://old.test/".to_string(),
        );

        let config = Config::from_map(&vars).unwrap();
        assert_eq!(
            config.alt_hosts,
            vec![
                "ca-x.example.io".to_string(),
                "pidge.mklab.se:443".to_string()
            ]
        );
        assert_eq!(config.legacy_issuers, vec!["https://old.test".to_string()]);
    }

    #[test]
    fn from_map_defaults_alt_hosts_and_legacy_issuers_to_empty() {
        let config = Config::from_map(&base_map()).unwrap();
        assert!(config.alt_hosts.is_empty());
        assert!(config.legacy_issuers.is_empty());
    }

    #[test]
    fn from_map_rejects_a_legacy_issuer_that_is_not_an_absolute_url() {
        let mut vars = base_map();
        vars.insert(
            "PIDGE_MCP_LEGACY_ISSUERS".to_string(),
            "not-a-url".to_string(),
        );
        let err = Config::from_map(&vars).unwrap_err();
        assert!(
            err.to_string().contains("not-a-url"),
            "error should name the bad value: {err}"
        );
    }

    #[test]
    fn from_map_defaults_log_format_to_json_and_accepts_text() {
        let config = Config::from_map(&base_map()).unwrap();
        assert_eq!(config.log_format, LogFormat::Json);

        let mut vars = base_map();
        vars.insert("PIDGE_MCP_LOG_FORMAT".to_string(), "text".to_string());
        let config = Config::from_map(&vars).unwrap();
        assert_eq!(config.log_format, LogFormat::Text);

        let mut vars = base_map();
        vars.insert("PIDGE_MCP_LOG_FORMAT".to_string(), "json".to_string());
        let config = Config::from_map(&vars).unwrap();
        assert_eq!(config.log_format, LogFormat::Json);
    }

    #[test]
    fn from_map_rejects_an_unknown_log_format_and_names_it() {
        let mut vars = base_map();
        vars.insert("PIDGE_MCP_LOG_FORMAT".to_string(), "yaml".to_string());
        let err = Config::from_map(&vars).unwrap_err();
        assert!(
            err.to_string().contains("yaml"),
            "error should name the bad value: {err}"
        );
    }
}
