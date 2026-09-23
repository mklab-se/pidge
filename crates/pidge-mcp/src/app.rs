//! Assembles the HTTP application: OAuth endpoints, the bearer-guarded MCP
//! service, and health. Kept separate from `main` so tests can drive the
//! whole router in-process.

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;

use crate::download;
use crate::oauth;
use crate::state::SharedState;
use crate::tools::PidgeMcp;

pub fn build_router(state: SharedState, cancel: CancellationToken) -> Router {
    let mcp_service = StreamableHttpService::new(
        {
            let state = state.clone();
            move || Ok(PidgeMcp::new(state.clone()))
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts(allowed_hosts(&state))
            .with_cancellation_token(cancel),
    );

    let mcp_routes = Router::new().nest_service("/mcp", mcp_service).layer(
        axum::middleware::from_fn_with_state(state.clone(), oauth::bearer::require_bearer),
    );

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/",
            get(|| async { "pidge-mcp: connect your MCP client to /mcp" }),
        )
        // The signed link is its own credential: outside the bearer layer.
        .route("/dl/{token}", get(download::download))
        .merge(oauth::router())
        .merge(mcp_routes)
        .layer(tower_http::trace::TraceLayer::new_for_http().make_span_with(request_span))
        .with_state(state)
}

/// The default request span, except that a `/dl/` URI is logged as
/// `/dl/<redacted>`: the token in it is a bearer credential.
fn request_span(req: &Request<Body>) -> tracing::Span {
    let uri = if req.uri().path().starts_with("/dl/") {
        "/dl/<redacted>".to_string()
    } else {
        req.uri().to_string()
    };
    tracing::debug_span!(
        "request",
        method = %req.method(),
        uri = %uri,
        version = ?req.version(),
    )
}

/// The public host (with and without its port) plus loopback for local runs.
/// rmcp validates `Host` against this list to block DNS-rebinding.
fn allowed_hosts(state: &SharedState) -> Vec<String> {
    let mut hosts = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    if let Some(host) = state.config.public_url.host_str() {
        hosts.push(host.to_string());
        if let Some(port) = state.config.public_url.port() {
            hosts.push(format!("{host}:{port}"));
        }
    }
    let port = state.config.port;
    hosts.push(format!("localhost:{port}"));
    hosts.push(format!("127.0.0.1:{port}"));
    hosts
}
