//! `pidge calendar rsvp <hash> --accept|--tentative|--decline`.

use anyhow::Result;

use pidge_client::{AuthClient, GraphClient, graph::events::RsvpKind};
use pidge_core::Config;

use crate::commands::calendar_fragment;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    fragment: &str,
    accept: bool,
    tentative: bool,
    decline: bool,
    comment: &str,
    send_response: bool,
    json: bool,
) -> Result<()> {
    let kind = match (accept, tentative, decline) {
        (true, false, false) => RsvpKind::Accept,
        (false, true, false) => RsvpKind::Tentative,
        (false, false, true) => RsvpKind::Decline,
        _ => anyhow::bail!("Exactly one of --accept, --tentative, --decline is required."),
    };
    let (_hash, r) = calendar_fragment::resolve(fragment)?;
    let _config = Config::load()?;
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    graph
        .rsvp_event(&r.account, &r.event_id, kind, comment, send_response)
        .await?;
    if json {
        println!("{}", serde_json::json!({ "ok": true }));
    } else {
        let verb = match kind {
            RsvpKind::Accept => "Accepted",
            RsvpKind::Tentative => "Tentatively accepted",
            RsvpKind::Decline => "Declined",
        };
        println!("{verb}.");
    }
    Ok(())
}
