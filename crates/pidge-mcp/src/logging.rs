//! The tracing subscriber `main` installs. Built here so tests can run the
//! exact production configuration against a captured writer: the flat JSON
//! shape is a contract for the KQL queries in `deploy/azure/README.md`.

use tracing::Subscriber;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::LogFormat;

/// The `RUST_LOG` filter, or `info` when unset or invalid.
fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())
}

/// One flat JSON object per line: `timestamp`, `level`, `target`,
/// `message`, and the event's own fields at the top level (no nested
/// `fields`, no span or span list).
pub fn json_subscriber<W>(filter: EnvFilter, writer: W) -> impl Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .json()
        .flatten_event(true)
        .with_current_span(false)
        .with_span_list(false)
        .finish()
}

/// `tracing-subscriber`'s human-readable default, for local development.
fn text_subscriber(filter: EnvFilter) -> impl Subscriber + Send + Sync {
    tracing_subscriber::fmt().with_env_filter(filter).finish()
}

/// Installs the global subscriber for `format`, writing to stdout.
pub fn init(format: LogFormat) {
    match format {
        LogFormat::Json => json_subscriber(env_filter(), std::io::stdout).init(),
        LogFormat::Text => text_subscriber(env_filter()).init(),
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use rmcp::ServerHandler;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::LogCapture;
    use crate::tools::tests::ToolHarness;

    /// The top-level keys of the one JSON line whose `message` is `event`.
    fn line_for(logged: &str, event: &str) -> serde_json::Map<String, serde_json::Value> {
        let lines: Vec<serde_json::Map<String, serde_json::Value>> = logged
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
            .filter(|l: &serde_json::Map<String, serde_json::Value>| l["message"] == event)
            .collect();
        assert_eq!(lines.len(), 1, "one {event} line expected: {logged}");
        lines.into_iter().next().unwrap()
    }

    fn assert_flat(line: &serde_json::Map<String, serde_json::Value>, fields: &[&str]) {
        for key in ["timestamp", "level", "target", "message"]
            .iter()
            .chain(fields)
        {
            assert!(line.contains_key(*key), "missing top-level {key}: {line:?}");
        }
        for nested in ["fields", "span", "spans"] {
            assert!(!line.contains_key(nested), "unexpected {nested}: {line:?}");
        }
    }

    #[tokio::test]
    async fn production_json_lines_are_flat_with_the_documented_fields() {
        let capture = LogCapture::default();
        let _guard = tracing::subscriber::set_default(json_subscriber(
            EnvFilter::new("info"),
            capture.clone(),
        ));

        let h = ToolHarness::new(&["jane@example.com"]).await;
        let request = rmcp::model::CallToolRequestParams::new("accounts_list");
        h.mcp.call_tool(request, h.ctx()).await.unwrap();
        let status = crate::app::build_router(h.state.clone(), CancellationToken::new())
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::OK);

        let logged = capture.text();
        let tool = line_for(&logged, "tool_call");
        assert_flat(&tool, &["tool", "user", "duration_ms", "outcome"]);
        assert_eq!(tool["tool"], "accounts_list");
        assert_eq!(tool["outcome"], "ok");
        assert_eq!(tool["level"], "INFO");

        let http = line_for(&logged, "http_request");
        assert_flat(&http, &["method", "route", "status", "latency_ms"]);
        assert_eq!(http["method"], "GET");
        assert_eq!(http["route"], "/healthz");
        assert_eq!(http["status"], 200);
    }
}
