//! The two HTML pages a human can see: an error, and (via the client's
//! redirect) nothing else — success never renders here.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

pub fn error(status: StatusCode, message: &str) -> Response {
    let body = format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>pidge</title>
<style>
  body{{font-family:-apple-system,system-ui,sans-serif;background:#111;color:#eee;display:grid;place-items:center;min-height:100vh;margin:0;padding:16px}}
  main{{max-width:28rem}} h1{{font-size:1.25rem;margin:0 0 .5rem}} p{{color:#bbb;line-height:1.5}}
</style></head>
<body><main><h1>pidge could not sign you in</h1><p>{}</p></main></body></html>"#,
        html_escape(message)
    );
    (status, Html(body)).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
