//! `pidge mail unsubscribe` — opt out of a sender using the message's
//! `List-Unsubscribe` / `List-Unsubscribe-Post` headers.
//!
//! Preference order: RFC 8058 one-click POST → RFC 2369 mailto → bail with
//! the HTTPS URL. See `pidge-client::unsubscribe` for the parser.

use anyhow::{Context, Result, anyhow};
use colored::Colorize;
use inquire::Confirm;

use pidge_client::{AuthClient, GraphClient, Outgoing, UnsubscribeMethod, parse_unsubscribe};

use crate::commands::mail_fragment::resolve;

pub async fn run(fragment: String, yes: bool) -> Result<()> {
    let gate = crate::guardrail::gate(
        crate::guardrail::GuardrailAction::Unsubscribe,
        &format!("unsubscribe from sender of message {fragment}"),
    )?;
    if gate == crate::guardrail::Gate::DryRun {
        return Ok(());
    }

    let (short, msg) = resolve(&fragment)?;
    let graph = GraphClient::new(AuthClient::from_env()?)?;

    let headers = graph
        .fetch_message_headers(&msg.account, &msg.graph_id)
        .await
        .context("fetching message headers from Microsoft Graph")?;

    let method = parse_unsubscribe(&headers);

    match method {
        UnsubscribeMethod::None => Err(anyhow!(
            "Message {short} has no `List-Unsubscribe` header — there is no \
             standard way to unsubscribe from this sender. Look for an \
             unsubscribe link in the body or contact the sender directly."
        )),
        UnsubscribeMethod::HttpsOnly(url) => {
            // GET on a bare HTTPS unsubscribe URL usually shows a "click to
            // confirm" page; pidge can't reliably drive that without a
            // browser. Surface the URL so the user can click it.
            println!(
                "{} Message {short}: only an HTTPS unsubscribe URL is offered, \
                 and there is no `List-Unsubscribe-Post: List-Unsubscribe=One-Click` \
                 marker.\n  Open this URL in a browser to finish:\n  {}",
                "⚠".yellow(),
                url.cyan().underline(),
            );
            Ok(())
        }
        UnsubscribeMethod::OneClickPost(url) => {
            if !confirm(
                yes,
                &format!(
                    "Unsubscribe from {} via one-click POST to {url}?",
                    msg.account
                ),
            )? {
                println!("Aborted.");
                return Ok(());
            }
            one_click_post(&url).await?;
            println!(
                "{} Unsubscribed via one-click POST ({})",
                "✔".green(),
                url.dimmed()
            );
            Ok(())
        }
        UnsubscribeMethod::Mailto {
            address,
            subject,
            body,
        } => {
            let subject = subject.unwrap_or_else(|| "unsubscribe".to_string());
            let body = body.unwrap_or_default();
            if !confirm(
                yes,
                &format!(
                    "Send unsubscribe e-mail to <{address}> from {} (subject: \"{subject}\")?",
                    msg.account
                ),
            )? {
                println!("Aborted.");
                return Ok(());
            }
            let outgoing = Outgoing {
                subject,
                body_text: body,
                to: vec![address.clone()],
                cc: vec![],
                bcc: vec![],
            };
            graph
                .send_mail(&msg.account, &outgoing)
                .await
                .context("Graph rejected the unsubscribe e-mail")?;
            println!(
                "{} Sent unsubscribe e-mail to {} (audit copy in Sent Items)",
                "✔".green(),
                address.dimmed()
            );
            Ok(())
        }
    }
}

fn confirm(yes_flag: bool, prompt: &str) -> Result<bool> {
    if yes_flag {
        return Ok(true);
    }
    Confirm::new(prompt)
        .with_default(false)
        .prompt()
        .map_err(|e| anyhow!("prompt cancelled: {e}"))
}

/// POST `List-Unsubscribe=One-Click` to the given URL per RFC 8058. The
/// body is form-urlencoded (the RFC says so explicitly).
///
/// We use a fresh `reqwest::Client` rather than the Graph client's — this
/// request goes to an arbitrary third-party host, not Microsoft, so it
/// must not carry the Graph bearer token.
async fn one_click_post(url: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(format!("pidge/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("building HTTP client")?;
    let resp = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await
        .context("POST failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let trimmed: String = body.chars().take(200).collect();
        return Err(anyhow!(
            "Unsubscribe endpoint returned HTTP {status}. Response (first 200 chars): {trimmed}\n\
             Try the URL in a browser instead: {url}"
        ));
    }
    Ok(())
}
