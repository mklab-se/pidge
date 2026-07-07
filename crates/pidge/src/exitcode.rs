//! Exit-code taxonomy + structured error envelope for agents.
//!
//! Codes: 0 ok · 1 unexpected · 2 usage/bad input · 3 auth expired/missing ·
//! 4 not found / ambiguous fragment · 5 throttled · 6 denied by guardrails.
//! (clap parse errors exit 2 on their own.)

use pidge_client::ClientError;
use pidge_core::FragmentError;
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
        if let Some(fragment) = cause.downcast_ref::<FragmentError>() {
            let code = match fragment {
                FragmentError::NotFound { .. } => "not_found",
                FragmentError::Ambiguous { .. } => "ambiguous",
            };
            return (
                ExitKind::NotFound,
                Envelope {
                    code,
                    message: fragment.to_string(),
                    hint: Some(match fragment {
                        FragmentError::NotFound { .. } => {
                            "refresh the cache with `pidge mail` or `pidge calendar`".into()
                        }
                        FragmentError::Ambiguous { .. } => {
                            "provide more characters of the id".into()
                        }
                    }),
                    account: None,
                    retry_after: None,
                },
            );
        }
        if let Some(guardrail) = cause.downcast_ref::<crate::guardrail::GuardrailError>() {
            return (
                ExitKind::GuardrailDenied,
                Envelope {
                    code: match guardrail {
                        crate::guardrail::GuardrailError::Denied { .. } => "guardrail_denied",
                        crate::guardrail::GuardrailError::ConfirmRequired { .. } => {
                            "guardrail_confirm_required"
                        }
                    },
                    message: guardrail.to_string(),
                    hint: Some(match guardrail {
                        crate::guardrail::GuardrailError::Denied { .. } => {
                            "this action class is denied by the user's guardrails config".into()
                        }
                        crate::guardrail::GuardrailError::ConfirmRequired { .. } => {
                            "ask the user to run this interactively or relax guardrails".into()
                        }
                    }),
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
    fn throttled_maps_to_5_with_retry_after() {
        let err = anyhow::Error::from(ClientError::Throttled {
            retry_after: Some(30),
        });
        let (kind, env) = classify(&err);
        assert_eq!(kind as i32, 5);
        assert_eq!(env.retry_after, Some(30));
    }

    #[test]
    fn fragment_errors_map_to_4() {
        let err = anyhow::Error::from(FragmentError::Ambiguous {
            fragment: "35".into(),
            count: 3,
        });
        let (kind, env) = classify(&err);
        assert_eq!(kind as i32, 4);
        assert_eq!(env.code, "ambiguous");

        let err = anyhow::Error::from(FragmentError::NotFound {
            fragment: "dead".into(),
        });
        let (kind, env) = classify(&err);
        assert_eq!(kind as i32, 4);
        assert_eq!(env.code, "not_found");
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
