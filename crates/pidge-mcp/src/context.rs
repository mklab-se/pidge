//! Per-call context for tool handlers: who is calling, their user record,
//! and the rules for which of their mailboxes a call may touch.
//!
//! Every tool starts with [`ToolContext::from_request`]. The identity comes
//! from the bearer token the HTTP layer verified, never from tool input, and
//! a mailbox named in tool input is only accepted if the record owns it.

use std::future::Future;

use pidge_client::ClientError;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer};

use crate::oauth::bearer::AuthenticatedUser;
use crate::state::SharedState;
use crate::users::{UserRecord, log_store_error, user_hash};

pub struct ToolContext {
    pub user: AuthenticatedUser,
    pub record: UserRecord,
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
                state
                    .users
                    .save(&rec)
                    .await
                    .map_err(|e| store_error("creating user record", &user.email, e))?;
                tracing::info!(user = %user_hash(&user.email), "created missing user record");
                rec
            }
            Err(e) => return Err(store_error("loading user record", &user.email, e)),
        };
        let record = usable_mailboxes(state, record).await?;
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

/// `record` with only the mailboxes the user may use: a mailbox whose stored
/// record names someone else as its owner is left out, so no tool (and no
/// code path reading `record.mailboxes` directly) reaches its tokens. A
/// mailbox with nothing stored stays: Graph then reports it as needing
/// reconnecting. A default sender that was left out falls back to the
/// sign-in mailbox. If the store can't say who owns a mailbox, the call
/// fails rather than guessing. A tool that saves this record drops the
/// left-out mailboxes from it for good, which is right: they are no longer
/// the user's.
async fn usable_mailboxes(
    state: &SharedState,
    mut record: UserRecord,
) -> Result<UserRecord, McpError> {
    let mut usable = Vec::with_capacity(record.mailboxes.len());
    for mailbox in std::mem::take(&mut record.mailboxes) {
        match state.token_backend.owner_of(&mailbox).await {
            Ok(Some(owner)) if !owner.eq_ignore_ascii_case(&record.signin) => {
                tracing::warn!(
                    user = %user_hash(&record.signin),
                    mailbox = %user_hash(&mailbox),
                    "mailbox left out: its stored record names another owner"
                );
            }
            Ok(_) => usable.push(mailbox),
            Err(e) => return Err(store_error("checking mailbox owner", &mailbox, e)),
        }
    }
    record.mailboxes = usable;
    if !record.owns(&record.default_sender) {
        record.default_sender = record.signin.clone();
    }
    Ok(record)
}

/// Runs a read through the per-user cache: returns a live hit, otherwise
/// calls `f` and, only on success, stores the result under `key` before
/// returning it. `key` is opaque to this function; callers build it with
/// [`crate::cache::ReadCache::key`].
pub async fn cached<F, Fut>(
    state: &SharedState,
    user: &str,
    key: String,
    f: F,
) -> Result<String, McpError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, McpError>>,
{
    if let Some(hit) = state.cache.get(user, &key) {
        return Ok(hit);
    }
    let result = f().await;
    if let Ok(value) = &result {
        state.cache.put(user, key, value.clone());
    }
    result
}

/// A one-line, actionable error for the harness.
pub fn tool_error(msg: impl Into<String>) -> McpError {
    McpError::invalid_params(msg.into(), None)
}

/// Maps a Graph failure to a one-line message; never a Graph payload dump.
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
        ClientError::Graph { status, .. } => {
            format!(
                "Microsoft Graph error: {} ({status})",
                graph_status_phrase(status)
            )
        }
        _ => "Microsoft Graph error: the request failed".to_string(),
    };
    McpError::internal_error(msg, None)
}

/// A fixed phrase for a Graph status. Graph's own message is never used: it
/// can echo addresses, message content or other third-party text.
fn graph_status_phrase(status: u16) -> &'static str {
    match status {
        400 => "Microsoft rejected the request",
        401 | 403 => "Microsoft denied access",
        404 => "not found",
        409 => "conflict",
        429 => "throttled; retry later",
        500..=599 => "Microsoft service error",
        _ => "the request failed",
    }
}

/// A secret-store failure during a tool call: logged redacted (see
/// [`log_store_error`]) and reported to the harness as a fixed message.
pub fn store_error(context: &str, account: &str, e: anyhow::Error) -> McpError {
    log_store_error(context, account, &e);
    McpError::internal_error(
        "pidge could not access your account settings; try again",
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

    #[tokio::test]
    async fn store_failures_are_reported_and_logged_without_the_address() {
        use std::sync::Arc;

        use crate::test_support::{FailingSecrets, LogCapture, assert_no_address};
        use crate::tools::tests::{request_context, test_state};

        let (logs, _guard) = LogCapture::start();
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(
            Arc::new(FailingSecrets),
            "http://127.0.0.1:9",
            "jane@example.com",
            dir.path(),
        );
        let err =
            match ToolContext::from_request(&state, &request_context("jane@example.com")).await {
                Err(e) => e,
                Ok(_) => panic!("store failure must fail the call"),
            };
        assert_no_address("tool error", &err.message);
        let logged = logs.text();
        assert!(
            logged.contains("loading user record: secret store failure"),
            "{logged}"
        );
        assert_no_address("logs", &logged);
    }

    #[tokio::test]
    async fn a_mailbox_whose_record_names_another_owner_is_not_used() {
        use crate::tools::tests::{ToolHarness, fresh_tokens};
        use crate::users::{MailboxRecord, UserStore};

        const JANE: &str = "jane@example.com";
        const WORK: &str = "work@example.com";
        let h = ToolHarness::new(&[JANE, WORK]).await;
        let mut rec = h.record().await;
        rec.default_sender = WORK.into();
        h.state.users.save(&rec).await.unwrap();
        // Jane's record lists WORK, but WORK's own record says Mallory's.
        UserStore::new(h.secrets.clone())
            .save_mailbox(
                &MailboxRecord {
                    owner: "mallory@example.com".into(),
                    tokens: fresh_tokens(),
                    identity: None,
                },
                WORK,
            )
            .await
            .unwrap();
        let tc = ToolContext::from_request(&h.state, &h.ctx()).await.unwrap();
        assert_eq!(tc.accounts(None).unwrap(), vec![JANE]);
        let err = tc.accounts(Some(WORK)).unwrap_err();
        assert!(err.message.contains("not one of your mailboxes"), "{err:?}");
        assert!(tc.sender(Some(WORK)).is_err());
        assert_eq!(tc.sender(None).unwrap(), JANE, "default sender falls back");
        assert_eq!(tc.my_addresses(), [JANE]);
        // The stored record is untouched.
        assert_eq!(h.record().await.mailboxes, vec![JANE, WORK]);
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
        assert_eq!(e.message, "Microsoft Graph error: not found (404)");
    }

    #[test]
    fn graph_errors_never_carry_graphs_message() {
        let e = graph_error(ClientError::Graph {
            status: 400,
            message: "secret detail about jane@example.com".into(),
        });
        assert_eq!(
            e.message,
            "Microsoft Graph error: Microsoft rejected the request (400)"
        );
        for (status, phrase) in [
            (401, "Microsoft denied access"),
            (403, "Microsoft denied access"),
            (409, "conflict"),
            (429, "throttled; retry later"),
            (503, "Microsoft service error"),
        ] {
            let e = graph_error(ClientError::Graph {
                status,
                message: "secret detail".into(),
            });
            assert!(!e.message.contains("secret detail"), "{e:?}");
            assert!(e.message.contains(phrase), "{e:?}");
            assert!(e.message.contains(&status.to_string()), "{e:?}");
        }
    }

    #[tokio::test]
    async fn cached_runs_f_once_on_hit_and_never_caches_an_err() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use crate::tools::tests::ToolHarness;

        let h = ToolHarness::new(&["jane@example.com"]).await;
        let calls = Arc::new(AtomicUsize::new(0));

        // Miss: runs f, caches the Ok value.
        let c = calls.clone();
        let out = cached(&h.state, "jane@example.com", "k".into(), || async move {
            c.fetch_add(1, Ordering::SeqCst);
            Ok("v".to_string())
        })
        .await
        .unwrap();
        assert_eq!(out, "v");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Hit: does not run f again, even though it would return something else.
        let c = calls.clone();
        let out = cached(&h.state, "jane@example.com", "k".into(), || async move {
            c.fetch_add(1, Ordering::SeqCst);
            Ok("ignored".to_string())
        })
        .await
        .unwrap();
        assert_eq!(out, "v");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // An Err from f is never cached: the next call for the same key runs f again.
        let c = calls.clone();
        let err = cached(
            &h.state,
            "jane@example.com",
            "other".into(),
            || async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(tool_error("boom"))
            },
        )
        .await
        .unwrap_err();
        assert!(err.message.contains("boom"), "{err:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(h.state.cache.get("jane@example.com", "other").is_none());
    }
}

/// For optional scalar tool inputs: the value itself, or the same value as
/// a string (`"true"`, `"3"`). A client holding a stale tool schema sends
/// parameters it doesn't know about as strings; refusing them would make
/// every new parameter unusable there until the client refreshes.
pub fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + std::str::FromStr,
{
    use serde::Deserialize;
    use serde::de::Error;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either<T> {
        Value(T),
        Text(String),
    }
    match Option::<Either<T>>::deserialize(d)? {
        None => Ok(None),
        Some(Either::Value(v)) => Ok(Some(v)),
        Some(Either::Text(t)) => t
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| D::Error::custom(format!("{t:?} is not a valid value here"))),
    }
}

#[cfg(test)]
mod lenient_tests {
    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Args {
        #[serde(default, deserialize_with = "super::lenient")]
        flag: Option<bool>,
        #[serde(default, deserialize_with = "super::lenient")]
        days: Option<u32>,
    }

    #[test]
    fn scalars_may_arrive_as_strings() {
        let parse = |j: &str| serde_json::from_str::<Args>(j);
        assert_eq!(
            parse(r#"{"flag": true, "days": 3}"#).unwrap(),
            Args {
                flag: Some(true),
                days: Some(3)
            }
        );
        assert_eq!(
            parse(r#"{"flag": "true", "days": "3"}"#).unwrap(),
            Args {
                flag: Some(true),
                days: Some(3)
            }
        );
        assert_eq!(
            parse(r#"{}"#).unwrap(),
            Args {
                flag: None,
                days: None
            }
        );
        assert_eq!(
            parse(r#"{"flag": null}"#).unwrap(),
            Args {
                flag: None,
                days: None
            }
        );
        assert!(parse(r#"{"flag": "maybe"}"#).is_err());
    }
}
