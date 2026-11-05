//! Exit-code taxonomy + structured error envelope for agents.
//!
//! Codes: 0 ok · 1 unexpected · 2 usage/bad input · 3 auth expired/missing ·
//! 4 not found / ambiguous · 5 throttled · 6 denied by guardrails.
//! (clap parse errors exit 2 on their own.)

use pidge_client::ClientError;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    #[allow(dead_code)] // constructed implicitly by success path
    Ok = 0,
    Unexpected = 1,
    Usage = 2,
    AuthExpired = 3,
    NotFound = 4,
    Throttled = 5,
    GuardrailDenied = 6,
}

/// Machine-readable error report, printed as one JSON line on stderr when
/// `--json` is active.
#[derive(Debug, Serialize)]
pub struct Envelope {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

/// Map an error to its exit kind and envelope by walking the anyhow chain
/// for the typed errors pidge produces.
pub fn classify(err: &anyhow::Error) -> (ExitKind, Envelope) {
    for cause in err.chain() {
        if let Some(client) = cause.downcast_ref::<ClientError>() {
            match client {
                ClientError::SessionExpired { email } => {
                    return (
                        ExitKind::AuthExpired,
                        Envelope {
                            code: "auth_expired",
                            message: client.to_string(),
                            hint: Some("run `pidge account add` to sign in again".into()),
                            account: Some(email.clone()),
                            retry_after: None,
                        },
                    );
                }
                ClientError::McpSessionExpired { server } => {
                    return (
                        ExitKind::AuthExpired,
                        Envelope {
                            code: "auth_expired",
                            message: client.to_string(),
                            hint: Some(format!(
                                "run `pidge mcp connect {server}` to sign in again"
                            )),
                            account: None,
                            retry_after: None,
                        },
                    );
                }
                ClientError::Throttled { retry_after } => {
                    return (
                        ExitKind::Throttled,
                        Envelope {
                            code: "throttled",
                            message: client.to_string(),
                            hint: Some("wait and retry; reduce request rate".into()),
                            account: None,
                            retry_after: *retry_after,
                        },
                    );
                }
                ClientError::Graph { status: 404, .. } => {
                    return (
                        ExitKind::NotFound,
                        Envelope {
                            code: "not_found",
                            message: client.to_string(),
                            hint: Some(
                                "the item may have been moved or deleted; refresh with a list command"
                                    .into(),
                            ),
                            account: None,
                            retry_after: None,
                        },
                    );
                }
                ClientError::Graph {
                    status: 401 | 403, ..
                } => {
                    return (
                        ExitKind::AuthExpired,
                        Envelope {
                            code: "auth_expired",
                            message: client.to_string(),
                            hint: Some("run `pidge account add` to sign in again".into()),
                            account: None,
                            retry_after: None,
                        },
                    );
                }
                _ => {}
            }
        }
        if let Some(op) = cause.downcast_ref::<pidge_engine::OpError>() {
            let kind = match op.kind.exit_code() {
                2 => ExitKind::Usage,
                3 => ExitKind::AuthExpired,
                4 => ExitKind::NotFound,
                5 => ExitKind::Throttled,
                6 => ExitKind::GuardrailDenied,
                _ => ExitKind::Unexpected,
            };
            return (
                kind,
                Envelope {
                    code: op.kind.as_str(),
                    message: op.message.clone(),
                    hint: op.hint.clone(),
                    account: None,
                    retry_after: None,
                },
            );
        }
        if let Some(mcp_usage) = cause.downcast_ref::<crate::commands::mcp::McpUsageError>() {
            use crate::commands::mcp::McpUsageError;
            let (kind, code, hint) = match mcp_usage {
                McpUsageError::NoServerConnected | McpUsageError::NotConnected { .. } => {
                    (ExitKind::NotFound, "not_found", None)
                }
                McpUsageError::AmbiguousServer { example, .. } => (
                    ExitKind::Usage,
                    "ambiguous",
                    Some(format!("run `{example}`")),
                ),
            };
            return (
                kind,
                Envelope {
                    code,
                    message: mcp_usage.to_string(),
                    hint,
                    account: None,
                    retry_after: None,
                },
            );
        }
        if let Some(cursor) = cause.downcast_ref::<pidge_client::CursorError>() {
            return (
                ExitKind::Usage,
                Envelope {
                    code: "bad_cursor",
                    message: cursor.to_string(),
                    hint: Some("use the next_cursor value from a previous response".into()),
                    account: None,
                    retry_after: None,
                },
            );
        }
    }
    (
        ExitKind::Unexpected,
        Envelope {
            code: "unexpected",
            message: format!("{err:#}"),
            hint: None,
            account: None,
            retry_after: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_expired_maps_to_auth_exit_3() {
        let err = anyhow::Error::from(ClientError::SessionExpired {
            email: "a@b.se".into(),
        });
        let (kind, env) = classify(&err);
        assert_eq!(kind as i32, 3);
        assert_eq!(env.code, "auth_expired");
        assert!(env.hint.unwrap().contains("pidge account add"));
        assert_eq!(env.account.as_deref(), Some("a@b.se"));
    }

    #[test]
    fn mcp_session_expired_maps_to_auth_exit_3_with_a_connect_hint() {
        let err = anyhow::Error::from(ClientError::McpSessionExpired {
            server: "https://mcp.example.com".into(),
        });
        let (kind, env) = classify(&err);
        assert_eq!(kind as i32, 3);
        assert_eq!(env.code, "auth_expired");
        assert!(
            env.hint
                .unwrap()
                .contains("pidge mcp connect https://mcp.example.com")
        );
        assert_eq!(env.account, None);
    }

    #[test]
    fn mcp_usage_errors_map_to_not_found_or_usage() {
        use crate::commands::mcp::McpUsageError;

        let err = anyhow::Error::from(McpUsageError::NoServerConnected);
        assert_eq!(classify(&err).0 as i32, 4);

        let err = anyhow::Error::from(McpUsageError::NotConnected {
            server: "https://mcp.example.com".into(),
        });
        assert_eq!(classify(&err).0 as i32, 4);

        let err = anyhow::Error::from(McpUsageError::AmbiguousServer {
            servers: "https://a.example.com, https://b.example.com".into(),
            example: "https://a.example.com".into(),
        });
        let (kind, env) = classify(&err);
        assert_eq!(kind as i32, 2);
        assert_eq!(env.code, "ambiguous");
    }

    #[test]
    fn throttled_maps_to_5_with_retry_after() {
        let err = anyhow::Error::from(ClientError::Throttled {
            retry_after: Some(30),
        });
        let (kind, env) = classify(&err);
        assert_eq!(kind as i32, 5);
        assert_eq!(env.retry_after, Some(30));
    }

    #[test]
    fn operation_errors_keep_their_kind() {
        let err = anyhow::Error::from(
            pidge_engine::OpError::not_found("no message").with_hint("use a handle"),
        );
        let (kind, env) = classify(&err);
        assert_eq!(kind as i32, 4);
        assert_eq!(env.code, "not_found");
        assert_eq!(env.hint.as_deref(), Some("use a handle"));
    }

    #[test]
    fn graph_404_is_not_found_and_unknown_is_1() {
        let err = anyhow::Error::from(ClientError::Graph {
            status: 404,
            message: "gone".into(),
        });
        assert_eq!(classify(&err).0 as i32, 4);

        let err = anyhow::anyhow!("something odd");
        let (kind, env) = classify(&err);
        assert_eq!(kind as i32, 1);
        assert_eq!(env.code, "unexpected");
    }

    #[test]
    fn wrapped_errors_are_found_through_context() {
        let err = anyhow::Error::from(ClientError::Throttled { retry_after: None })
            .context("while listing inbox");
        assert_eq!(classify(&err).0 as i32, 5);
    }
}
