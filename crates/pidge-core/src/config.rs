//! Persistent configuration file for pidge.
//!
//! Path: `${XDG_CONFIG_HOME:-~/.config}/pidge/config.yaml`.
//! Contains only non-sensitive metadata — tokens live in the OS keychain.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::error::CoreError;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    pub defaults: Defaults,
    pub trusted_senders: Vec<String>,
    pub classify: ClassifyConfig,
    /// Guardrails: action class -> "allow" | "confirm" | "deny".
    /// Classes: send, delete, cancel, rsvp, bulk, unsubscribe.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub guardrails: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    pub send: Option<String>,
    pub calendar: Option<String>,
}

/// User-configurable defaults for `pidge ai classify`. Every field is
/// optional; an unset field falls back to a built-in default at call time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClassifyConfig {
    /// Default classification prompt (instructions + valid outputs).
    pub prompt: Option<String>,
    /// Default batch concurrency.
    pub parallel: Option<usize>,
    /// Whether to cache classifications by message-id + prompt hash.
    pub cache: Option<bool>,
    /// Optional allowed-label set for validation.
    pub labels: Vec<String>,
}

impl Config {
    /// Default path: `${XDG_CONFIG_HOME:-~/.config}/pidge/config.yaml`.
    pub fn default_path() -> Result<PathBuf, CoreError> {
        let dir = dirs::config_dir()
            .ok_or(CoreError::NoConfigDir)?
            .join("pidge");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join("config.yaml"))
    }

    /// Load the config from the default path. If the file doesn't exist, returns `Config::default()`.
    pub fn load() -> Result<Self, CoreError> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    /// Load from a specific path. Useful for tests.
    pub fn load_from(path: &Path) -> Result<Self, CoreError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }

    /// Save the config to the default path.
    pub fn save(&self) -> Result<(), CoreError> {
        let path = Self::default_path()?;
        self.save_to(&path)
    }

    /// Save to a specific path. Useful for tests.
    pub fn save_to(&self, path: &Path) -> Result<(), CoreError> {
        let text = serde_yaml::to_string(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Add or replace an account by email. If this is the first account,
    /// also sets it as the default send AND default calendar account.
    pub fn add_account(&mut self, account: Account) {
        if let Some(existing) = self.accounts.iter_mut().find(|a| a.email == account.email) {
            *existing = account;
            return;
        }
        if self.accounts.is_empty() {
            self.defaults.send = Some(account.email.clone());
            self.defaults.calendar = Some(account.email.clone());
        }
        self.accounts.push(account);
    }

    /// Remove an account by email. Returns the removed account.
    /// If the removed account was a default, that default is cleared.
    pub fn remove_account(&mut self, email: &str) -> Option<Account> {
        let idx = self.accounts.iter().position(|a| a.email == email)?;
        let removed = self.accounts.remove(idx);
        if self.defaults.send.as_deref() == Some(email) {
            self.defaults.send = None;
        }
        if self.defaults.calendar.as_deref() == Some(email) {
            self.defaults.calendar = None;
        }
        Some(removed)
    }

    /// Set the default send account. Errors if the email isn't a known signed-in account.
    pub fn set_default_send(&mut self, email: &str) -> Result<(), CoreError> {
        if !self.accounts.iter().any(|a| a.email == email) {
            return Err(CoreError::UnknownAccount {
                email: email.to_string(),
            });
        }
        self.defaults.send = Some(email.to_string());
        Ok(())
    }

    /// Set the default calendar account. Errors if the email isn't a known signed-in account.
    pub fn set_default_calendar(&mut self, email: &str) -> Result<(), CoreError> {
        if !self.accounts.iter().any(|a| a.email == email) {
            return Err(CoreError::UnknownAccount {
                email: email.to_string(),
            });
        }
        self.defaults.calendar = Some(email.to_string());
        Ok(())
    }

    /// Find an account by email.
    pub fn find(&self, email: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.email == email)
    }

    /// Add an email to the trusted-senders list (case-insensitive). Idempotent.
    pub fn add_trusted_sender(&mut self, email: &str) {
        let lower = email.to_lowercase();
        if !self
            .trusted_senders
            .iter()
            .any(|s| s.to_lowercase() == lower)
        {
            self.trusted_senders.push(email.to_string());
        }
    }

    /// Remove an email from the trusted-senders list (case-insensitive).
    /// Returns true if it was present, false if it wasn't (idempotent either way).
    pub fn remove_trusted_sender(&mut self, email: &str) -> bool {
        let lower = email.to_lowercase();
        let before = self.trusted_senders.len();
        self.trusted_senders.retain(|s| s.to_lowercase() != lower);
        before != self.trusted_senders.len()
    }

    /// Case-insensitive check for whether an email is in the trusted-senders list.
    pub fn is_sender_trusted(&self, email: &str) -> bool {
        let lower = email.to_lowercase();
        self.trusted_senders
            .iter()
            .any(|s| s.to_lowercase() == lower)
    }

    /// Read a dotted config key as a display string, or `None` if unset.
    pub fn get_key(&self, key: &str) -> Option<String> {
        match key {
            "classify.prompt" => self.classify.prompt.clone(),
            "classify.parallel" => self.classify.parallel.map(|n| n.to_string()),
            "classify.cache" => self.classify.cache.map(|b| b.to_string()),
            "classify.labels" => {
                if self.classify.labels.is_empty() {
                    None
                } else {
                    Some(self.classify.labels.join(","))
                }
            }
            _ => {
                if let Some(class) = key.strip_prefix("guardrails.") {
                    return self.guardrails.get(class).cloned();
                }
                None
            }
        }
    }

    /// Valid guardrail action classes.
    pub const GUARDRAIL_CLASSES: [&'static str; 6] =
        ["send", "delete", "cancel", "rsvp", "bulk", "unsubscribe"];

    /// Set a dotted config key from a string value. Errors on unknown key or
    /// unparseable value.
    pub fn set_key(&mut self, key: &str, value: &str) -> Result<(), CoreError> {
        match key {
            "classify.prompt" => self.classify.prompt = Some(value.to_string()),
            "classify.parallel" => {
                let n: usize = value
                    .trim()
                    .parse()
                    .map_err(|_| CoreError::InvalidConfigValue {
                        key: key.to_string(),
                        value: value.to_string(),
                    })?;
                self.classify.parallel = Some(n);
            }
            "classify.cache" => {
                let b = match value.trim() {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(CoreError::InvalidConfigValue {
                            key: key.to_string(),
                            value: value.to_string(),
                        });
                    }
                };
                self.classify.cache = Some(b);
            }
            "classify.labels" => {
                self.classify.labels = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => {
                if let Some(class) = key.strip_prefix("guardrails.") {
                    if !Self::GUARDRAIL_CLASSES.contains(&class) {
                        return Err(CoreError::UnknownConfigKey {
                            key: key.to_string(),
                        });
                    }
                    if !["allow", "confirm", "deny"].contains(&value.trim()) {
                        return Err(CoreError::InvalidConfigValue {
                            key: key.to_string(),
                            value: value.to_string(),
                        });
                    }
                    self.guardrails
                        .insert(class.to_string(), value.trim().to_string());
                    return Ok(());
                }
                return Err(CoreError::UnknownConfigKey {
                    key: key.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Revert a dotted config key to its unset/default state.
    pub fn unset_key(&mut self, key: &str) -> Result<(), CoreError> {
        match key {
            "classify.prompt" => self.classify.prompt = None,
            "classify.parallel" => self.classify.parallel = None,
            "classify.cache" => self.classify.cache = None,
            "classify.labels" => self.classify.labels.clear(),
            _ => {
                if let Some(class) = key.strip_prefix("guardrails.") {
                    if !Self::GUARDRAIL_CLASSES.contains(&class) {
                        return Err(CoreError::UnknownConfigKey {
                            key: key.to_string(),
                        });
                    }
                    self.guardrails.remove(class);
                    return Ok(());
                }
                return Err(CoreError::UnknownConfigKey {
                    key: key.to_string(),
                });
            }
        }
        Ok(())
    }

    /// Every settable config key, for `pidge config show`/help.
    pub const KNOWN_KEYS: &'static [&'static str] = &[
        "classify.prompt",
        "classify.parallel",
        "classify.cache",
        "classify.labels",
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_account(email: &str) -> Account {
        Account {
            email: email.into(),
            tenant_id: "tid".into(),
            home_account_id: "home".into(),
            added_at: chrono::Utc.with_ymd_and_hms(2026, 5, 13, 22, 0, 0).unwrap(),
            storage: crate::TokenStorage::default(),
        }
    }

    #[test]
    fn empty_config_serializes_and_deserializes() {
        let c = Config::default();
        let yaml = serde_yaml::to_string(&c).unwrap();
        let c2: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(c, c2);
    }

    #[test]
    fn first_added_account_becomes_both_defaults() {
        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        assert_eq!(c.defaults.send.as_deref(), Some("a@b.com"));
        assert_eq!(c.defaults.calendar.as_deref(), Some("a@b.com"));
    }

    #[test]
    fn second_added_account_does_not_change_defaults() {
        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        c.add_account(make_account("c@d.com"));
        assert_eq!(c.defaults.send.as_deref(), Some("a@b.com"));
        assert_eq!(c.defaults.calendar.as_deref(), Some("a@b.com"));
        assert_eq!(c.accounts.len(), 2);
    }

    #[test]
    fn removing_default_account_clears_default() {
        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        c.add_account(make_account("c@d.com"));
        c.remove_account("a@b.com");
        assert_eq!(c.defaults.send, None);
        assert_eq!(c.defaults.calendar, None);
    }

    #[test]
    fn set_default_send_for_unknown_account_errors() {
        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        assert!(matches!(
            c.set_default_send("ghost@nowhere.com"),
            Err(CoreError::UnknownAccount { .. })
        ));
    }

    #[test]
    fn config_roundtrips_through_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");

        let mut c = Config::default();
        c.add_account(make_account("a@b.com"));
        c.add_account(make_account("c@d.com"));
        c.set_default_calendar("c@d.com").unwrap();
        c.save_to(&path).unwrap();

        let c2 = Config::load_from(&path).unwrap();
        assert_eq!(c, c2);
    }

    #[test]
    fn add_trusted_sender_is_idempotent() {
        let mut c = Config::default();
        c.add_trusted_sender("a@b.com");
        c.add_trusted_sender("a@b.com");
        assert_eq!(c.trusted_senders.len(), 1);
    }

    #[test]
    fn add_trusted_sender_is_case_insensitive() {
        let mut c = Config::default();
        c.add_trusted_sender("Maria@MKLab.se");
        c.add_trusted_sender("maria@mklab.se");
        assert_eq!(c.trusted_senders.len(), 1);
    }

    #[test]
    fn remove_trusted_sender_returns_true_when_present() {
        let mut c = Config::default();
        c.add_trusted_sender("a@b.com");
        assert!(c.remove_trusted_sender("a@b.com"));
        assert!(c.trusted_senders.is_empty());
    }

    #[test]
    fn remove_trusted_sender_returns_false_when_absent() {
        let mut c = Config::default();
        assert!(!c.remove_trusted_sender("ghost@nowhere.com"));
    }

    #[test]
    fn remove_trusted_sender_is_case_insensitive() {
        let mut c = Config::default();
        c.add_trusted_sender("Maria@MKLab.se");
        assert!(c.remove_trusted_sender("MARIA@mklab.SE"));
        assert!(c.trusted_senders.is_empty());
    }

    #[test]
    fn is_sender_trusted_case_insensitive() {
        let mut c = Config::default();
        c.add_trusted_sender("Maria@MKLab.se");
        assert!(c.is_sender_trusted("maria@mklab.se"));
        assert!(c.is_sender_trusted("MARIA@MKLAB.SE"));
        assert!(!c.is_sender_trusted("anna@mklab.se"));
    }

    #[test]
    fn config_with_missing_trusted_senders_loads_as_empty() {
        let yaml = "accounts: []\ndefaults: {}\n";
        let c: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(c.trusted_senders.is_empty());
    }

    #[test]
    fn classify_config_defaults_are_empty() {
        let c = Config::default();
        assert!(c.classify.prompt.is_none());
        assert!(c.classify.parallel.is_none());
        assert!(c.classify.cache.is_none());
        assert!(c.classify.labels.is_empty());
    }

    #[test]
    fn classify_config_roundtrips_through_yaml() {
        let mut c = Config::default();
        c.classify.prompt = Some("Classify it".into());
        c.classify.parallel = Some(8);
        c.classify.cache = Some(true);
        c.classify.labels = vec!["invoice".into(), "receipt".into()];
        let yaml = serde_yaml::to_string(&c).unwrap();
        let back: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.classify.prompt.as_deref(), Some("Classify it"));
        assert_eq!(back.classify.parallel, Some(8));
        assert_eq!(back.classify.labels, vec!["invoice", "receipt"]);
    }

    #[test]
    fn config_set_get_unset_roundtrip() {
        let mut c = Config::default();
        c.set_key("classify.parallel", "8").unwrap();
        assert_eq!(c.get_key("classify.parallel"), Some("8".to_string()));
        c.set_key("classify.labels", "invoice,receipt,ticket")
            .unwrap();
        assert_eq!(
            c.get_key("classify.labels"),
            Some("invoice,receipt,ticket".to_string())
        );
        c.set_key("classify.cache", "true").unwrap();
        assert_eq!(c.get_key("classify.cache"), Some("true".to_string()));
        c.unset_key("classify.parallel").unwrap();
        assert_eq!(c.get_key("classify.parallel"), None);
    }

    #[test]
    fn config_set_rejects_unknown_key_and_bad_value() {
        let mut c = Config::default();
        assert!(c.set_key("classify.nope", "x").is_err());
        assert!(c.set_key("classify.parallel", "notanumber").is_err());
        assert!(c.set_key("classify.cache", "maybe").is_err());
    }
}
