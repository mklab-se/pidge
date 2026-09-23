//! Assembles the HTTP application: OAuth endpoints, the bearer-guarded MCP
//! service, and health. Kept separate from `main` so tests can drive the
//! whole router in-process.

use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::MatchedPath;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;
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
        .route(DOWNLOAD_ROUTE, get(download::download))
        .merge(oauth::router())
        .merge(mcp_routes)
        .layer(tower_http::trace::TraceLayer::new_for_http().make_span_with(request_span))
        .layer(axum::middleware::from_fn(log_http_request))
        .with_state(state)
}

/// The download route's template. Its `{token}` is a bearer credential, so
/// it is logged as [`DOWNLOAD_ROUTE_LOGGED`], never with the real path.
const DOWNLOAD_ROUTE: &str = "/dl/{token}";
const DOWNLOAD_ROUTE_LOGGED: &str = "/dl/<redacted>";

/// Method and route for a request, shared by the debug-level
/// [`request_span`] and the structured `http_request` log line. The route
/// is axum's matched route template ([`MatchedPath`]), never the request
/// path: no query string (on `/callback` it carries Microsoft's
/// authorization code, on `/authorize` the client's state), no path
/// segment a caller chose, and no download token. The download route is
/// `/dl/<redacted>`; a request that matched no route is `<unmatched>`.
fn method_and_route(req: &Request<Body>) -> (String, String) {
    let route = match req
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
    {
        Some(DOWNLOAD_ROUTE) => DOWNLOAD_ROUTE_LOGGED.to_string(),
        Some(template) => template.to_string(),
        None => "<unmatched>".to_string(),
    };
    (req.method().to_string(), route)
}

/// The request's span: method and route template only.
fn request_span(req: &Request<Body>) -> tracing::Span {
    let (method, uri) = method_and_route(req);
    tracing::debug_span!(
        "request",
        method = %method,
        uri = %uri,
        version = ?req.version(),
    )
}

/// One `info`-level `http_request` event per request: method, route
/// template, status and latency. No query string and no headers.
async fn log_http_request(req: Request<Body>, next: Next) -> Response {
    let (method, route) = method_and_route(&req);
    let start = Instant::now();
    let response = next.run(req).await;
    let latency_ms = start.elapsed().as_millis() as u64;
    tracing::info!(
        method = %method,
        route = %route,
        status = response.status().as_u16(),
        latency_ms,
        "http_request"
    );
    response
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
    hosts.extend(state.config.alt_hosts.iter().cloned());
    hosts
}
