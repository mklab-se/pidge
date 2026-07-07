//! Guardrails: user-enforced policy over agent-initiated actions.
//!
//! The user sets, per action class, whether pidge should `allow` it (default),
//! require interactive human confirmation (`confirm` — overrides `-y`), or
//! refuse outright (`deny`). Configured via
//! `pidge config set guardrails.<class> <mode>`.

// Wired into the mutating command handlers in the guardrails task; the
// enforcement API lands first so exit-code classification can reference it.
#![allow(dead_code)]

use pidge_core::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardrailAction {
    Send,
    Delete,
    Cancel,
    Rsvp,
    Bulk,
    Unsubscribe,
}

impl GuardrailAction {
    pub fn key(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Delete => "delete",
            Self::Cancel => "cancel",
            Self::Rsvp => "rsvp",
            Self::Bulk => "bulk",
            Self::Unsubscribe => "unsubscribe",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GuardrailMode {
    #[default]
    Allow,
    Confirm,
    Deny,
}

#[derive(Debug, thiserror::Error)]
pub enum GuardrailError {
    #[error("action '{action}' is denied by guardrails (guardrails.{action}: deny)")]
    Denied { action: &'static str },

    #[error(
        "action '{action}' requires interactive confirmation (guardrails.{action}: confirm) and no terminal is attached"
    )]
    ConfirmRequired { action: &'static str },
}

/// Read the configured mode for an action class (absent → Allow).
pub fn mode_for(config: &Config, action: GuardrailAction) -> GuardrailMode {
    match config
        .get_key(&format!("guardrails.{}", action.key()))
        .as_deref()
    {
        Some("confirm") => GuardrailMode::Confirm,
        Some("deny") => GuardrailMode::Deny,
        _ => GuardrailMode::Allow,
    }
}

static DRY_RUN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called once from main when --dry-run is passed.
pub fn set_dry_run() {
    DRY_RUN.store(true, std::sync::atomic::Ordering::SeqCst);
}

pub fn dry_run_active() -> bool {
    DRY_RUN.load(std::sync::atomic::Ordering::SeqCst)
}

/// Outcome of gating an action: proceed, or stop because this is a dry run.
#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub enum Gate {
    Proceed,
    DryRun,
}

/// The one call every mutating command makes before acting:
/// evaluates guardrails (unless this is a dry run — dry-run reveals policy
/// instead of failing on it) and reports the dry-run decision.
///
/// On `Gate::DryRun` the caller prints nothing else and returns — this
/// helper already printed the "would ..." report (human or JSON).
pub fn gate(action: GuardrailAction, description: &str) -> anyhow::Result<Gate> {
    let config = Config::load().unwrap_or_default();
    if dry_run_active() {
        let mode = mode_for(&config, action);
        let policy = match mode {
            GuardrailMode::Allow => "allow",
            GuardrailMode::Confirm => "confirm",
            GuardrailMode::Deny => "deny (would be blocked)",
        };
        println!(
            "{}",
            serde_json::json!({
                "dry_run": true,
                "action": action.key(),
                "detail": description,
                "guardrail": policy,
            })
        );
        return Ok(Gate::DryRun);
    }
    enforce(&config, action, description)?;
    Ok(Gate::Proceed)
}

/// Enforce guardrails for an action about to run. `description` is shown in
/// the confirmation prompt (e.g. "send e-mail to 2 recipients").
///
/// - Allow → Ok.
/// - Confirm + TTY → interactive [y/N] prompt regardless of any `-y` flag.
/// - Confirm + no TTY → `ConfirmRequired` (exit 6).
/// - Deny → `Denied` (exit 6).
pub fn enforce(
    config: &Config,
    action: GuardrailAction,
    description: &str,
) -> Result<(), GuardrailError> {
    match mode_for(config, action) {
        GuardrailMode::Allow => Ok(()),
        GuardrailMode::Deny => Err(GuardrailError::Denied {
            action: action.key(),
        }),
        GuardrailMode::Confirm => {
            if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                return Err(GuardrailError::ConfirmRequired {
                    action: action.key(),
                });
            }
            let approved = inquire::Confirm::new(&format!(
                "guardrail [{}]: {} — proceed?",
                action.key(),
                description
            ))
            .with_default(false)
            .prompt()
            .unwrap_or(false);
            if approved {
                Ok(())
            } else {
                Err(GuardrailError::Denied {
                    action: action.key(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(key: &str, value: &str) -> Config {
        let mut config = Config::default();
        config.set_key(key, value).unwrap();
        config
    }

    #[test]
    fn absent_guardrails_default_to_allow() {
        let config = Config::default();
        for action in [
            GuardrailAction::Send,
            GuardrailAction::Delete,
            GuardrailAction::Bulk,
        ] {
            assert_eq!(mode_for(&config, action), GuardrailMode::Allow);
        }
    }

    #[test]
    fn configured_modes_are_read() {
        let config = config_with("guardrails.send", "confirm");
        assert_eq!(
            mode_for(&config, GuardrailAction::Send),
            GuardrailMode::Confirm
        );
        let config = config_with("guardrails.delete", "deny");
        assert_eq!(
            mode_for(&config, GuardrailAction::Delete),
            GuardrailMode::Deny
        );
        // invalid values are rejected at set time
        let mut config = Config::default();
        assert!(config.set_key("guardrails.rsvp", "sometimes").is_err());
        assert!(config.set_key("guardrails.nonsense", "deny").is_err());
        // and a hand-edited invalid value in the file falls back to allow
        config.guardrails.insert("rsvp".into(), "sometimes".into());
        assert_eq!(
            mode_for(&config, GuardrailAction::Rsvp),
            GuardrailMode::Allow
        );
    }

    #[test]
    fn deny_errors_and_allow_passes() {
        let config = config_with("guardrails.cancel", "deny");
        let err = enforce(&config, GuardrailAction::Cancel, "cancel meeting").unwrap_err();
        assert!(matches!(err, GuardrailError::Denied { action: "cancel" }));

        let config = Config::default();
        assert!(enforce(&config, GuardrailAction::Send, "send").is_ok());
    }

    #[test]
    fn confirm_without_tty_requires_human() {
        // Tests never have a TTY on stdin under cargo test.
        let config = config_with("guardrails.send", "confirm");
        let err = enforce(&config, GuardrailAction::Send, "send e-mail").unwrap_err();
        assert!(matches!(
            err,
            GuardrailError::ConfirmRequired { action: "send" }
        ));
    }
}
