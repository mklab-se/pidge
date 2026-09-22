//! Bearer-token gate for the MCP endpoint (RFC 6750 + RFC 9728 challenge).

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::SharedState;

/// The identity attached to every authenticated MCP request. Tool handlers
/// read it from the request extensions and never take a user from input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedUser {
    /// Lower-cased e-mail; also the id of the mailbox the user may access.
    pub email: String,
}

fn challenge(state: &SharedState, error: Option<&str>) -> Response {
    let metadata = format!(
        "{}/.well-known/oauth-protected-resource/mcp",
        state.config.base_url()
    );
    let value = match error {
        Some(e) => format!(r#"Bearer resource_metadata="{metadata}", error="{e}""#),
        None => format!(r#"Bearer resource_metadata="{metadata}""#),
    };
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, value)],
    )
        .into_response()
}

pub async fn require_bearer(
    State(state): State<SharedState>,
    mut req: Request,
    next: Next,
) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty());

    let Some(token) = token else {
        return challenge(&state, None);
    };
    let claims = match state.signer.verify_access(token) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error = %e, "rejected bearer token");
            return challenge(&state, Some("invalid_token"));
        }
    };
    if !state.config.is_allowed(&claims.sub) {
        return challenge(&state, Some("invalid_token"));
    }

    req.extensions_mut()
        .insert(AuthenticatedUser { email: claims.sub });
    next.run(req).await
}
