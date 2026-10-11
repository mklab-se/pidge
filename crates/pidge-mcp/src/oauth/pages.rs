//! The HTML pages a human can see: an error, the consent pages before a
//! sign-in or a mailbox connect goes to Microsoft, and the confirmation
//! after connecting an extra mailbox. A sign-in's success never renders
//! here: it redirects back to the MCP client.

use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

use super::art::{self, Labels};

pub fn error(status: StatusCode, message: &str) -> Response {
    trouble(
        status,
        "pidge could not sign you in",
        "Sign-in stopped",
        message,
    )
}

/// A refusal page with the puzzled mascot; `panel` is its short caption.
fn trouble(status: StatusCode, heading: &str, panel: &str, message: &str) -> Response {
    let picture = art::ERROR.html(
        &Labels {
            card: None,
            caption: "pidge",
            value: panel,
        },
        "The pidge mascot, puzzled",
    );
    render(status, heading, message, "", &picture)
}

/// A refusal on the `/ai` key page.
pub fn ai_error(status: StatusCode, message: &str) -> Response {
    trouble(status, "pidge AI setup", "Setup stopped", message)
}

/// The one answer `/dl/…` gives for every refusal, so a link's holder
/// learns nothing about why it stopped working.
pub fn link_unavailable() -> Response {
    trouble(
        StatusCode::NOT_FOUND,
        "Download unavailable",
        "Link expired",
        "This download link is invalid or has expired. Ask your assistant for a new one.",
    )
}

/// A 200 page confirming something finished for the pidge account
/// `owner`, for flows with no client to redirect back to.
pub fn done(owner: &str, message: &str) -> Response {
    let picture = art::DONE.html(
        &Labels {
            card: None,
            caption: "Pidge account",
            value: owner,
        },
        "The pidge mascot giving a thumbs up",
    );
    render(StatusCode::OK, "All set", message, "", &picture)
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
    let picture = art::CONNECT.html(
        &Labels {
            card: Some(mailbox.unwrap_or("your mailbox")),
            caption: "Pidge account",
            value: owner,
        },
        "A mailbox joins a pidge account",
    );
    render(
        StatusCode::OK,
        "Connect a mailbox",
        &message,
        &action,
        &picture,
    )
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
    let short: String = name.chars().take(24).collect();
    let picture = art::SIGN_IN.html(
        &Labels {
            card: Some(&short),
            caption: "pidge",
            value: "your mail and calendar",
        },
        "An AI app connects to pidge",
    );
    render(
        StatusCode::OK,
        "Sign in to pidge",
        &message,
        &action,
        &picture,
    )
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
    let picture = art::AI_KEY.html(
        &Labels {
            card: Some("AI key"),
            caption: "Pidge account",
            value: owner,
        },
        "An AI key goes to a pidge account",
    );
    render(
        StatusCode::OK,
        "Set up AI sorting",
        &message,
        &form,
        &picture,
    )
}

/// `picture` (from [`art`]) goes above the heading and `extra_html` after
/// the message, both verbatim; their builders escape what they insert.
fn render(
    status: StatusCode,
    heading: &str,
    message: &str,
    extra_html: &str,
    picture: &str,
) -> Response {
    let body = format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>pidge</title>
<style>
  body{{font-family:-apple-system,system-ui,sans-serif;background:#111;color:#eee;display:grid;place-items:center;min-height:100vh;margin:0;padding:16px}}
  main{{width:100%;max-width:40rem}} h1{{font-size:1.25rem;margin:0 0 .5rem}} p{{color:#bbb;line-height:1.5}}
  a.go,button.go{{display:inline-block;background:#eee;color:#111;padding:.5rem 1rem;border-radius:6px;text-decoration:none;border:0;font:inherit;cursor:pointer}}
  label{{display:block;margin:.75rem 0;color:#bbb}} input,select{{display:block;width:100%;box-sizing:border-box;margin-top:.25rem;padding:.4rem;border-radius:6px;border:1px solid #444;background:#1c1c1c;color:#eee;font:inherit}}
  p.err{{color:#ff8a65}}
  {}
</style></head>
<body><main>{}<h1>{}</h1><p>{}</p>{}</main></body></html>"#,
        art::CSS,
        picture,
        html_escape(heading),
        html_escape(message),
        extra_html
    );
    (status, SECURITY_HEADERS, Html(body)).into_response()
}

/// The policy every page is served with. No scripts, fonts or frames load
/// at all (`default-src 'none'`), only the inline stylesheet and this
/// server's own pictures (`img-src 'self'`);
/// forms post only back here; no other site may frame the page. Plain links
/// (Continue to Microsoft) are navigation, which CSP leaves alone.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; form-action 'self'; frame-ancestors 'none'";

/// Sent with every page: the CSP above, framing refused for older
/// browsers too, nothing cached (pages carry one-time links and nonces),
/// and no `Referer` (their URLs carry flow state) on the way out.
const SECURITY_HEADERS: [(header::HeaderName, &str); 4] = [
    (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
    (header::X_FRAME_OPTIONS, "DENY"),
    (header::CACHE_CONTROL, "no-store"),
    (header::REFERRER_POLICY, "no-referrer"),
];

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_is_sent_with_the_security_headers() {
        for resp in [
            error(StatusCode::BAD_REQUEST, "no"),
            ai_error(StatusCode::BAD_REQUEST, "no"),
            link_unavailable(),
            done("jane@example.com", "ok"),
            confirm_connect("jane@example.com", None, "https://x.test/connect/go"),
            confirm_sign_in(Some("App"), "claude.ai", "https://x.test/authorize/go"),
            ai_form("jane@example.com", "s", "t", None),
        ] {
            let h = resp.headers();
            assert_eq!(h[header::CONTENT_SECURITY_POLICY], CONTENT_SECURITY_POLICY);
            assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
            assert_eq!(h[header::CACHE_CONTROL], "no-store");
            assert_eq!(h[header::REFERRER_POLICY], "no-referrer");
            assert!(
                h[header::CONTENT_TYPE]
                    .to_str()
                    .unwrap()
                    .starts_with("text/html")
            );
        }
    }
}

/// Writes every page to `$PIDGE_DUMP_PAGES` for a visual check:
/// `PIDGE_DUMP_PAGES=/tmp/p cargo test -p pidge-mcp dump_pages -- --ignored`.
#[cfg(test)]
#[tokio::test]
#[ignore]
async fn dump_pages() {
    let Ok(dir) = std::env::var("PIDGE_DUMP_PAGES") else {
        return;
    };
    let pages = [
        (
            "connect",
            confirm_connect(
                "kristofer.liljeblad@live.com",
                Some("kristofer@mklab.se"),
                "#",
            ),
        ),
        (
            "connect-any",
            confirm_connect("jane@example.com", None, "#"),
        ),
        ("sign-in", confirm_sign_in(Some("Claude"), "claude.ai", "#")),
        (
            "ai",
            ai_form("kristofer.liljeblad@live.com", "s", "t", None),
        ),
        (
            "done",
            done(
                "kristofer.liljeblad@live.com",
                "kristofer@mklab.se is connected to the pidge account kristofer.liljeblad@live.com. You can close this tab.",
            ),
        ),
        (
            "error",
            error(
                StatusCode::BAD_REQUEST,
                "This connect link has expired or is not valid. Ask your AI client for a new one.",
            ),
        ),
        (
            "long",
            confirm_connect(
                "a.really.long.signin.address@subdomain.example.com",
                Some("another.quite.long.mailbox.name@example.co.uk"),
                "#",
            ),
        ),
    ];
    for (name, resp) in pages {
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        std::fs::write(format!("{dir}/{name}.html"), bytes).unwrap();
    }
}
