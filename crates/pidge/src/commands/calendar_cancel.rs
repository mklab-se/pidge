//! `pidge calendar cancel <hash>` — organizer cancellation with notification.

use anyhow::Result;
use std::io::Write;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::Config;

use crate::commands::calendar_fragment;

pub async fn run(fragment: &str, comment: &str, yes: bool, series: bool, json: bool) -> Result<()> {
    let (hash, r) = calendar_fragment::resolve(fragment)?;
    let _config = Config::load()?;
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let mut event_id = r.event_id.clone();
    let cur = graph.get_event(&r.account, &event_id).await?;
    if !cur.is_organizer {
        anyhow::bail!(
            "You're not the organizer of \"{}\". Use `pidge calendar rsvp <hash> --decline` \
             to remove yourself, or `pidge calendar delete <hash>` to drop the event from \
             your calendar without notifying anyone.",
            cur.subject
        );
    }
    if series {
        if let Some(m) = &cur.series_master_id {
            event_id = m.clone();
        }
    }
    if !yes {
        print!(
            "Cancel \"{}\" and notify {} attendee{}? [y/N] ",
            cur.subject,
            cur.attendees.len(),
            if cur.attendees.len() == 1 { "" } else { "s" },
        );
        std::io::stdout().flush()?;
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        if !s.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }
    graph.cancel_event(&r.account, &event_id, comment).await?;
    calendar_fragment::purge_from_cache(&hash)?;
    if json {
        println!("{}", serde_json::json!({ "ok": true }));
    } else {
        println!("Canceled. Notices sent.");
    }
    Ok(())
}
