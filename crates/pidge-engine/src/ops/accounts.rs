//! `accounts`: the user's mailboxes: list, connect, disconnect, and the
//! user's own AI key.

use std::sync::Arc;

use chrono::Utc;
use pidge_core::provider::ProviderKind;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::ctx::User;
use crate::error::{OpError, OpResult};
use crate::registry::{Meta, OpDef, Output, op};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AccountsInput {
    /// `list` (default), `connect` (add a mailbox), `disconnect`, or `ai`
    /// (how to set up the user's own AI key for sorting and send checks).
    pub action: Option<String>,
    /// For `connect`: `microsoft` (default; Outlook.com and Microsoft 365)
    /// or `google` (Gmail).
    pub provider: Option<String>,
    /// For `connect`: the address to connect (a hint); for `disconnect`:
    /// the mailbox.
    pub email: Option<String>,
}

pub fn accounts() -> OpDef {
    op(
        Meta {
            name: "accounts",
            cli: &["accounts"],
            summary: "Your mailboxes: list, connect (Microsoft or Gmail), disconnect, AI key",
            description: "The user's mailboxes and their health, the index state, the default sender and \
timezone. action=connect provider=microsoft|google adds one (show the user the link it returns, \
or it signs in locally); action=disconnect email=... removes one; action=ai tells how to set up the \
user's own AI key (optional: it sharpens sorting and send checks).",
            positional: &["action"],
            read_only: false,
        },
        run_accounts,
    )
}

async fn run_accounts(user: Arc<User>, input: AccountsInput) -> OpResult<Output> {
    match input.action.as_deref().unwrap_or("list") {
        "list" => {
            let prefs = user.prefs().await;
            let tz = user.tz().await;
            let list = user.host.accounts().await;
            if list.is_empty() {
                return Ok(Output::new(
                    "No mailbox is connected yet.\nnext: accounts action=connect provider=microsoft (or google)",
                    json!({ "accounts": [] }),
                ));
            }
            let mut text = String::from("Mailboxes:\n");
            let mut data = Vec::new();
            for a in &list {
                let st = user.store.sync_state(&a.address)?;
                let synced = match (st.complete, st.synced_at) {
                    (true, Some(t)) => format!("synced {} ago", crate::render::age(t, Utc::now())),
                    (false, _) if st.cursor.is_some() => "loading".into(),
                    _ => "not synced yet".into(),
                };
                let default = prefs.default_sender.as_deref() == Some(&a.address)
                    || (prefs.default_sender.is_none()
                        && Some(&a.address) == list.first().map(|x| &x.address));
                text.push_str(&format!(
                    "- {} ({}): {}, {synced}{}\n",
                    a.address,
                    match a.provider {
                        ProviderKind::Microsoft => "Microsoft",
                        ProviderKind::Google => "Gmail",
                    },
                    a.status,
                    if default { ", default sender" } else { "" }
                ));
                data.push(json!({ "address": a.address, "provider": a.provider.as_str(), "status": a.status, "default": default }));
            }
            text.push_str(&format!("Timezone: {tz}\n"));
            let ai = if user.ai().is_some() {
                format!(
                    "Your AI: {}",
                    if prefs.ai.status.is_empty() {
                        "ok"
                    } else {
                        &prefs.ai.status
                    }
                )
            } else {
                "Your AI: off (optional; accounts action=ai)".into()
            };
            text.push_str(&ai);
            Ok(Output::new(
                text,
                json!({ "accounts": data, "timezone": tz.name() }),
            ))
        }
        "connect" => {
            let provider = match input
                .provider
                .as_deref()
                .unwrap_or("microsoft")
                .to_ascii_lowercase()
                .as_str()
            {
                "microsoft" | "outlook" | "m365" | "office365" | "hotmail" => {
                    ProviderKind::Microsoft
                }
                "google" | "gmail" => ProviderKind::Google,
                other => {
                    return Err(OpError::usage(format!(
                        "provider is microsoft or google, not {other:?}"
                    )));
                }
            };
            let text = user.host.connect(provider, input.email.as_deref()).await?;
            Ok(Output::text(text))
        }
        "disconnect" => {
            let email = input
                .email
                .clone()
                .ok_or_else(|| OpError::usage("disconnect needs email"))?;
            let p = user.provider(&email)?;
            let address = p.address().to_string();
            let text = user.host.disconnect(&address).await?;
            user.store.forget_account(&address)?;
            Ok(Output::text(text))
        }
        "ai" => Ok(Output::text(user.host.ai_setup().await?)),
        other => Err(OpError::usage(format!(
            "unknown action {other:?}; use list, connect, disconnect or ai"
        ))),
    }
}
