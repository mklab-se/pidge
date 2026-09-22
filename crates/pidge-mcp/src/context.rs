//! Per-call context for tool handlers: who is calling, their user record,
//! and the rules for which of their mailboxes a call may touch.
//!
//! Every tool starts with [`ToolContext::from_request`]. The identity comes
//! from the bearer token the HTTP layer verified, never from tool input, and
//! a mailbox named in tool input is only accepted if the record owns it.

use pidge_client::ClientError;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};

use crate::oauth::bearer::AuthenticatedUser;
use crate::state::SharedState;
use crate::users::{UserRecord, user_hash};

pub struct ToolContext {
    #[allow(dead_code)] // read by the mail and calendar tools (Tasks 10+)
    pub user: AuthenticatedUser,
    pub record: UserRecord,
    #[allow(dead_code)] // read by the mail and calendar tools (Tasks 10+)
    pub tz: chrono_tz::Tz,
}

impl ToolContext {
    /// Build from the request; loads the user record, creating it for the
    /// sign-in mailbox if missing (covers users who signed in before user
    /// records existed and whose only trace is their mailbox secret).
    pub async fn from_request(
        state: &SharedState,
        ctx: &RequestContext<RoleServer>,
    ) -> Result<Self, McpError> {
        let user = ctx
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<AuthenticatedUser>())
            .cloned()
            .ok_or_else(|| McpError::internal_error("request is not authenticated", None))?;

        let record = match state.users.load(&user.email).await {
            Ok(Some(rec)) => rec,
            Ok(None) => {
                let rec = UserRecord::new(&user.email);
                state.users.save(&rec).await.map_err(store_error)?;
                tracing::info!(user = %user_hash(&user.email), "created missing user record");
                rec
            }
            Err(e) => return Err(store_error(e)),
        };
        let tz = record.tz();
        Ok(Self { user, record, tz })
    }

    /// Accounts to operate on: the named one (must be owned) or all.
    pub fn accounts(&self, account: Option<&str>) -> Result<Vec<String>, McpError> {
        match account {
            Some(name) => Ok(vec![self.owned(name)?]),
            None => Ok(self.record.mailboxes.clone()),
        }
    }

    /// The one account a write goes through: named (owned) or the default sender.
    pub fn sender(&self, from_account: Option<&str>) -> Result<String, McpError> {
        match from_account {
            Some(name) => self.owned(name),
            None => Ok(self.record.default_sender.clone()),
        }
    }

    /// Every address the user receives mail at.
    #[allow(dead_code)] // used for item flags by the mail tools (Tasks 10+)
    pub fn my_addresses(&self) -> &[String] {
        &self.record.mailboxes
    }

    /// The record's spelling of `name`, or an error if the user doesn't own it.
    fn owned(&self, name: &str) -> Result<String, McpError> {
        self.record
            .mailboxes
            .iter()
            .find(|m| m.eq_ignore_ascii_case(name.trim()))
            .cloned()
            .ok_or_else(|| {
                tool_error(format!(
                    "{name} is not one of your mailboxes. Call accounts_list to see them, or accounts_connect to add it."
                ))
            })
    }
}

/// A one-line, actionable error for the harness.
pub fn tool_error(msg: impl Into<String>) -> McpError {
    McpError::invalid_params(msg.into(), None)
}

/// Maps a Graph failure to a one-line message; never a Graph payload dump.
#[allow(dead_code)] // used by every Graph-calling tool (Tasks 10+)
pub fn graph_error(e: ClientError) -> McpError {
    let msg = match e {
        ClientError::SessionExpired { email } => {
            format!("Mailbox {email} needs reconnecting: call accounts_connect")
        }
        ClientError::Throttled {
            retry_after: Some(secs),
        } => format!("Microsoft is throttling; retry in {secs}s"),
        ClientError::Throttled { retry_after: None } => {
            "Microsoft is throttling; retry in a minute".to_string()
        }
        other => format!("Microsoft Graph error: {other}"),
    };
    McpError::internal_error(msg, None)
}

fn store_error(e: anyhow::Error) -> McpError {
    tracing::error!(error = %e, "user store");
    McpError::internal_error(
        "pidge could not read your account settings; try again",
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(mailboxes: &[&str]) -> ToolContext {
        let mut record = UserRecord::new(mailboxes[0]);
        record.mailboxes = mailboxes.iter().map(|m| m.to_string()).collect();
        ToolContext {
            user: AuthenticatedUser {
                email: mailboxes[0].to_string(),
            },
            tz: record.tz(),
            record,
        }
    }

    #[test]
    fn accounts_defaults_to_all_and_checks_ownership() {
        let c = ctx_with(&["jane@example.com", "work@example.com"]);
        assert_eq!(
            c.accounts(None).unwrap(),
            vec!["jane@example.com", "work@example.com"]
        );
        assert_eq!(
            c.accounts(Some("Work@Example.com")).unwrap(),
            vec!["work@example.com"]
        );
        let err = c.accounts(Some("mallory@example.com")).unwrap_err();
        assert!(err.message.contains("not one of your mailboxes"), "{err:?}");
    }

    #[test]
    fn sender_defaults_to_default_sender_and_checks_ownership() {
        let mut c = ctx_with(&["jane@example.com", "work@example.com"]);
        c.record.default_sender = "work@example.com".into();
        assert_eq!(c.sender(None).unwrap(), "work@example.com");
        assert_eq!(
            c.sender(Some("JANE@example.com")).unwrap(),
            "jane@example.com"
        );
        assert!(c.sender(Some("mallory@example.com")).is_err());
        assert_eq!(c.my_addresses(), ["jane@example.com", "work@example.com"]);
    }

    #[test]
    fn graph_errors_are_one_actionable_line() {
        let e = graph_error(ClientError::SessionExpired {
            email: "work@example.com".into(),
        });
        assert_eq!(
            e.message,
            "Mailbox work@example.com needs reconnecting: call accounts_connect"
        );
        let e = graph_error(ClientError::Throttled {
            retry_after: Some(30),
        });
        assert_eq!(e.message, "Microsoft is throttling; retry in 30s");
        let e = graph_error(ClientError::Graph {
            status: 404,
            message: "not found".into(),
        });
        assert!(e.message.starts_with("Microsoft Graph error: "), "{e:?}");
    }
}
