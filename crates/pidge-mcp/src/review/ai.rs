//! Per-user AI configuration for sorting mail: the user's own provider key
//! in its own secret, the status pidge reports when it stops working, and
//! the Ailloy client built from it.
//!
//! Only providers with fixed public endpoints are offered, and the client
//! is built here from provider + model + key: no URL ever comes from
//! input, so a user can't point the server at an internal address.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

use ailloy::ErrorKind;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::secrets::SharedSecrets;

/// How long a built client is reused before the secret is read again.
const CLIENT_TTL: StdDuration = StdDuration::from_secs(10 * 60);
/// Longest key accepted from the form.
pub const MAX_KEY_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
    Typesafe,
    Openai,
    Anthropic,
}

impl AiProvider {
    pub const ALL: [AiProvider; 3] = [
        AiProvider::Typesafe,
        AiProvider::Openai,
        AiProvider::Anthropic,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AiProvider::Typesafe => "TypeSafe",
            AiProvider::Openai => "OpenAI",
            AiProvider::Anthropic => "Anthropic",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            AiProvider::Typesafe => "jev-latest",
            AiProvider::Openai => "gpt-5.4-mini",
            AiProvider::Anthropic => "claude-haiku-5-5",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "typesafe" | "jev" => Some(AiProvider::Typesafe),
            "openai" => Some(AiProvider::Openai),
            "anthropic" => Some(AiProvider::Anthropic),
            _ => None,
        }
    }
}

/// The secret: never rendered, never logged, never sent to the harness.
#[derive(Clone, Serialize, Deserialize)]
pub struct AiSecret {
    pub provider: AiProvider,
    pub model: String,
    pub api_key: String,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for AiSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiSecret")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// What pidge last saw of the user's AI setup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiStatus {
    /// No key configured.
    #[default]
    Off,
    Ok,
    /// The account is out of credits or quota.
    NoCredits,
    /// The key was refused (invalid, revoked, no permission).
    KeyRejected,
    /// Requests are refused for another reason (unknown model, unexpected
    /// response): needs the user to look at the setup.
    Failing,
    /// Temporarily unreachable or overloaded.
    Unavailable,
}

impl AiStatus {
    pub fn from_kind(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::QuotaExhausted => AiStatus::NoCredits,
            ErrorKind::Auth => AiStatus::KeyRejected,
            ErrorKind::RateLimited | ErrorKind::Unavailable | ErrorKind::Network => {
                AiStatus::Unavailable
            }
            _ => AiStatus::Failing,
        }
    }

    /// Needs the user to act (top up, new key); the provider is not called
    /// on every review while in this state.
    pub fn needs_user(self) -> bool {
        matches!(
            self,
            AiStatus::NoCredits | AiStatus::KeyRejected | AiStatus::Failing
        )
    }

    /// For `accounts_list`.
    pub fn describe(self) -> &'static str {
        match self {
            AiStatus::Off => "off",
            AiStatus::Ok => "ok",
            AiStatus::NoCredits => "out of credits",
            AiStatus::KeyRejected => "key rejected",
            AiStatus::Failing => "failing",
            AiStatus::Unavailable => "temporarily unavailable",
        }
    }
}

/// The non-secret AI state kept on the user record.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSettings {
    #[serde(default)]
    pub status: AiStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_since: Option<DateTime<Utc>>,
    /// `TypeSafe jev-latest`: what is configured, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// When the provider was last tried while in a needs-user state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_hint_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub hints_off: bool,
}

impl AiSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Records `status`, keeping `status_since` when it is unchanged.
    pub fn set_status(&mut self, status: AiStatus, now: DateTime<Utc>) {
        if self.status != status || self.status_since.is_none() {
            self.status_since = Some(now);
        }
        self.status = status;
    }

    /// One line for `accounts_list`.
    pub fn line(&self) -> String {
        match (self.status, &self.provider) {
            (AiStatus::Off, _) => {
                "AI sorting: off (accounts_connect_ai sets up the user's own key)".to_string()
            }
            (status, Some(p)) => format!("AI sorting: {} ({p})", status.describe()),
            (status, None) => format!("AI sorting: {}", status.describe()),
        }
    }
}

/// `ai-` + 16 hex of the SHA-256 of the sign-in: like the user record's
/// name, it doesn't put the address in Key Vault's secret listing.
pub fn secret_name(signin: &str) -> String {
    let digest = Sha256::digest(signin.to_ascii_lowercase().as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("ai-{hex}")
}

pub async fn load(secrets: &SharedSecrets, signin: &str) -> Result<Option<AiSecret>> {
    match secrets.get(&secret_name(signin)).await? {
        Some(raw) if !raw.trim().is_empty() => Ok(Some(serde_json::from_str(&raw)?)),
        _ => Ok(None),
    }
}

pub async fn save(secrets: &SharedSecrets, signin: &str, secret: &AiSecret) -> Result<()> {
    secrets
        .set(&secret_name(signin), &serde_json::to_string(secret)?)
        .await
}

/// Removes the key (an empty value reads as absent; see `delete_mailbox`).
pub async fn clear(secrets: &SharedSecrets, signin: &str) -> Result<()> {
    if secrets.get(&secret_name(signin)).await?.is_some() {
        secrets.set(&secret_name(signin), "").await?;
    }
    Ok(())
}

/// A provider key looks plausible: printable ASCII, no spaces, bounded.
pub fn check_key(key: &str) -> Result<String, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("Paste the API key.".into());
    }
    if key.len() > MAX_KEY_LEN || !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err("That does not look like an API key.".into());
    }
    Ok(key.to_string())
}

/// A model name is a short token; empty means the provider's default.
pub fn check_model(provider: AiProvider, model: &str) -> Result<String, String> {
    let model = model.trim();
    if model.is_empty() {
        return Ok(provider.default_model().to_string());
    }
    if model.len() > 80
        || !model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._:".contains(c))
    {
        return Err("That does not look like a model name.".into());
    }
    Ok(model.to_string())
}

/// Builds the Ailloy client. `endpoint` is only ever set by tests (a mock
/// TypeSafe); production passes `None` and the provider's own URL is used.
pub fn build_client(secret: &AiSecret, endpoint: Option<&str>) -> Result<ailloy::Client> {
    let builder = ailloy::Client::builder()
        .api_key(secret.api_key.clone())
        .model(secret.model.clone());
    let builder = match secret.provider {
        AiProvider::Typesafe => {
            let b = builder.typesafe();
            match endpoint {
                Some(e) => b.endpoint(e),
                None => b,
            }
        }
        AiProvider::Openai => builder.openai(),
        AiProvider::Anthropic => builder.anthropic(),
    };
    builder.build()
}

/// A cached client: when it was built, the client, and its label.
type CachedClient = (Instant, Arc<ailloy::Client>, String);

/// Built clients per user, reused for [`CLIENT_TTL`] and dropped whenever
/// the user's key changes.
#[derive(Default)]
pub struct AiClients {
    clients: Mutex<HashMap<String, CachedClient>>,
}

impl AiClients {
    pub fn get(&self, user: &str) -> Option<(Arc<ailloy::Client>, String)> {
        let map = self.clients.lock().expect("ai clients lock");
        map.get(user)
            .filter(|(at, _, _)| at.elapsed() < CLIENT_TTL)
            .map(|(_, c, label)| (c.clone(), label.clone()))
    }

    pub fn put(&self, user: &str, client: Arc<ailloy::Client>, label: String) {
        let mut map = self.clients.lock().expect("ai clients lock");
        map.retain(|_, (at, _, _)| at.elapsed() < CLIENT_TTL);
        map.insert(user.to_string(), (Instant::now(), client, label));
    }

    pub fn forget(&self, user: &str) {
        self.clients.lock().expect("ai clients lock").remove(user);
    }
}

/// The label stored on the record and used in cache keys: `TypeSafe jev-latest`.
pub fn label(secret: &AiSecret) -> String {
    format!("{} {}", secret.provider.label(), secret.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_never_shows_the_key() {
        let s = AiSecret {
            provider: AiProvider::Typesafe,
            model: "jev-latest".into(),
            api_key: "sk-very-secret".into(),
            updated_at: Utc::now(),
        };
        assert!(!format!("{s:?}").contains("very-secret"));
    }

    #[test]
    fn secret_names_do_not_contain_the_address() {
        let n = secret_name("Jane@Example.com");
        assert!(n.starts_with("ai-") && n.len() == 19, "{n}");
        assert!(!n.contains("jane"));
        assert_eq!(n, secret_name("jane@example.com"));
    }

    #[test]
    fn error_kinds_map_to_statuses() {
        assert_eq!(
            AiStatus::from_kind(ErrorKind::QuotaExhausted),
            AiStatus::NoCredits
        );
        assert_eq!(AiStatus::from_kind(ErrorKind::Auth), AiStatus::KeyRejected);
        assert_eq!(
            AiStatus::from_kind(ErrorKind::RateLimited),
            AiStatus::Unavailable
        );
        assert_eq!(
            AiStatus::from_kind(ErrorKind::Network),
            AiStatus::Unavailable
        );
        assert_eq!(
            AiStatus::from_kind(ErrorKind::InvalidRequest),
            AiStatus::Failing
        );
        assert!(AiStatus::NoCredits.needs_user());
        assert!(!AiStatus::Unavailable.needs_user());
    }

    #[test]
    fn keys_and_models_are_checked() {
        assert!(check_key("  ").is_err());
        assert!(check_key("has space").is_err());
        assert_eq!(check_key(" ts-abc123 ").unwrap(), "ts-abc123");
        assert_eq!(check_model(AiProvider::Typesafe, "").unwrap(), "jev-latest");
        assert!(check_model(AiProvider::Openai, "gpt/../../x").is_err());
    }

    #[test]
    fn status_since_only_moves_on_a_change() {
        let mut s = AiSettings::default();
        let t1 = Utc::now();
        s.set_status(AiStatus::Ok, t1);
        s.set_status(AiStatus::Ok, t1 + chrono::Duration::hours(1));
        assert_eq!(s.status_since, Some(t1));
        s.set_status(AiStatus::NoCredits, t1 + chrono::Duration::hours(2));
        assert_eq!(s.status_since, Some(t1 + chrono::Duration::hours(2)));
    }
}
