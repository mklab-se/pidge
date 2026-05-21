//! `pidge calendar move <hash> --to <calendar>` — move between calendars.

use anyhow::{Result, anyhow};

use pidge_client::{AuthClient, GraphClient};
use pidge_core::Config;

use crate::commands::calendar_fragment;

pub async fn run(fragment: &str, to: &str, json: bool) -> Result<()> {
    let (_hash, r) = calendar_fragment::resolve(fragment)?;
    let _config = Config::load()?;
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let cur = graph.get_event(&r.account, &r.event_id).await?;
    if cur.series_master_id.is_some() {
        anyhow::bail!(
            "This is one occurrence of a recurring series. Move the whole series by \
             editing the master event directly (run `pidge calendar show <hash>` to see \
             its ID, then re-run `move` against that)."
        );
    }
    let cals = graph.list_calendars(&r.account).await?;
    let dest = cals
        .iter()
        .find(|c| c.id == to || c.name.eq_ignore_ascii_case(to))
        .ok_or_else(|| {
            anyhow!(
                "Calendar '{to}' not found on {}. Try `pidge calendar calendars`.",
                r.account
            )
        })?;
    graph
        .move_event_to_calendar(&r.account, &r.event_id, &dest.id)
        .await?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "calendar": dest.name })
        );
    } else {
        println!("Moved to calendar \"{}\".", dest.name);
    }
    Ok(())
}
