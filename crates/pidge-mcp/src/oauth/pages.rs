//! The HTML pages a human can see: an error, and the confirmation after
//! connecting an extra mailbox. A sign-in's success never renders here: it
//! redirects back to the MCP client.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

pub fn error(status: StatusCode, message: &str) -> Response {
    page(status, "pidge could not sign you in", message)
}

/// A 200 page confirming something finished, for flows with no client to
/// redirect back to.
pub fn done(message: &str) -> Response {
    page(StatusCode::OK, "pidge", message)
}

/// The interstitial before a connect link goes to Microsoft: names the pidge
/// account the mailbox will join, with a Continue link to `go_url`.
pub fn confirm_connect(owner: &str, go_url: &str) -> Response {
    let message = format!(
        "You are about to connect a mailbox to the pidge account {owner}. Continue only if that is your account."
    );
    let action = format!(
        r#"<p><a class="go" href="{}">Continue to Microsoft sign-in</a></p>"#,
        html_escape(go_url)
    );
    render(StatusCode::OK, "Connect a mailbox", &message, &action)
}

fn page(status: StatusCode, heading: &str, message: &str) -> Response {
    render(status, heading, message, "")
}

/// `extra_html` is inserted verbatim after the message; callers escape it.
fn render(status: StatusCode, heading: &str, message: &str, extra_html: &str) -> Response {
    let body = format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>pidge</title>
<style>
  body{{font-family:-apple-system,system-ui,sans-serif;background:#111;color:#eee;display:grid;place-items:center;min-height:100vh;margin:0;padding:16px}}
  main{{max-width:28rem}} h1{{font-size:1.25rem;margin:0 0 .5rem}} p{{color:#bbb;line-height:1.5}}
  a.go{{display:inline-block;background:#eee;color:#111;padding:.5rem 1rem;border-radius:6px;text-decoration:none}}
</style></head>
<body><main><h1>{}</h1><p>{}</p>{}</main></body></html>"#,
        html_escape(heading),
        html_escape(message),
        extra_html
    );
    (status, Html(body)).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
