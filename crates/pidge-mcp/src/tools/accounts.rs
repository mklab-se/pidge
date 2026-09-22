//! `accounts_*` tools: which mailboxes the user has, their health, and the
//! per-user settings (default sender, timezone, trusted senders).

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, schemars, tool, tool_router};
use serde::Deserialize;

use chrono::Utc;
use chrono_tz::Tz;

use super::PidgeMcp;
use crate::context::{ToolContext, store_error, tool_error};
use crate::oauth::jwt::random_id;
use crate::state::{PENDING_TTL, PendingAuthorization, PendingKind};
use crate::users::{UserRecord, log_store_error, user_hash};

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct ConnectArgs {
    /// The address of the mailbox to connect, if known; shown to the user
    /// alongside the link so they pick that account at Microsoft.
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct UpdateArgs {
    /// Make this mailbox (one of the user's own) the default for new mail and events.
    #[serde(default)]
    pub default_sender: Option<String>,
    /// IANA timezone name, e.g. `Europe/Stockholm`.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Disconnect this mailbox and delete its stored session. The sign-in
    /// mailbox cannot be disconnected.
    #[serde(default)]
    pub disconnect: Option<String>,
    /// Add a sender address to the trusted-senders list.
    #[serde(default)]
    pub trust: Option<String>,
    /// Remove a sender address from the trusted-senders list.
    #[serde(default)]
    pub untrust: Option<String>,
}

#[tool_router(router = accounts_router, vis = "pub(crate)")]
impl PidgeMcp {
    #[tool(
        description = "The user's connected mailboxes with their health (ok / needs reconnect), the sign-in address, the default sender for new mail and events, the timezone, and trusted senders."
    )]
    async fn accounts_list(
        &self,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tc = ToolContext::from_request(&self.state, &ctx).await?;
        Ok(ok(self.render_accounts(&tc.record).await))
    }

    #[tool(
        description = "Start connecting another mailbox the user can sign in to at Microsoft. Returns a link, valid 10 minutes, for the user to open in a browser; after they finish, call accounts_list to confirm."
    )]
    async fn accounts_connect(
        &self,
        Parameters(args): Parameters<ConnectArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tc = ToolContext::from_request(&self.state, &ctx).await?;
        let microsoft_state = random_id();
        self.state.insert_pending(
            microsoft_state.clone(),
            PendingAuthorization {
                kind: PendingKind::Connect {
                    owner: tc.record.signin.clone(),
                },
                client_id: String::new(),
                client_redirect_uri: String::new(),
                client_state: None,
                code_challenge: String::new(),
                microsoft_verifier: random_id() + &random_id(),
                consent_nonce: None,
                created_at: Utc::now(),
            },
        );
        tracing::info!(user = %user_hash(&tc.record.signin), "connect link issued");

        let target = args
            .email
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .unwrap_or("the mailbox");
        Ok(ok(format!(
            "Connect {target} by opening: {}/connect?state={microsoft_state}  (valid {} minutes)\n\
             The user signs in at Microsoft with that mailbox's account.\n\
             next: accounts_list once they have finished",
            self.state.config.base_url(),
            PENDING_TTL.num_minutes(),
        )))
    }

    #[tool(
        description = "Change account settings: default_sender, timezone, disconnect a mailbox, or trust/untrust a sender. Returns the updated accounts_list."
    )]
    async fn accounts_update(
        &self,
        Parameters(args): Parameters<UpdateArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tc = ToolContext::from_request(&self.state, &ctx).await?;
        let signin = tc.record.signin.clone();

        // Validate everything before changing anything.
        let default_sender = args
            .default_sender
            .as_deref()
            .map(|m| tc.sender(Some(m)))
            .transpose()?;
        let timezone = args
            .timezone
            .as_deref()
            .map(|tz| {
                tz.trim().parse::<Tz>().map_err(|_| {
                    tool_error(format!(
                        "{tz} is not a timezone; use an IANA name such as Europe/Stockholm"
                    ))
                })
            })
            .transpose()?;
        let disconnect = match args.disconnect.as_deref() {
            Some(m) => {
                let mailbox = tc.accounts(Some(m))?.remove(0);
                if mailbox == signin {
                    return Err(tool_error(format!(
                        "{mailbox} is the sign-in mailbox and cannot be disconnected"
                    )));
                }
                Some(mailbox)
            }
            None => None,
        };
        let trust = sender_address(args.trust.as_deref(), "trust")?;
        let untrust = sender_address(args.untrust.as_deref(), "untrust")?;

        // Merge onto the freshest record: a connect callback may have
        // appended a mailbox since `from_request` loaded it.
        let mut record = self
            .state
            .users
            .load(&signin)
            .await
            .map_err(|e| store_error("loading user record", &signin, e))?
            .unwrap_or_else(|| tc.record.clone());

        if let Some(sender) = default_sender {
            record.default_sender = sender;
        }
        if let Some(tz) = timezone {
            record.timezone = tz.name().to_string();
        }
        if let Some(addr) = trust
            && !record.trusted_senders.contains(&addr)
        {
            record.trusted_senders.push(addr);
        }
        if let Some(addr) = untrust {
            record.trusted_senders.retain(|t| *t != addr);
        }
        if let Some(mailbox) = &disconnect {
            record.mailboxes.retain(|m| m != mailbox);
            if record.default_sender == *mailbox {
                record.default_sender = record.signin.clone();
            }
        }

        // Save the record first: if deleting the session then fails, the
        // mailbox is already gone from the user's view rather than listed
        // with a half-deleted session.
        self.state
            .users
            .save(&record)
            .await
            .map_err(|e| store_error("saving user record", &signin, e))?;
        if let Some(mailbox) = &disconnect {
            self.state
                .users
                .delete_mailbox(mailbox)
                .await
                .map_err(|e| store_error("deleting mailbox session", mailbox, e))?;
            self.state.token_backend.forget(mailbox);
        }
        tracing::info!(user = %user_hash(&record.signin), "account settings updated");

        Ok(ok(format!(
            "Saved.\n\n{}",
            self.render_accounts(&record).await
        )))
    }
}

impl PidgeMcp {
    async fn render_accounts(&self, record: &UserRecord) -> String {
        let mut out = format!(
            "signed in as: {}\ndefault sender: {}\ntimezone: {}\nmailboxes:\n",
            record.signin, record.default_sender, record.timezone
        );
        let mut any_broken = false;
        for mailbox in &record.mailboxes {
            let healthy = self.mailbox_is_healthy(mailbox, &record.signin).await;
            any_broken |= !healthy;
            let role = if *mailbox == record.signin {
                " (sign-in)"
            } else {
                ""
            };
            let health = if healthy { "ok" } else { "needs reconnect" };
            out.push_str(&format!("  - {mailbox}{role}: {health}\n"));
        }
        let trusted = if record.trusted_senders.is_empty() {
            "none".to_string()
        } else {
            record.trusted_senders.join(", ")
        };
        out.push_str(&format!("trusted senders: {trusted}\n"));
        out.push_str(if any_broken {
            "next: accounts_connect email=<mailbox> to reconnect"
        } else {
            "next: accounts_connect to add another mailbox"
        });
        out
    }

    /// `ok` if the mailbox's record is owned by `owner` and its access token
    /// is still fresh, or a refresh succeeds. Makes no Graph call beyond that
    /// refresh, and never touches tokens stamped with another owner.
    async fn mailbox_is_healthy(&self, mailbox: &str, owner: &str) -> bool {
        match self.state.users.load_mailbox(mailbox).await {
            Ok(Some(rec)) if !rec.owner.eq_ignore_ascii_case(owner) => false,
            Ok(Some(rec)) if !rec.tokens.needs_refresh() => true,
            Ok(Some(_)) => self
                .state
                .graph
                .auth()
                .get_valid_token(mailbox)
                .await
                .is_ok(),
            Ok(None) => false,
            Err(e) => {
                log_store_error("loading mailbox record", mailbox, &e);
                false
            }
        }
    }
}

/// A trusted-sender address, lower-cased; must look like an address.
fn sender_address(input: Option<&str>, field: &str) -> Result<Option<String>, McpError> {
    let Some(raw) = input.map(str::trim) else {
        return Ok(None);
    };
    if !raw.contains('@') || raw.contains(char::is_whitespace) {
        return Err(tool_error(format!(
            "{field}: {raw} is not an e-mail address"
        )));
    }
    Ok(Some(raw.to_ascii_lowercase()))
}

fn ok(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tests::{ToolHarness, request_context, text};
    use crate::users::UserStore;

    const JANE: &str = "jane@example.com";
    const WORK: &str = "work@example.com";

    #[tokio::test]
    async fn list_shows_signin_default_sender_timezone_and_health() {
        let h = ToolHarness::new(&[JANE, WORK]).await;
        // WORK's session is gone: no stored tokens at all.
        UserStore::new(h.secrets.clone())
            .delete_mailbox(WORK)
            .await
            .unwrap();
        let out = text(&h.mcp.accounts_list(h.ctx()).await.unwrap());
        assert!(out.contains("signed in as: jane@example.com"), "{out}");
        assert!(out.contains("default sender: jane@example.com"), "{out}");
        assert!(out.contains("timezone: Europe/Stockholm"), "{out}");
        assert!(out.contains("jane@example.com (sign-in): ok"), "{out}");
        assert!(out.contains("work@example.com: needs reconnect"), "{out}");
    }

    #[tokio::test]
    async fn list_reports_a_mailbox_stamped_with_another_owner_as_needing_reconnect() {
        let h = ToolHarness::new(&[JANE, WORK]).await;
        // Fresh tokens, but the mailbox record belongs to someone else.
        UserStore::new(h.secrets.clone())
            .save_mailbox(
                &crate::users::MailboxRecord {
                    owner: "mallory@example.com".into(),
                    tokens: crate::tools::tests::fresh_tokens(),
                },
                WORK,
            )
            .await
            .unwrap();
        let out = text(&h.mcp.accounts_list(h.ctx()).await.unwrap());
        assert!(out.contains("jane@example.com (sign-in): ok"), "{out}");
        assert!(out.contains("work@example.com: needs reconnect"), "{out}");
    }

    #[tokio::test]
    async fn list_creates_a_missing_record_for_spike_era_users() {
        let h = ToolHarness::new(&[JANE]).await;
        let out = text(
            &h.mcp
                .accounts_list(request_context("legacy@example.com"))
                .await
                .unwrap(),
        );
        assert!(out.contains("signed in as: legacy@example.com"), "{out}");
        assert!(
            h.state
                .users
                .load("legacy@example.com")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn update_rejects_unknown_timezone_and_foreign_sender() {
        let h = ToolHarness::new(&[JANE, WORK]).await;
        let err = h
            .mcp
            .accounts_update(
                Parameters(UpdateArgs {
                    timezone: Some("Mars/Olympus".into()),
                    ..Default::default()
                }),
                h.ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("Mars/Olympus"), "{err:?}");

        let err = h
            .mcp
            .accounts_update(
                Parameters(UpdateArgs {
                    default_sender: Some("notmine@x".into()),
                    ..Default::default()
                }),
                h.ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("not one of your mailboxes"), "{err:?}");
        assert_eq!(h.record().await.default_sender, JANE, "nothing saved");
    }

    #[tokio::test]
    async fn update_saves_default_sender_timezone_and_trust() {
        let h = ToolHarness::new(&[JANE, WORK]).await;
        let out = text(
            &h.mcp
                .accounts_update(
                    Parameters(UpdateArgs {
                        default_sender: Some("Work@Example.com".into()),
                        timezone: Some("Europe/London".into()),
                        trust: Some("Anna@Example.com".into()),
                        ..Default::default()
                    }),
                    h.ctx(),
                )
                .await
                .unwrap(),
        );
        assert!(out.contains("default sender: work@example.com"), "{out}");
        let rec = h.record().await;
        assert_eq!(rec.default_sender, WORK);
        assert_eq!(rec.timezone, "Europe/London");
        assert_eq!(rec.trusted_senders, vec!["anna@example.com"]);

        h.mcp
            .accounts_update(
                Parameters(UpdateArgs {
                    untrust: Some("anna@example.com".into()),
                    ..Default::default()
                }),
                h.ctx(),
            )
            .await
            .unwrap();
        assert!(h.record().await.trusted_senders.is_empty());
    }

    #[tokio::test]
    async fn disconnect_removes_mailbox_and_resets_default_sender() {
        let h = ToolHarness::new(&[JANE, WORK]).await;
        // Warm the token cache so we can see it being evicted.
        h.state.graph.auth().get_valid_token(WORK).await.unwrap();
        h.mcp
            .accounts_update(
                Parameters(UpdateArgs {
                    default_sender: Some(WORK.into()),
                    ..Default::default()
                }),
                h.ctx(),
            )
            .await
            .unwrap();
        h.mcp
            .accounts_update(
                Parameters(UpdateArgs {
                    disconnect: Some(WORK.into()),
                    ..Default::default()
                }),
                h.ctx(),
            )
            .await
            .unwrap();
        let rec = h.record().await;
        assert_eq!(rec.mailboxes, vec![JANE]);
        assert_eq!(rec.default_sender, JANE);
        assert!(h.state.users.load_mailbox(WORK).await.unwrap().is_none());
        assert!(
            h.state.graph.auth().get_valid_token(WORK).await.is_err(),
            "cached tokens were evicted"
        );
    }

    #[tokio::test]
    async fn disconnect_refuses_the_signin_mailbox() {
        let h = ToolHarness::new(&[JANE, WORK]).await;
        let err = h
            .mcp
            .accounts_update(
                Parameters(UpdateArgs {
                    disconnect: Some("JANE@example.com".into()),
                    ..Default::default()
                }),
                h.ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("sign-in mailbox"), "{err:?}");
        assert!(h.state.users.load_mailbox(JANE).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn connect_returns_link_and_binds_pending_entry_to_the_caller() {
        let h = ToolHarness::new(&[JANE]).await;
        let out = text(
            &h.mcp
                .accounts_connect(
                    Parameters(ConnectArgs {
                        email: Some("work@example.com".into()),
                    }),
                    h.ctx(),
                )
                .await
                .unwrap(),
        );
        assert!(
            out.contains(
                "Connect work@example.com by opening: http://localhost:8080/connect?state="
            ),
            "{out}"
        );
        assert!(out.contains("(valid 10 minutes)"), "{out}");
        let state = out
            .split("/connect?state=")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let pending = h.state.peek_pending(state).expect("pending entry");
        assert_eq!(pending.kind, PendingKind::Connect { owner: JANE.into() });
        assert!(pending.created_at <= Utc::now());
        assert!(!pending.microsoft_verifier.is_empty());

        let out = text(
            &h.mcp
                .accounts_connect(Parameters(ConnectArgs::default()), h.ctx())
                .await
                .unwrap(),
        );
        assert!(out.contains("Connect the mailbox by opening: "), "{out}");
    }
}
