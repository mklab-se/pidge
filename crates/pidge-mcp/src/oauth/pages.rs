//! The HTML pages a human can see: an error, the consent pages before a
//! sign-in or a mailbox connect goes to Microsoft, and the confirmation
//! after connecting an extra mailbox. A sign-in's success never renders
//! here: it redirects back to the MCP client.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

pub fn error(status: StatusCode, message: &str) -> Response {
    page(status, "pidge could not sign you in", message)
}

/// A refusal on the `/ai` key page.
pub fn ai_error(status: StatusCode, message: &str) -> Response {
    page(status, "pidge AI setup", message)
}

/// The one answer `/dl/…` gives for every refusal, so a link's holder
/// learns nothing about why it stopped working.
pub fn link_unavailable() -> Response {
    page(
        StatusCode::NOT_FOUND,
        "Download unavailable",
        "This download link is invalid or has expired. Ask your assistant for a new one.",
    )
}

/// A 200 page confirming something finished, for flows with no client to
/// redirect back to.
pub fn done(message: &str) -> Response {
    page(StatusCode::OK, "pidge", message)
}

/// The interstitial before a connect link goes to Microsoft: names the
/// mailbox being connected (when the link was issued for one) and the pidge
/// account it will join, with a Continue link to `go_url`.
pub fn confirm_connect(owner: &str, mailbox: Option<&str>, go_url: &str) -> Response {
    let message = match mailbox {
        Some(mailbox) => format!(
            "You are about to connect the mailbox {mailbox} to the pidge account {owner}. \
             At Microsoft, sign in as {mailbox}; signing in with any other account connects nothing. \
             Continue only if {owner} is your pidge account."
        ),
        None => format!(
            "You are about to connect a mailbox to the pidge account {owner}. \
             At Microsoft, sign in with the mailbox you want to connect. \
             Continue only if {owner} is your pidge account."
        ),
    };
    let action = format!(
        r#"<p><a class="go" href="{}">Continue to Microsoft sign-in</a></p>"#,
        html_escape(go_url)
    );
    render(StatusCode::OK, "Connect a mailbox", &message, &action)
}

/// The interstitial before a client's sign-in goes to Microsoft: names the
/// client (as it registered itself) and the host the sign-in will be sent
/// to, with a Continue link to `go_url`.
pub fn confirm_sign_in(client_name: Option<&str>, redirect_host: &str, go_url: &str) -> Response {
    let name: String = client_name
        .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|n| !n.is_empty())
        .map(|n| n.chars().take(80).collect())
        .unwrap_or_else(|| "an unnamed client".to_string());
    let message = format!(
        "Sign in to pidge for {name} at {redirect_host}? Continue only if you started this from that app."
    );
    let action = format!(
        r#"<p><a class="go" href="{}">Continue to Microsoft sign-in</a></p>"#,
        html_escape(go_url)
    );
    render(StatusCode::OK, "Sign in to pidge", &message, &action)
}

/// The form for a user's own AI key, bound to `state` and `token` (the
/// cookie nonce, repeated as a field). `error` is shown above the form when
/// a previous attempt was refused.
pub fn ai_form(owner: &str, state: &str, token: &str, error: Option<&str>) -> Response {
    let message = format!(
        "Set the AI key pidge uses to sort mail for the pidge account {owner}. \
         The key is stored only for this account and never shown to your assistant. \
         Continue only if {owner} is your pidge account."
    );
    let error = error
        .map(|e| format!(r#"<p class="err">{}</p>"#, html_escape(e)))
        .unwrap_or_default();
    let options: String = crate::review::ai::AiProvider::ALL
        .iter()
        .map(|p| {
            let value = serde_json::to_value(p)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            format!(
                r#"<option value="{}">{} (default model {})</option>"#,
                html_escape(&value),
                html_escape(p.label()),
                html_escape(p.default_model())
            )
        })
        .collect();
    let form = format!(
        r#"{error}<form method="post" action="/ai">
<input type="hidden" name="state" value="{}"><input type="hidden" name="token" value="{}">
<label>Provider<select name="provider">{options}</select></label>
<label>Model (optional)<input name="model" autocomplete="off" placeholder="leave empty for the default"></label>
<label>API key<input name="key" type="password" autocomplete="off" required></label>
<p><button class="go" type="submit">Test and save</button></p>
</form>"#,
        html_escape(state),
        html_escape(token)
    );
    render(StatusCode::OK, "Set up AI sorting", &message, &form)
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
  a.go,button.go{{display:inline-block;background:#eee;color:#111;padding:.5rem 1rem;border-radius:6px;text-decoration:none;border:0;font:inherit;cursor:pointer}}
  label{{display:block;margin:.75rem 0;color:#bbb}} input,select{{display:block;width:100%;box-sizing:border-box;margin-top:.25rem;padding:.4rem;border-radius:6px;border:1px solid #444;background:#1c1c1c;color:#eee;font:inherit}}
  p.err{{color:#ff8a65}}
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
